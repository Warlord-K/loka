use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub enum StreamEvent {
    SessionStarted {
        session_id: String,
    },
    Spawned {
        pid: u32,
    },
    ToolUse {
        tool: String,
        input_summary: String,
    },
    ToolResult {
        tool: String,
        output: String,
    },
    AssistantText(String),
    Error(String),
    Usage(Usage),
}

pub type EventSender = mpsc::UnboundedSender<StreamEvent>;

pub struct Steer {
    pub text: String,
    pub interrupt: bool,
    pub ack: tokio::sync::oneshot::Sender<bool>,
}

pub type SteerSender = mpsc::UnboundedSender<Steer>;
pub type SteerReceiver = mpsc::UnboundedReceiver<Steer>;

#[derive(Default)]
pub struct SteerSlot(std::sync::Mutex<Option<SteerReceiver>>);

impl SteerSlot {
    pub fn new(receiver: SteerReceiver) -> Self {
        Self(std::sync::Mutex::new(Some(receiver)))
    }

    pub fn take(&self) -> Option<SteerReceiver> {
        self.0.lock().unwrap().take()
    }

    pub fn is_set(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

pub fn steerable(backend: &str) -> bool {
    matches!(backend, "claude" | "codex" | "omp" | "cursor" | "opencode")
}

fn steers_natively(backend: &str) -> bool {
    matches!(backend, "claude" | "codex" | "omp")
}

pub async fn deliver(sender: &SteerSender, text: String, interrupt: bool) -> bool {
    let (ack, accepted) = tokio::sync::oneshot::channel();
    if sender.send(Steer { text, interrupt, ack }).is_err() {
        return false;
    }
    matches!(tokio::time::timeout(Duration::from_secs(30), accepted).await, Ok(Ok(true)))
}

pub(crate) fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub(crate) const LOKA_APP_PRIVATE: &[&str] = &[
    "Library/Application Support/dev.ygpt.loka/local.json",
    "Library/Application Support/dev.ygpt.loka/postgres",
    "Library/Application Support/dev.ygpt.loka/pids",
    "Library/Logs/Loka",
];
pub(crate) const MCP_AUTH_DIR: &str = "mcp-auth";

pub(crate) fn runner_private_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("WORLD_RUNNER_ROOT")
        .filter(|root| !root.is_empty())
        .map(|root| private_paths_in(Path::new(&root)))
        .unwrap_or_default();
    if paths.is_empty() {
        return paths;
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        paths.extend(LOKA_APP_PRIVATE.iter().map(|rel| Path::new(&home).join(rel)));
    }
    paths
}

fn private_paths_in(root: &Path) -> Vec<PathBuf> {
    ["leases", "runner.log", "identity", "browser", MCP_AUTH_DIR].iter().map(|name| root.join(name)).collect()
}

tokio::task_local! {
    static JOB_UID: JobUid;
}

static FREE_JOB_UIDS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

static CLI_STATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) const JOB_UMASK: libc::mode_t = 0o007;

struct LeasedUid {
    uid: u32,
    gid: u32,
}

impl Drop for LeasedUid {
    fn drop(&mut self) {
        FREE_JOB_UIDS.lock().unwrap().push(self.uid);
    }
}

#[derive(Clone)]
pub(crate) struct JobUid(std::sync::Arc<LeasedUid>);

impl JobUid {
    pub(crate) fn fill(uids: std::ops::Range<u32>) {
        FREE_JOB_UIDS.lock().unwrap().extend(uids);
    }

    pub(crate) fn free() -> usize {
        if job_user().is_none() {
            usize::MAX
        } else {
            FREE_JOB_UIDS.lock().unwrap().len()
        }
    }

    pub(crate) async fn take() -> std::io::Result<Option<Self>> {
        let Some(uid) = FREE_JOB_UIDS.lock().unwrap().pop() else {
            return Ok(None);
        };
        let base = base_gid();
        let gid = base.unwrap_or_default();
        let job = Self(std::sync::Arc::new(LeasedUid { uid, gid }));
        std::process::Command::new("pkill").args(["-KILL", "-U", &uid.to_string()]).status()?;
        let Some(base) = base else {
            return Ok(Some(job));
        };
        reset_slot_dir(uid, gid)?;
        let _state = CLI_STATE.lock().await;
        share_cli_state(uid, base);
        Ok(Some(job))
    }

    pub(crate) fn current() -> Option<Self> {
        JOB_UID.try_with(Clone::clone).ok()
    }

    pub(crate) fn id(&self) -> u32 {
        self.0.uid
    }

    pub(crate) fn gid(&self) -> u32 {
        self.0.gid
    }

    pub(crate) async fn scope<F: std::future::Future>(uid: Option<Self>, future: F) -> F::Output {
        match uid {
            Some(uid) => JOB_UID.scope(uid, future).await,
            None => future.await,
        }
    }
}

fn base_gid() -> Option<u32> {
    let id = |name: &str| std::env::var(name).ok()?.parse().ok();
    id("WORLD_RUNNER_JOB_UID").and(id("WORLD_RUNNER_JOB_GID"))
}

pub(crate) fn job_user() -> Option<(u32, u32)> {
    let id = |name: &str| std::env::var(name).ok()?.parse().ok();
    let base = id("WORLD_RUNNER_JOB_UID")?;
    let gid = base_gid()?;
    Some(JOB_UID.try_with(|job| (job.id(), job.gid())).unwrap_or((base, gid)))
}

pub(crate) fn job_uids() -> Option<std::ops::Range<u32>> {
    let id = |name: &str| std::env::var(name).ok()?.parse::<u32>().ok();
    let base = id("WORLD_RUNNER_JOB_UID")?;
    Some(base..base + id("WORLD_RUNNER_JOB_UIDS")?)
}

pub(crate) fn is_job_uid(uid: u32) -> bool {
    job_uids().is_some_and(|uids| uids.contains(&uid))
}

pub(crate) fn temp_dir() -> PathBuf {
    job_temp_dir().unwrap_or_else(std::env::temp_dir)
}

pub(crate) const QUEST_TEMP_PREFIX: &str = "loka-q-";

pub(crate) fn quest_id(working_dir: &Path) -> Option<uuid::Uuid> {
    let parts: Vec<&std::ffi::OsStr> = working_dir.components().map(|part| part.as_os_str()).collect();
    let at = parts.windows(2).position(|pair| pair[0] == "tasks" && pair[1].to_str().is_some_and(|id| id.parse::<uuid::Uuid>().is_ok()))?;
    parts[at + 1].to_str()?.parse().ok()
}

pub(crate) fn quest_temp_name(id: uuid::Uuid) -> String {
    format!("{QUEST_TEMP_PREFIX}{}", &id.simple().to_string()[..12])
}

pub(crate) fn quest_temp_dir(working_dir: &Path) -> Option<PathBuf> {
    if job_user().is_some() {
        return None;
    }
    Some(std::env::temp_dir().join(quest_temp_name(quest_id(working_dir)?)))
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn job_temp_dir() -> Option<PathBuf> {
    slot_dir(JOB_UID.try_with(JobUid::id).ok()?)
}

fn slot_dir(uid: u32) -> Option<PathBuf> {
    let root = std::env::var_os("WORLD_RUNNER_ROOT").filter(|root| !root.is_empty())?;
    Some(Path::new(&root).join("tmp").join(uid.to_string()))
}

fn reset_slot_dir(uid: u32, gid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = slot_dir(uid) {
        use std::os::unix::fs::DirBuilderExt;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        return std::os::unix::fs::chown(&dir, Some(uid), Some(gid));
    }
    Ok(())
}

const SHARED_CLI_HOMES: [(&str, usize); 4] = [("CODEX_HOME", 0), ("CLAUDE_CONFIG_DIR", 0), ("CURSOR_CONFIG_DIR", 4), ("CURSOR_DATA_DIR", 2)];

pub(crate) fn cursor_homes_set() -> bool {
    ["CURSOR_CONFIG_DIR", "CURSOR_DATA_DIR"].iter().all(|name| std::env::var_os(name).is_some_and(|dir| !dir.is_empty()))
}

fn share_cli_state(uid: u32, gid: u32) {
    for (home, depth) in SHARED_CLI_HOMES {
        if let Some(dir) = std::env::var_os(home).filter(|dir| !dir.is_empty()) {
            hand_over(Path::new(&dir), depth, uid, gid);
        }
    }
}

fn hand_over(dir: &Path, depth: usize, uid: u32, gid: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
            else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            let owner = metadata.uid();
            let (handed, mode) = match metadata.is_dir() {
                true => (depth > 0 && is_job_uid(owner), 0o2770),
                false => (metadata.is_file() && (is_job_uid(owner) || owner == runner_uid()), 0o660),
            };
            if handed {
                if let Err(error) = hand_file_to(&file, &path, metadata.mode(), mode, uid, gid) {
                    warn!(%error, "Could not fully hand CLI state to the next job");
                }
            }
            if metadata.is_dir() && depth > 0 {
                hand_over(&path, depth - 1, uid, gid);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (dir, depth, uid, gid);
}

#[cfg(unix)]
fn hand_file_to(file: &std::fs::File, path: &Path, current: u32, mode: u32, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let failed = |op: &'static str| {
        move |error: std::io::Error| std::io::Error::new(error.kind(), format!("{op} {}: {error}", path.display()))
    };
    let chmod = match current & 0o7777 == mode {
        true => Ok(()),
        false => file.set_permissions(std::fs::Permissions::from_mode(mode)).map_err(failed("fchmod")),
    };
    std::os::unix::fs::fchown(file, Some(uid), Some(gid)).map_err(failed("fchown"))?;
    chmod
}


pub(crate) async fn own_cli_state() -> tokio::sync::MutexGuard<'static, ()> {
    let state = CLI_STATE.lock().await;
    if let (Some((uid, _)), Some(base)) = (job_user(), base_gid()) {
        share_cli_state(uid, base);
    }
    state
}

pub(crate) fn job_std_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let Some((uid, gid)) = job_user() else {
        return std::process::Command::new(program);
    };
    let mut command = std::process::Command::new("setpriv");
    command
        .arg(format!("--reuid={uid}"))
        .arg(format!("--regid={gid}"))
        .args(["--clear-groups", "--inh-caps=-all", "--ambient-caps=-all", "--"])
        .arg(program);
    set_git_config(&mut command, JOB_GIT_CONFIG);
    if let Some(dir) = job_temp_dir() {
        command.env("TMPDIR", &dir).env("HOME", dir);
    }
    #[cfg(unix)]
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            libc::umask(JOB_UMASK);
            Ok(())
        });
    }
    command
}

pub(crate) const TRUSTED_GIT_ARGS: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.ext.allow=never",
];

pub(crate) fn job_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let is_git = Path::new(program.as_ref()).file_name().is_some_and(|name| name == "git");
    let mut command = job_std_command(program);
    if is_git {
        command.args(TRUSTED_GIT_ARGS);
    }
    command.into()
}

const JOB_GIT_CONFIG: [(&str, &str); 2] =
    [("safe.directory", "*"), ("credential.https://github.com.helper", "!gh auth git-credential")];

const WORKSPACE_GIT_CONFIG: [(&str, &str); 8] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("core.alternateRefsCommand", ""),
    ("protocol.ext.allow", "never"),
    ("submodule.recurse", "false"),
    ("fetch.recurseSubmodules", "false"),
    ("push.recurseSubmodules", "no"),
    ("credential.helper", ""),
];

const GIT_PROGRAMS: [(&str, &str); 6] = [
    ("core.sshcommand", "ssh"),
    ("core.askpass", ""),
    ("gpg.program", "gpg"),
    ("gpg.ssh.program", "ssh-keygen"),
    ("gpg.x509.program", "gpgsm"),
    ("gpg.ssh.defaultkeycommand", ""),
];

const REPO_GIT_COMMANDS: &str = r"^(filter\..+\.(clean|smudge|process)|merge\..+\.driver|core\.(sshcommand|askpass)|gpg\.(openpgp\.|ssh\.|x509\.)?program|gpg\.ssh\.defaultkeycommand)$";

fn set_git_config<'a>(command: &mut std::process::Command, config: impl IntoIterator<Item = (&'a str, &'a str)>) {
    let mut count = 0;
    for (key, value) in config {
        command.env(format!("GIT_CONFIG_KEY_{count}"), key).env(format!("GIT_CONFIG_VALUE_{count}"), value);
        count += 1;
    }
    command.env("GIT_CONFIG_COUNT", count.to_string());
}

fn workspace_std_command(program: &str, dir: &Path, repo_config: &[(String, String)]) -> std::process::Command {
    let mut command = job_std_command(program);
    command.current_dir(dir);
    if job_user().is_some() {
        command
            .env_clear()
            .env("HOME", "/nonexistent")
            .env("TMPDIR", temp_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        for name in ["PATH", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
    command.env("GIT_ALLOW_PROTOCOL", "https:ssh");
    if dir.join(".git").exists() {
        command.env("GIT_WORK_TREE", dir);
    }
    let config = WORKSPACE_GIT_CONFIG
        .into_iter()
        .chain(JOB_GIT_CONFIG)
        .chain(repo_config.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    set_git_config(&mut command, config);
    command
}

fn neutralised_git_programs(listed: &str) -> Vec<(String, String)> {
    let mut config: Vec<(String, String)> =
        GIT_PROGRAMS.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    let mut parts = listed.split('\0');
    while let (Some(scope), Some(entry)) = (parts.next(), parts.next()) {
        let (name, value) = entry.split_once('\n').unwrap_or((entry, ""));
        let name = if name == "gpg.openpgp.program" { "gpg.program" } else { name };
        let repo = matches!(scope, "local" | "worktree");
        match config.iter_mut().find(|(key, _)| key == name) {
            Some(program) if !repo => program.1 = value.to_string(),
            None if repo => config.push((name.to_string(), String::new())),
            _ => {}
        }
    }
    config
}

pub(crate) async fn workspace_command(program: &str, dir: &Path) -> tokio::process::Command {
    let listed = tokio::process::Command::from(workspace_std_command("git", dir, &[]))
        .args(["config", "-z", "--show-scope", "--get-regexp", REPO_GIT_COMMANDS])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    workspace_std_command(program, dir, &neutralised_git_programs(&listed)).into()
}

pub(crate) fn give_to_job_user(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some((uid, gid)) = job_user() {
        return std::os::unix::fs::lchown(path, Some(uid), Some(gid));
    }
    Ok(())
}

pub(crate) fn share_with_jobs(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some((_, gid)) = job_user() {
        use std::os::unix::fs::PermissionsExt;
        std::os::unix::fs::lchown(path, Some(runner_uid()), Some(gid))?;
        return std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

const SOCKET_PATH_MAX: usize = 100;

pub(crate) fn socket_path(wanted: &Path) -> std::io::Result<PathBuf> {
    let text = wanted.to_string_lossy();
    if text.len() <= SOCKET_PATH_MAX && !text.chars().any(char::is_whitespace) {
        return Ok(wanted.to_path_buf());
    }
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = std::env::temp_dir().join(format!("loka-{:012x}", hash & 0xffff_ffff_ffff));
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.uid() != runner_uid() {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} is not a folder this runner owns", dir.display())));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o711))?;
    }
    let name = wanted.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "socket".into());
    let short = dir.join(name);
    if short.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("no short enough place for the socket {}", wanted.display())));
    }
    Ok(short)
}

#[cfg(unix)]
pub(crate) fn runner_uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn steer_interrupt_after() -> Duration {
    Duration::from_secs(env_secs("WORLD_STEER_INTERRUPT_SECS", 180))
}

pub fn silent_after() -> Option<Duration> {
    match env_secs("WORLD_SILENT_SECS", 600) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

pub fn tool_hang_after() -> Option<Duration> {
    match env_secs("WORLD_TOOL_HANG_SECS", 900) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

pub const INTERRUPTED_NOTE: &str = "[The step you were running was stopped so you could read this. Do not run it again the same way; add a timeout or run it in the background and check on it.]";

pub fn hang_notice(stuck: Duration) -> String {
    format!(
        "[runtime] A tool call ran for {} minutes without finishing, so it was stopped. Do not run it again the same way: add a timeout, or run it in the background and poll it. Then carry on with the task.",
        stuck.as_secs() / 60
    )
}

pub struct OpenTools(std::collections::HashMap<String, std::time::Instant>);

impl OpenTools {
    pub fn new() -> Self {
        Self(Default::default())
    }

    pub fn start(&mut self, id: &str) {
        self.0.entry(id.to_string()).or_insert_with(std::time::Instant::now);
    }

    pub fn finish(&mut self, id: &str) {
        self.0.remove(id);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn oldest(&self) -> Option<Duration> {
        self.0.values().map(|t| t.elapsed()).max()
    }

    pub fn stuck_longer_than(&self, limit: Duration) -> bool {
        self.oldest().is_some_and(|age| age >= limit)
    }
}

pub async fn kill_process_trees(roots: Vec<u32>) {
    if roots.is_empty() {
        return;
    }
    let Some(parents) = process_parents().await else {
        return;
    };
    let mut children = std::collections::BTreeMap::<u32, Vec<u32>>::new();
    for (pid, parent) in parents {
        children.entry(parent).or_default().push(pid);
    }
    let mut descendants = roots.clone();
    let mut pending = roots;
    while let Some(parent) = pending.pop() {
        for child in children.remove(&parent).unwrap_or_default() {
            descendants.push(child);
            pending.push(child);
        }
    }
    if descendants.is_empty() {
        return;
    }
    let pids = descendants.iter().map(u32::to_string).collect::<Vec<_>>();
    let mut term = tokio::process::Command::new("kill");
    term.arg("-TERM")
        .args(&pids)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = term.status().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut kill = tokio::process::Command::new("kill");
    kill.arg("-KILL")
        .args(&pids)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = kill.status().await;
}

#[cfg(target_os = "macos")]
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
    };
    (read == size).then_some(info.pbi_ppid)
}

#[cfg(target_os = "linux")]
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn process_start(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
    };
    (read == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process_start(_pid: u32) -> Option<u64> {
    None
}

pub(crate) async fn process_parents() -> Option<BTreeMap<u32, u32>> {
    let output = tokio::process::Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .await
        .ok()?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect(),
    )
}

fn elapsed_secs(etime: &str) -> Option<u64> {
    let (days, rest) = match etime.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let mut secs = 0;
    for part in rest.split(':') {
        secs = secs * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

pub async fn kill_recent_descendants(root: u32, within: Duration, skip: &[&str]) {
    let Ok(output) = tokio::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,etime=,command="])
        .output()
        .await
    else {
        return;
    };
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(parent), Some(etime)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(pid), Ok(parent), Some(age)) = (pid.parse::<u32>(), parent.parse::<u32>(), elapsed_secs(etime)) else {
            continue;
        };
        rows.push((pid, parent, age, fields.collect::<Vec<_>>().join(" ")));
    }
    let mut tree = vec![root];
    let mut index = 0;
    while index < tree.len() {
        let parent = tree[index];
        tree.extend(rows.iter().filter(|row| row.1 == parent).map(|row| row.0));
        index += 1;
    }
    let recent: Vec<u32> = rows
        .iter()
        .filter(|row| row.0 != root && tree.contains(&row.0))
        .filter(|row| row.2 <= within.as_secs())
        .filter(|row| !skip.iter().any(|pattern| row.3.contains(pattern)))
        .map(|row| row.0)
        .collect();
    let tops: Vec<u32> = recent
        .iter()
        .copied()
        .filter(|pid| rows.iter().find(|row| row.0 == *pid).is_none_or(|row| !recent.contains(&row.1)))
        .collect();
    if !tops.is_empty() {
        info!(?tops, "Stopping interrupted commands");
        kill_process_trees(tops).await;
    }
}

pub fn close_steers(receiver: &mut SteerReceiver) {
    receiver.close();
    while let Ok(steer) = receiver.try_recv() {
        let _ = steer.ack.send(false);
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Timing {
    pub warm: bool,
    pub spawned: bool,
    pub spawn_ms: u64,
    pub init_ms: Option<u64>,
    pub first_output_ms: Option<u64>,
    pub last_output_ms: Option<u64>,
    pub result_ms: Option<u64>,
    pub api_ms: Option<u64>,
    pub model_turns: Option<u64>,
    pub tool_calls: u32,
}

const STDERR_TAIL: usize = 4000;

tokio::task_local! {
    static SPAWN_SINK: crate::mcp_hub::Pids;
}

const RUN_FILE_PREFIXES: [&str; 8] = [
    "loka-claude-mcp-",
    "loka-cursor-plugin-",
    "loka-opencode-",
    "loka-omp-",
    "karna-claude-mcp-",
    "karna-cursor-plugin-",
    "karna-opencode-",
    "karna-omp-",
];

pub fn sweep_stale_run_files(older_than: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !RUN_FILE_PREFIXES.iter().any(|prefix| name.to_string_lossy().starts_with(prefix)) {
            continue;
        }
        let Ok(metadata) = entry.path().symlink_metadata() else {
            continue;
        };
        if metadata.modified().ok().and_then(|at| at.elapsed().ok()).is_none_or(|age| age < older_than) {
            continue;
        }
        let gone = if metadata.is_dir() { std::fs::remove_dir_all(entry.path()) } else { std::fs::remove_file(entry.path()) };
        if gone.is_ok() {
            removed += 1;
        }
    }
    removed
}

pub async fn record_spawns<F: std::future::Future>(pids: crate::mcp_hub::Pids, future: F) -> F::Output {
    SPAWN_SINK.scope(pids, future).await
}

pub fn note_spawn(event_tx: Option<&EventSender>, pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    let _ = SPAWN_SINK.try_with(|pids| pids.lock().unwrap().push((pid, process_start(pid))));
    if let Some(tx) = event_tx {
        let _ = tx.send(StreamEvent::Spawned { pid });
    }
}

pub fn announce_spawn(event_tx: Option<&EventSender>, child: &tokio::process::Child) {
    note_spawn(event_tx, child.id());
}

pub fn stderr_tail(pipe: Option<tokio::process::ChildStderr>) -> std::sync::Arc<std::sync::Mutex<String>> {
    use tokio::io::AsyncReadExt;
    let sink = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut pipe) = pipe {
        let tail = sink.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok(n) = pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut text = tail.lock().unwrap();
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if text.len() > STDERR_TAIL {
                    let from = text.len() - STDERR_TAIL;
                    let cut = (from..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
                    text.drain(..cut);
                }
            }
        });
    }
    sink
}

pub fn session_id_from_init<'a>(backend: &str, event: &'a serde_json::Value) -> Option<&'a str> {
    match backend {
        "codex" if event.get("type").and_then(|v| v.as_str()) == Some("thread.started") => {
            event.get("thread_id").and_then(|v| v.as_str())
        }
        "claude" | "cursor"
            if event.get("type").and_then(|v| v.as_str()) == Some("system")
                && event.get("subtype").and_then(|v| v.as_str()) == Some("init") =>
        {
            event.get("session_id").and_then(|v| v.as_str())
        }
        _ => None,
    }
}

pub struct CliResult {
    pub output: String,
    pub session_id: Option<String>,
    pub cost_usd: f64,
    pub exit_code: i32,
    pub usage: Usage,
    pub checkpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub context_tokens: u64,
}

impl Usage {
    pub fn sum<'a>(calls: impl IntoIterator<Item = &'a Usage>) -> Usage {
        calls.into_iter().fold(Usage::default(), |total, call| Usage {
            input_tokens: total.input_tokens + call.input_tokens,
            cached_input_tokens: total.cached_input_tokens + call.cached_input_tokens,
            output_tokens: total.output_tokens + call.output_tokens,
            context_tokens: call.context_tokens.max(total.context_tokens),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    Auto,
    #[default]
    Full,
    ReadOnly,
}

impl Approval {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("full") => Self::Full,
            Some("read_only") => Self::ReadOnly,
            _ => Self::Auto,
        }
    }
}

pub async fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let output = workspace_command("git", dir)
        .await
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    (!path.starts_with(dir)).then_some(path)
}

pub struct CliOptions<'a> {
    pub working_dir: &'a Path,
    pub prompt: &'a str,
    pub system_prompt: Option<&'a str>,
    pub allowed_tools: Option<&'a str>,
    pub max_turns: u32,
    pub model: &'a str,
    pub mcp_config_json: Option<String>,
    pub session_id: Option<&'a str>,
    pub resume: bool,
    pub event_tx: Option<EventSender>,
    pub image_paths: Vec<PathBuf>,
    pub fork: bool,
    pub lean: bool,
    pub effort: Option<&'a str>,
    pub fast: bool,
    pub environment: BTreeMap<String, String>,
    pub mcp_has_secrets: bool,
    pub sensitive: bool,
    pub rewind_to: Option<&'a str>,
    pub approval: Approval,
    pub steer: SteerSlot,
    pub omp_models: Option<String>,
    pub provider_keys: BTreeMap<String, String>,
    pub readable_files: Vec<PathBuf>,
}

impl CliOptions<'_> {
    pub fn sign_commits(&self, command: &mut tokio::process::Command) {
        if !self.lean {
            crate::git::workspace::sign_with_loka_key(command, self.working_dir);
        }
    }

    pub fn child_environment(&self) -> BTreeMap<String, String> {
        let mut environment = self.provider_keys.clone();
        environment.extend(self.environment.clone());
        environment
    }

    pub fn secret_values(&self) -> Vec<String> {
        let mut values: Vec<String> = self.child_environment().into_values().filter(|value| value.len() >= 4).collect();
        if let Some(Ok(mcp)) = self.mcp_config_json.as_deref().map(serde_json::from_str) {
            crate::remote_runner::collect_json_strings(&mcp, &mut values);
        }
        values.extend(host_secrets().iter().cloned());
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        values
    }
}

const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TZ",
    "TMPDIR",
    "SSH_AUTH_SOCK",
    "DISPLAY",
    "EDITOR",
    "PAGER",
    "MANPATH",
    "INFOPATH",
    "CLAUDE_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_OPTIONAL_LOCKS",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_SSH_COMMAND",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "BUN_INSTALL",
    "NVM_DIR",
    "VOLTA_HOME",
    "PNPM_HOME",
    "PYENV_ROOT",
    "ANDROID_HOME",
    "DEVELOPER_DIR",
    "SDKROOT",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "CLOUD_ML_REGION",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_PROFILE",
    "GOOGLE_CLOUD_PROJECT",
    "OPENAI_BASE_URL",
    "MCP_TIMEOUT",
    "MCP_TOOL_TIMEOUT",
    "MAX_MCP_OUTPUT_TOKENS",
    "BASH_DEFAULT_TIMEOUT_MS",
    "BASH_MAX_TIMEOUT_MS",
    "BASH_MAX_OUTPUT_LENGTH",
    "RUST_LOG",
    "RUST_BACKTRACE",
];

const INHERITED_PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "HOMEBREW_",
    "CLAUDE_CODE_",
    "DISABLE_",
    "CODEX_",
    "OMP_",
    "PI_",
    "OPENCODE_",
    "CURSOR_",
];

pub(crate) const CREDENTIAL_FILE_ENV: &[&str] = &["AWS_CONFIG_FILE", "AWS_SHARED_CREDENTIALS_FILE", "GOOGLE_APPLICATION_CREDENTIALS"];

const SERVER_ONLY_ENV: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_HEADERS",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

const HOST_SESSION_ENV: &[&str] = &[
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SIMPLE",
    "CLAUDE_CODE_SAFE_MODE",
    "CLAUDE_CODE_REMOTE",
    "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_CODE_ACCOUNT_UUID",
    "CLAUDE_CODE_ORGANIZATION_UUID",
    "CLAUDE_CODE_USER_EMAIL",
    "CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL",
    "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING",
    "CLAUDE_CODE_EMIT_TOOL_USE_SUMMARIES",
    "CLAUDE_CODE_STARTUP_FAILURE_RESULTS",
    "CLAUDE_CODE_TERMINAL_MCP_TOOLS",
];

const HOST_SESSION_PREFIXES: &[&str] = &[
    "CLAUDE_CODE_SESSION",
    "CLAUDE_CODE_HOST_",
    "CLAUDE_CODE_SDK_",
    "CLAUDE_CODE_MESSAGING_",
    "CLAUDE_CODE_DESKTOP_",
];

fn host_session_var(name: &str) -> bool {
    HOST_SESSION_ENV.contains(&name) || HOST_SESSION_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

pub fn shell_environment_patterns(granted: &BTreeMap<String, String>) -> (Vec<String>, Vec<String>) {
    let include = INHERITED_ENV
        .iter()
        .map(|name| name.to_string())
        .chain(INHERITED_PREFIXES.iter().map(|prefix| format!("{prefix}*")))
        .chain(["GIT_CONFIG_*".to_string()])
        .chain(granted.keys().cloned())
        .collect();
    let exclude = loka_shared::providers::env_names()
        .chain(SERVER_ONLY_ENV.iter().copied())
        .filter(|name| !granted.contains_key(*name))
        .map(String::from)
        .chain(HOST_SESSION_ENV.iter().map(|name| name.to_string()))
        .chain(HOST_SESSION_PREFIXES.iter().map(|prefix| format!("{prefix}*")))
        .chain(loka_shared::loka_env::RESERVED_PREFIXES[1..].iter().map(|prefix| format!("{prefix}*")))
        .collect();
    (include, exclude)
}

fn inherits(name: &str, provider_names: &[&str], share_server_keys: bool) -> bool {
    if provider_names.contains(&name) || SERVER_ONLY_ENV.contains(&name) {
        return share_server_keys;
    }
    if host_session_var(name) {
        return false;
    }
    INHERITED_ENV.contains(&name) || INHERITED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn runtime_name(name: &str) -> bool {
    [crate::browser::LAUNCHER_ENV, crate::browser::ROOT_ENV, crate::screen::LAUNCHER_ENV, crate::apps::LAUNCHER_ENV, crate::apps::LEASE_ENV, loka_shared::command_rules::ENV, loka_shared::approvals::ENV].contains(&name)
}

pub fn configure_child_environment(
    cmd: &mut tokio::process::Command,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    let explicit: Vec<(std::ffi::OsString, std::ffi::OsString)> = cmd
        .as_std()
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
        .collect();
    let provider_names: Vec<&str> = loka_shared::providers::env_names().collect();
    let share_server_keys = loka_shared::providers::share_server_keys();
    cmd.env_clear();
    for (name, value) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| inherits(name, &provider_names, share_server_keys)) {
            cmd.env(name, value);
        }
    }
    cmd.envs(explicit);
    if let Some(dir) = cmd.as_std().get_current_dir().and_then(quest_temp_dir) {
        ensure_private_dir(&dir)?;
        cmd.env("TMPDIR", dir);
    }
    for (name, value) in environment {
        anyhow::ensure!(
            loka_shared::environment::valid_name(name) || runtime_name(name),
            "invalid or reserved environment key {name}"
        );
        cmd.env(name, value);
    }
    Ok(())
}

static HOST_SECRETS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub fn capture_host_secrets() {
    host_secrets();
}

pub fn host_secrets() -> &'static [String] {
    HOST_SECRETS.get_or_init(|| {
        let mut values: Vec<String> = std::env::vars()
            .filter(|(name, value)| value.len() >= 8 && secret_like(name, value))
            .map(|(_, value)| value)
            .collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        values
    })
}

fn secret_like(name: &str, value: &str) -> bool {
    const WORDS: &[&str] = &[
        "KEY", "APIKEY", "SECRET", "TOKEN", "PASSWORD", "PASSWD", "PASS", "CREDENTIAL", "CREDENTIALS", "DSN",
    ];
    let named = name.to_ascii_uppercase().split('_').any(|word| WORDS.contains(&word));
    let url_with_login = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .is_some_and(|authority| authority.contains('@'));
    named || url_with_login
}

pub fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub fn summarize_tool_input(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "Read" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(shorten_path)
            .unwrap_or_default(),
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = input.get("path").and_then(|v| v.as_str()).map(shorten_path);
            match path {
                Some(p) => format!("\"{pattern}\" in {p}"),
                None => format!("\"{pattern}\""),
            }
        }
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            loka_shared::text::clip_with_ellipsis(cmd, 120)
        }
        "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(shorten_path)
            .unwrap_or_default(),
        "Agent" => input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("subagent")
            .to_string(),
        _ => String::new(),
    }
}

pub const TOOL_OUTPUT_MAX_CHARS: usize = 3000;

pub async fn collect_stderr(mut handle: JoinHandle<String>) -> String {
    match tokio::time::timeout(Duration::from_secs(1), &mut handle).await {
        Ok(Ok(stderr)) => stderr,
        Ok(Err(error)) => {
            warn!(%error, "Stderr reader failed");
            String::new()
        }
        Err(_) => {
            warn!("Stderr pipe remained open after CLI exit");
            handle.abort();
            let _ = handle.await;
            String::new()
        }
    }
}

pub fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

pub fn summarize_tool_output(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(summarize_tool_output)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                return text.trim().to_string();
            }
            if let Some(content) = map.get("content") {
                let nested = summarize_tool_output(content);
                if !nested.is_empty() {
                    return nested;
                }
            }
            if let Some(output) = map.get("output") {
                let nested = summarize_tool_output(output);
                if !nested.is_empty() {
                    return nested;
                }
            }
            serde_json::to_string(map).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod environment_tests {
    use super::{configure_child_environment, inherits, secret_like, shell_environment_patterns};
    use std::collections::BTreeMap;

    fn child_env(command: &tokio::process::Command) -> BTreeMap<String, Option<String>> {
        command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().to_string(),
                    value.map(|value| value.to_string_lossy().to_string()),
                )
            })
            .collect()
    }

    #[test]
    fn quest_runs_get_their_own_temp_dir() {
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let tree = |id: uuid::Uuid| std::path::PathBuf::from(format!("/Users/a b/Library/Application Support/dev.ygpt.loka/runner/workspaces/tasks/{id}/world"));
        let temp_of = |dir: &std::path::Path| {
            let mut command = tokio::process::Command::new("true");
            command.current_dir(dir);
            configure_child_environment(&mut command, &BTreeMap::new()).unwrap();
            child_env(&command).get("TMPDIR").cloned().flatten()
        };
        let one = temp_of(&tree(first)).expect("quest temp dir");
        let two = temp_of(&tree(second)).expect("quest temp dir");
        assert_ne!(one, two);
        assert_eq!(one, std::env::temp_dir().join(super::quest_temp_name(first)).to_string_lossy().to_string());
        assert!(std::path::Path::new(&one).is_dir());
        assert_eq!(temp_of(&tree(first).join("src")).as_deref(), Some(one.as_str()));
        assert!(one.len() < 100 && !one.contains(' '));
        let _ = std::fs::remove_dir_all(&one);
        let _ = std::fs::remove_dir_all(&two);
    }

    #[test]
    fn quest_temp_skips_other_dirs() {
        let id = uuid::Uuid::new_v4();
        assert!(super::quest_id(std::path::Path::new("/r/workspaces/agents/main")).is_none());
        assert!(super::quest_id(std::path::Path::new("/r/workspaces/tasks/not-a-uuid/repo")).is_none());
        assert_eq!(super::quest_id(std::path::Path::new(&format!("/tasks/x/runner/workspaces/tasks/{id}/repo"))), Some(id));
        let mut elsewhere = tokio::process::Command::new("true");
        elsewhere.current_dir("/r/workspaces/agents/main");
        configure_child_environment(&mut elsewhere, &BTreeMap::new()).unwrap();
        let temp = child_env(&elsewhere).get("TMPDIR").cloned().flatten().unwrap_or_default();
        assert!(!temp.contains(super::QUEST_TEMP_PREFIX));
    }

    #[test]
    fn child_environment_removes_master_key_and_forwards_grants() {
        let mut command = tokio::process::Command::new("true");
        command.env("GIT_CONFIG_COUNT", "1");
        let environment = BTreeMap::from([("DATABASE_URL".to_string(), "app-db".to_string())]);
        configure_child_environment(&mut command, &environment).unwrap();
        let env = child_env(&command);
        for name in [
            "WORLD_SECRETS_KEY",
            "WORLD_RUNNER_TOKEN",
            "WORLD_RUNNER_MCP_TOKEN",
            "WORLD_RUNNER_URL",
            "WORLD_RUNNER_ID",
            "WORLD_RUNNER_ROOT",
            "WORLD_RUNNER_REPOSITORIES",
            "WORLD_RUNNER_STORAGE_ENDPOINT",
            "WORLD_RUNNER_STORAGE_REGION",
            "WORLD_RUNNER_STORAGE_BUCKET",
            "WORLD_RUNNER_STORAGE_ACCESS_KEY_ID",
            "WORLD_RUNNER_STORAGE_SECRET_ACCESS_KEY",
            "POSTGRES_PASSWORD",
            "REDIS_URL",
            "REDIS_PASSWORD",
            "AUTH_SECRET",
            "NEXTAUTH_SECRET",
            "GH_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(!env.contains_key(name), "{name} should be removed");
        }
        assert_eq!(env.get("DATABASE_URL"), Some(&Some("app-db".to_string())));
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&Some("1".to_string())));
    }

    #[test]
    fn child_environment_keeps_the_gate_root_of_the_job() {
        let mut command = tokio::process::Command::new("true");
        let environment = BTreeMap::from([
            (crate::browser::LAUNCHER_ENV.to_string(), "/x/loka-agent browser a quest".to_string()),
            (crate::browser::ROOT_ENV.to_string(), "/runner".to_string()),
        ]);
        configure_child_environment(&mut command, &environment).unwrap();
        let env = child_env(&command);
        assert_eq!(env.get(crate::browser::ROOT_ENV), Some(&Some("/runner".to_string())));
        assert_eq!(env.get(crate::browser::LAUNCHER_ENV), Some(&Some("/x/loka-agent browser a quest".to_string())));
    }

    #[test]
    fn child_environment_removes_inherited_database_url() {
        let mut command = tokio::process::Command::new("true");
        configure_child_environment(&mut command, &BTreeMap::new()).unwrap();
        assert!(!child_env(&command).contains_key("DATABASE_URL"));
    }

    #[test]
    fn only_allowlisted_host_variables_are_inherited() {
        let providers = ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];
        for name in ["PATH", "HOME", "LC_ALL", "XDG_CONFIG_HOME", "CODEX_HOME", "CLAUDE_CODE_USE_BEDROCK"] {
            assert!(inherits(name, &providers, false), "{name} should be inherited");
        }
        for name in [
            "SLACK_BOT_TOKEN",
            "SLACK_APP_TOKEN",
            "LINEAR_API_KEY",
            "GITHUB_WEBHOOK_SECRET",
            "AUTH_GOOGLE_SECRET",
            "CODE_SERVER_PASSWORD",
            "WORLD_SECRETS_KEY",
            "LOKA_MCP_ENV_0",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!inherits(name, &providers, true), "{name} should not be inherited");
        }
        assert!(inherits("CLAUDE_CODE_OAUTH_TOKEN", &providers, true));
        assert!(!inherits("CLAUDE_CODE_OAUTH_TOKEN", &providers, false));
        assert!(!inherits("ANTHROPIC_API_KEY", &providers, false));
    }

    #[test]
    fn a_parent_claude_session_does_not_leak_into_child_clis() {
        let providers = ["ANTHROPIC_API_KEY"];
        for name in [
            "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SIMPLE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_HOST_SESSION_ID",
            "CLAUDE_CODE_DESKTOP_APP_VERSION",
        ] {
            assert!(!inherits(name, &providers, true), "{name} should not be inherited");
        }
        for name in ["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_MAX_OUTPUT_TOKENS", "AWS_PROFILE", "AWS_REGION", "CLOUD_ML_REGION", "ANTHROPIC_VERTEX_PROJECT_ID"] {
            assert!(inherits(name, &providers, false), "{name} should be inherited");
        }
        assert!(inherits("ANTHROPIC_AUTH_TOKEN", &providers, true));
        assert!(!inherits("ANTHROPIC_AUTH_TOKEN", &providers, false));
        for name in super::CREDENTIAL_FILE_ENV {
            assert!(!inherits(name, &providers, false), "{name} points at host credentials and follows the server key setting");
            assert!(inherits(name, &providers, true));
        }
        let (_, exclude) = shell_environment_patterns(&BTreeMap::new());
        for pattern in ["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "CLAUDE_CODE_SESSION*", "AWS_SHARED_CREDENTIALS_FILE", "GOOGLE_APPLICATION_CREDENTIALS"] {
            assert!(exclude.iter().any(|p| p == pattern), "{pattern} should be excluded from shells");
        }
    }

    #[test]
    fn host_secret_detection_skips_plain_settings() {
        assert!(secret_like("SLACK_BOT_TOKEN", "xoxb-1"));
        assert!(secret_like("WORLD_RUNNER_STORAGE_SECRET_ACCESS_KEY", "x"));
        assert!(secret_like("CODE_SERVER_PASSWORD", "x"));
        assert!(secret_like("DATABASE_URL", "postgres://loka:pw@postgres:5432/loka"));
        assert!(!secret_like("REDIS_URL", "redis://redis:6379"));
        assert!(!secret_like("GIT_AUTHOR_NAME", "Loka Agent"));
        assert!(!secret_like("SSH_AUTH_SOCK", "/tmp/agent.sock"));
        assert!(!secret_like("WORLD_PUBLIC_API_URL", "https://api.world.ygpt.dev"));
    }

    #[test]
    fn child_environment_rejects_reserved_names() {
        let mut command = tokio::process::Command::new("true");
        let environment = BTreeMap::from([("WORLD_SECRETS_KEY".to_string(), "value".to_string())]);
        assert!(configure_child_environment(&mut command, &environment).is_err());
    }
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(3).collect();
    if parts.len() < 3 {
        return path.to_string();
    }
    let shortened: Vec<&str> = parts.into_iter().rev().collect();
    format!("…/{}", shortened.join("/"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn mcp_logins_are_private_to_the_runner() {
        let root = Path::new("/runner");
        assert!(super::private_paths_in(root).contains(&root.join("mcp-auth")));
    }

    #[test]
    fn long_runner_roots_get_short_socket_paths() {
        let short = Path::new("/tmp/runner/leases/mcp.sock");
        assert_eq!(super::socket_path(short).unwrap(), short);
        let root = Path::new("/Users/someone-with-a-very-long-account-name-on-this-mac/Library/Application Support/dev.ygpt.loka/runner");
        let long = root.join("leases/mcp.sock");
        let moved = super::socket_path(&long).unwrap();
        assert!(moved.as_os_str().len() <= 100, "{}", moved.display());
        assert_eq!(moved.file_name().unwrap(), "mcp.sock");
        assert_eq!(super::socket_path(&long).unwrap(), moved);
        assert_ne!(super::socket_path(&root.join("leases/mcp-hub.sock")).unwrap(), moved);
        let listener = std::os::unix::net::UnixListener::bind(&moved).unwrap();
        drop(listener);
        let _ = std::fs::remove_file(&moved);
    }

    use super::{
        collect_stderr, session_id_from_init, summarize_tool_output, truncate_for_log, JobUid,
        FREE_JOB_UIDS, JOB_UID, TOOL_OUTPUT_MAX_CHARS,
    };

    #[test]
    fn runtime_launchers_pass_but_other_reserved_names_do_not() {
        use std::collections::BTreeMap;
        let mut cmd = tokio::process::Command::new("true");
        let launcher = BTreeMap::from([(crate::browser::LAUNCHER_ENV.to_string(), "/bin/launcher".to_string())]);
        super::configure_child_environment(&mut cmd, &launcher).unwrap();
        let reserved = BTreeMap::from([("WORLD_SECRETS_KEY".to_string(), "x".to_string())]);
        assert!(super::configure_child_environment(&mut cmd, &reserved).is_err());
    }

    #[test]
    fn every_agent_cli_runs_read_only() {
        for cli in ["claude", "codex", "cursor", "opencode", "omp"] {
            assert!(super::honours_read_only(cli), "{cli}");
        }
        assert!(!super::honours_read_only("grok"));
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn job_uids_return_to_the_pool_only_after_every_holder_lets_go() {
        FREE_JOB_UIDS.lock().unwrap().push(4242);
        let job = JobUid::take().await.unwrap().unwrap();
        let warm = JOB_UID.sync_scope(job.clone(), JobUid::current).unwrap();
        assert_eq!(warm.id(), 4242);
        drop(job);
        assert!(!FREE_JOB_UIDS.lock().unwrap().contains(&4242));
        drop(warm);
        assert!(FREE_JOB_UIDS.lock().unwrap().contains(&4242));
    }

    #[test]
    fn truncates_tool_output_to_log_cap() {
        let long = "a".repeat(TOOL_OUTPUT_MAX_CHARS + 64);
        let truncated = truncate_for_log(&long, TOOL_OUTPUT_MAX_CHARS);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), TOOL_OUTPUT_MAX_CHARS + 1);
    }

    #[test]
    fn flattens_nested_tool_output_blocks() {
        let value = serde_json::json!([
            {"type": "text", "text": "line one"},
            {"type": "text", "text": "line two"}
        ]);
        assert_eq!(summarize_tool_output(&value), "line one\nline two");
    }

    #[test]
    fn captures_only_top_level_session_initialization() {
        let claude = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "claude-task"
        });
        let codex = serde_json::json!({
            "type": "thread.started",
            "thread_id": "codex-task"
        });
        let cursor = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "cursor-task"
        });
        let nested = serde_json::json!({
            "type": "assistant",
            "session_id": "nested-agent"
        });

        assert_eq!(session_id_from_init("claude", &claude), Some("claude-task"));
        assert_eq!(session_id_from_init("codex", &codex), Some("codex-task"));
        assert_eq!(session_id_from_init("cursor", &cursor), Some("cursor-task"));
        assert_eq!(session_id_from_init("claude", &nested), None);
        assert_eq!(session_id_from_init("codex", &nested), None);
        assert_eq!(session_id_from_init("cursor", &nested), None);
    }

    #[tokio::test]
    async fn stops_stderr_reader_when_pipe_never_closes() {
        struct Dropped(Arc<AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = dropped.clone();
        let handle = tokio::spawn(async move {
            let _dropped = Dropped(task_dropped);
            std::future::pending::<String>().await
        });
        tokio::task::yield_now().await;

        let result = tokio::time::timeout(Duration::from_secs(2), collect_stderr(handle))
            .await
            .expect("stderr collection should be bounded");

        assert!(result.is_empty());
        assert!(dropped.load(Ordering::SeqCst));
    }
}

pub const COMPACT_PROMPT: &str = "/compact";

pub fn compaction_run(backend: &str, opts: &CliOptions<'_>) -> bool {
    backend != "claude" && opts.prompt == COMPACT_PROMPT && opts.resume && opts.session_id.is_some()
}

pub fn check_model(model: &str) -> Result<()> {
    if model.trim_start().starts_with('-') {
        anyhow::bail!("model names cannot start with a dash");
    }
    Ok(())
}

fn honours_read_only(backend: &str) -> bool {
    matches!(backend, "claude" | "codex" | "cursor" | "opencode" | "omp")
}

pub async fn run(backend: &str, opts: CliOptions<'_>) -> Result<CliResult> {
    check_model(opts.model)?;
    if !loka_shared::backends::enabled_here(backend) {
        anyhow::bail!(loka_shared::backends::OPENCODE_DISABLED);
    }
    if opts.approval == Approval::ReadOnly && !opts.lean && !honours_read_only(backend) {
        anyhow::bail!("{backend} cannot run in read-only mode. Switch this agent to Claude or Codex, or change its approval mode.");
    }
    if !steers_natively(backend) && !opts.lean {
        if let Some(steer) = opts.steer.take() {
            return relay(backend, opts, steer).await;
        }
    }
    dispatch(backend, opts).await
}

async fn dispatch(backend: &str, opts: CliOptions<'_>) -> Result<CliResult> {
    match backend {
        "codex" => crate::codex::run(opts).await,
        "opencode" => crate::opencode::run(opts).await,
        "cursor" => crate::cursor::run(opts).await,
        "grok" => crate::grok::run(opts).await,
        "omp" => crate::omp::run(opts).await,
        _ => crate::claude::run(opts).await,
    }
}

#[cfg(test)]
mod steer_live_tests {
    use super::{deliver, run, CliOptions, SteerSlot};

    async fn steered(backend: &str, model: &str, interrupt: bool) -> String {
        let dir = std::env::temp_dir();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let steer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(12)).await;
            let text = if interrupt {
                "Stop waiting on that command and do not run it again. Just reply with the word banana."
            } else {
                "Also put the word banana on its own line at the end of your final reply."
            };
            deliver(&sender, text.into(), interrupt).await
        });
        let result = run(
            backend,
            CliOptions {
                working_dir: &dir,
                prompt: "Run this exact shell command in the foreground and wait for it: python3 -c \"import time; time.sleep(40); print('slept')\" . Then reply with its output.",
                system_prompt: None,
                allowed_tools: None,
                max_turns: 10,
                model,
                mcp_config_json: None,
                session_id: None,
                resume: false,
                event_tx: None,
                image_paths: Vec::new(),
                fork: false,
                lean: false,
                effort: Some("low"),
                fast: false,
                environment: Default::default(),
                mcp_has_secrets: false,
                sensitive: false,
                rewind_to: None,
                approval: Default::default(),
                steer: SteerSlot::new(receiver),
                omp_models: None,
                provider_keys: Default::default(),
                readable_files: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert!(steer.await.unwrap(), "{backend} did not accept the steer");
        result.output
    }

    #[tokio::test]
    #[ignore]
    async fn live_relay_stops_and_resumes() {
        for interrupt in [false, true] {
            let dir = std::env::temp_dir();
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let steer = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(12)).await;
                deliver(&sender, "Also put the word banana on its own line at the end of your final reply.".into(), interrupt).await
            });
            let session = uuid::Uuid::new_v4().to_string();
            let started = std::time::Instant::now();
            let result = super::relay(
                "claude",
                CliOptions {
                    working_dir: &dir,
                    prompt: "Run this exact shell command in the foreground and wait for it: python3 -c \"import time; time.sleep(40); print('slept')\" . Then reply with its output.",
                    system_prompt: None,
                    allowed_tools: None,
                    max_turns: 10,
                    model: "haiku",
                    mcp_config_json: None,
                    session_id: Some(&session),
                    resume: false,
                    event_tx: None,
                    image_paths: Vec::new(),
                    fork: false,
                    lean: false,
                    effort: Some("low"),
                    fast: false,
                    environment: Default::default(),
                    mcp_has_secrets: false,
                    sensitive: false,
                    rewind_to: None,
                    approval: Default::default(),
                    steer: SteerSlot::default(),
                    omp_models: None,
                    provider_keys: Default::default(),
                    readable_files: Vec::new(),
                },
                receiver,
            )
            .await
            .unwrap();
            assert!(steer.await.unwrap());
            eprintln!("relay interrupt={interrupt} {:?}: {}", started.elapsed(), result.output);
            assert!(result.output.to_lowercase().contains("banana"), "{}", result.output);
            if interrupt {
                assert!(started.elapsed() < std::time::Duration::from_secs(40));
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_steer_and_interrupt() {
        let _ = tracing_subscriber::fmt().with_env_filter("loka_agent=info").try_init();
        let mut backends = vec![("claude".to_string(), "haiku".to_string())];
        for (backend, var) in [("codex", "CODEX_TEST_MODEL"), ("omp", "OMP_TEST_MODEL"), ("cursor", "CURSOR_TEST_MODEL"), ("opencode", "OPENCODE_TEST_MODEL")] {
            if let Ok(model) = std::env::var(var) {
                backends.push((backend.to_string(), model));
            }
        }
        let only = std::env::var("STEER_TEST_ONLY").ok();
        for (backend, model) in backends.iter().filter(|(b, _)| only.as_deref().is_none_or(|o| o == b)) {
            let (backend, model) = (backend.as_str(), model.as_str());
            for interrupt in [false, true] {
                let started = std::time::Instant::now();
                let output = steered(backend, model, interrupt).await;
                eprintln!("{backend} interrupt={interrupt} {:?}: {output}", started.elapsed());
                assert!(output.to_lowercase().contains("banana"), "{backend} interrupt={interrupt}: {output}");
                if interrupt {
                    assert!(started.elapsed() < std::time::Duration::from_secs(40), "{backend} interrupt did not stop the command");
                }
            }
        }
    }
}

enum Stop {
    No,
    AtBoundary,
    Now,
}

async fn relay(backend: &str, opts: CliOptions<'_>, mut steer: SteerReceiver) -> Result<CliResult> {
    let interrupt_after = steer_interrupt_after();
    let hang_after = tool_hang_after();
    let mut prompt = opts.prompt.to_string();
    let mut session_id = opts.session_id.map(String::from);
    let mut resume = opts.resume;
    let mut fork = opts.fork;
    let mut outputs: Vec<String> = Vec::new();
    let mut usage = Usage::default();
    let mut cost_usd = 0.0;
    let mut checkpoint = None;
    loop {
        let (tx, mut events) = mpsc::unbounded_channel::<StreamEvent>();
        let mut pids: Vec<u32> = Vec::new();
        let mut tool_started: Option<std::time::Instant> = None;
        let mut pending: Vec<String> = Vec::new();
        let mut stop = Stop::No;
        let mut stopped = false;
        let mut hard = false;
        let mut started_session: Option<String> = None;
        let mut watchdog = tokio::time::interval(Duration::from_secs(15));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let outcome = {
            let segment = dispatch(
                backend,
                CliOptions {
                    working_dir: opts.working_dir,
                    prompt: &prompt,
                    system_prompt: opts.system_prompt,
                    allowed_tools: opts.allowed_tools,
                    max_turns: opts.max_turns,
                    model: opts.model,
                    mcp_config_json: opts.mcp_config_json.clone(),
                    session_id: session_id.as_deref(),
                    resume,
                    event_tx: Some(tx),
                    image_paths: if outputs.is_empty() { opts.image_paths.clone() } else { Vec::new() },
                    fork,
                    lean: false,
                    effort: opts.effort,
                    fast: opts.fast,
                    environment: opts.environment.clone(),
                    mcp_has_secrets: opts.mcp_has_secrets,
                    sensitive: opts.sensitive,
                    rewind_to: None,
                    approval: opts.approval,
                    steer: SteerSlot::default(),
                    omp_models: None,
                    provider_keys: opts.provider_keys.clone(),
                    readable_files: opts.readable_files.clone(),
                },
            );
            tokio::pin!(segment);
            loop {
                tokio::select! {
                    outcome = &mut segment => break outcome,
                    Some(event) = events.recv() => {
                        let boundary = matches!(event, StreamEvent::ToolResult { .. } | StreamEvent::AssistantText(_));
                        match &event {
                            StreamEvent::Spawned { pid } => pids.push(*pid),
                            StreamEvent::SessionStarted { session_id } => started_session = Some(session_id.clone()),
                            StreamEvent::ToolUse { .. } => { tool_started.get_or_insert_with(std::time::Instant::now); }
                            StreamEvent::ToolResult { .. } => tool_started = None,
                            _ => {}
                        }
                        if let Some(forward) = &opts.event_tx {
                            let _ = forward.send(event);
                        }
                        if boundary && matches!(stop, Stop::AtBoundary) {
                            stop = Stop::Now;
                        }
                    }
                    Some(next) = steer.recv() => {
                        let stuck = tool_started.is_some_and(|t| t.elapsed() >= interrupt_after);
                        if next.interrupt || stuck {
                            stop = Stop::Now;
                            hard = true;
                        } else if matches!(stop, Stop::No) {
                            stop = if tool_started.is_some() { Stop::AtBoundary } else { Stop::Now };
                        }
                        pending.push(next.text);
                        let _ = next.ack.send(true);
                    }
                    _ = watchdog.tick() => {
                        if let (Some(limit), Some(at)) = (hang_after, tool_started) {
                            if at.elapsed() >= limit {
                                warn!(backend, secs = at.elapsed().as_secs(), "Tool call looks stuck, stopping to resume");
                                pending.insert(0, hang_notice(at.elapsed()));
                                stop = Stop::Now;
                                hard = true;
                            }
                        }
                    }
                }
                if matches!(stop, Stop::Now) && !stopped {
                    stopped = true;
                    tracing::debug!(backend, "Stopping run to pass in a message, then resuming");
                    kill_process_trees(std::mem::take(&mut pids)).await;
                }
            }
        };
        while let Ok(next) = events.try_recv() {
            if let StreamEvent::SessionStarted { session_id } = &next {
                started_session = Some(session_id.clone());
            }
            if let Some(forward) = &opts.event_tx {
                let _ = forward.send(next);
            }
        }
        match outcome {
            Ok(result) => {
                if !result.output.trim().is_empty() {
                    outputs.push(result.output);
                }
                usage.input_tokens += result.usage.input_tokens;
                usage.cached_input_tokens += result.usage.cached_input_tokens;
                usage.output_tokens += result.usage.output_tokens;
                cost_usd = if loka_shared::backends::cost_per_run(backend) { cost_usd + result.cost_usd } else { result.cost_usd.max(cost_usd) };
                checkpoint = result.checkpoint.or(checkpoint);
                if let Some(id) = result.session_id {
                    session_id = Some(id);
                }
            }
            Err(error) if !stopped => {
                close_steers(&mut steer);
                return Err(error);
            }
            Err(error) => info!(backend, %error, "Stopped run ended with an error, resuming"),
        }
        if let Some(id) = started_session {
            session_id = Some(id);
        }
        while let Ok(next) = steer.try_recv() {
            pending.push(next.text);
            let _ = next.ack.send(true);
        }
        if pending.is_empty() {
            break;
        }
        let Some(_) = session_id.as_ref() else {
            close_steers(&mut steer);
            anyhow::bail!("{backend} gave no session id, so the run could not be resumed with the new message");
        };
        prompt = pending.join("\n\n");
        if hard {
            prompt = format!("{INTERRUPTED_NOTE}\n\n{prompt}");
        }
        resume = true;
        fork = false;
    }
    close_steers(&mut steer);
    Ok(CliResult {
        output: outputs.join("\n\n"),
        session_id,
        cost_usd,
        exit_code: 0,
        usage,
        checkpoint,
    })
}

#[cfg(test)]
mod parent_pid_tests {
    #[test]
    fn parent_pid_matches_getppid() {
        assert_eq!(super::parent_pid(std::process::id()), Some(unsafe { libc::getppid() } as u32));
        assert_eq!(super::parent_pid(u32::MAX - 7), None);
    }
}
