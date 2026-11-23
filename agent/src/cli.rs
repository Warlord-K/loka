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

pub use loka_shared::redaction::Redactor;

impl StreamEvent {
    pub fn redacted_clip(self, redactor: &Redactor, text_max: usize) -> Self {
        match self {
            StreamEvent::ToolUse { tool, input_summary } => StreamEvent::ToolUse {
                tool: redactor.redact(&tool),
                input_summary: redactor.clip(&input_summary, TOOL_INPUT_MAX_CHARS.min(text_max)),
            },
            StreamEvent::ToolResult { tool, output } => StreamEvent::ToolResult {
                tool: redactor.redact(&tool),
                output: redactor.clip(&output, text_max),
            },
            StreamEvent::AssistantText(text) => StreamEvent::AssistantText(redactor.clip(&text, text_max)),
            StreamEvent::Error(text) => StreamEvent::Error(redactor.clip(&text, text_max)),
            other => other,
        }
    }
}

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
pub(crate) const ROOM_ANSWER_ENV: &str = "LOKA_ROOM_ANSWER";

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

pub(crate) fn private_paths_in(root: &Path) -> Vec<PathBuf> {
    ["leases", "runner.log", "identity", "browser", MCP_AUTH_DIR].iter().map(|name| root.join(name)).collect()
}

tokio::task_local! {
    static JOB_UID: JobUid;
}

tokio::task_local! {
    pub(crate) static SESSION_TEMP: uuid::Uuid;
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
pub(crate) const SESSION_TEMP_PREFIX: &str = "loka-s-";
pub(crate) const PRIVATE_TEMP_OWNER: &str = ".loka-owner";
pub(crate) const PRIVATE_TEMP_NAME_LEN: usize = QUEST_TEMP_PREFIX.len() + 12;
pub(crate) const SHORT_TEMP_ROOT: &str = "/tmp";
const CLAUDE_SHELL_TEMP_LIMIT: usize = 44;
const CLAUDE_SHELL_TEMP_SUFFIX: &str = "/claude-";

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

pub(crate) fn short_enough_for_shell_temp(root: &Path) -> bool {
    let uid_digits = current_uid().to_string().len();
    let longest = root.as_os_str().len() + 1 + PRIVATE_TEMP_NAME_LEN + CLAUDE_SHELL_TEMP_SUFFIX.len() + uid_digits;
    longest <= CLAUDE_SHELL_TEMP_LIMIT
}

pub(crate) const PRIVATE_TEMP_ROOT_ENV: &str = "PRIVATE_TEMP_ROOT";

pub(crate) fn private_temp_root() -> PathBuf {
    match loka_shared::loka_env::var(PRIVATE_TEMP_ROOT_ENV).filter(|root| !root.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => private_temp_root_in(std::env::temp_dir()),
    }
}

fn private_temp_root_in(preferred: PathBuf) -> PathBuf {
    if cfg!(unix) && !short_enough_for_shell_temp(&preferred) {
        return PathBuf::from(SHORT_TEMP_ROOT);
    }
    preferred
}

pub(crate) fn private_temp_homes() -> Vec<PathBuf> {
    let mut homes = vec![private_temp_root()];
    let shared = std::env::temp_dir();
    if !homes.contains(&shared) {
        homes.push(shared);
    }
    homes
}

pub(crate) fn both_spellings(path: &Path) -> Vec<PathBuf> {
    let mut spellings = vec![path.to_path_buf()];
    let canonical = std::fs::canonicalize(path).ok().or_else(|| Some(std::fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?)));
    spellings.extend(canonical.filter(|real| real != path));
    spellings
}

pub(crate) async fn scope_session_temp<F: std::future::Future>(session: Option<uuid::Uuid>, future: F) -> F::Output {
    match session {
        Some(id) => SESSION_TEMP.scope(id, future).await,
        None => future.await,
    }
}

pub(crate) fn session_temp_name(id: uuid::Uuid) -> String {
    format!("{SESSION_TEMP_PREFIX}{}", &id.simple().to_string()[..12])
}

pub(crate) fn private_temp_owner(dir: &Path) -> Option<(uuid::Uuid, PathBuf)> {
    let text = std::fs::read_to_string(dir.join(PRIVATE_TEMP_OWNER)).ok()?;
    let mut lines = text.lines();
    let id: uuid::Uuid = lines.next()?.parse().ok()?;
    let name = dir.file_name()?.to_str()?;
    (name == session_temp_name(id) || name == quest_temp_name(id)).then(|| (id, PathBuf::from(lines.next().unwrap_or_default())))
}

pub(crate) fn mark_private_temp_owner(dir: &Path, id: uuid::Uuid, root: &Path) -> std::io::Result<()> {
    let marker = dir.join(PRIVATE_TEMP_OWNER);
    if marker.exists() {
        return Ok(());
    }
    std::fs::write(marker, format!("{id}\n{}\n", root.display()))
}

pub(crate) fn private_temp_dir(working_dir: &Path) -> Option<PathBuf> {
    if job_user().is_some() {
        return None;
    }
    let name = match quest_id(working_dir) {
        Some(quest) => quest_temp_name(quest),
        None => session_temp_name(SESSION_TEMP.try_with(|id| *id).ok()?),
    };
    Some(private_temp_root().join(name))
}

pub(crate) fn room_answer_dir(session: uuid::Uuid) -> std::io::Result<PathBuf> {
    let root = private_temp_root().join(session_temp_name(session));
    ensure_private_dir(&root)?;
    mark_private_temp_owner(&root, session, Path::new(""))?;
    let dir = root.join(loka_shared::world::ROOM_ANSWER_WORKSPACE);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

pub(crate) fn set_temp_env(cmd: &mut tokio::process::Command, dir: &Path) {
    for name in ["TMPDIR", "TMP", "TEMP"] {
        cmd.env(name, dir);
    }
}

fn prepare_private_temp_dir(dir: &Path, working_dir: &Path) -> std::io::Result<()> {
    ensure_private_dir(dir)?;
    if let Some((quest, root)) = quest_place(working_dir).filter(|(quest, _)| dir.ends_with(quest_temp_name(*quest))) {
        return mark_private_temp_owner(dir, quest, &root);
    }
    let Some(id) = SESSION_TEMP.try_with(|id| *id).ok().filter(|id| dir.ends_with(session_temp_name(*id))) else {
        return Ok(());
    };
    mark_private_temp_owner(dir, id, Path::new(&std::env::var("WORLD_RUNNER_ROOT").unwrap_or_default()))
}

pub(crate) fn quest_place(working_dir: &Path) -> Option<(uuid::Uuid, PathBuf)> {
    let parts: Vec<std::path::Component> = working_dir.components().collect();
    let at = parts.windows(2).position(|pair| pair[0].as_os_str() == "tasks" && pair[1].as_os_str().to_str().is_some_and(|id| id.parse::<uuid::Uuid>().is_ok()))?;
    let id = parts[at + 1].as_os_str().to_str()?.parse().ok()?;
    let root_end = if at > 0 && parts[at - 1].as_os_str() == "workspaces" { at - 1 } else { at };
    Some((id, parts[..root_end].iter().collect()))
}

pub(crate) fn quest_id(working_dir: &Path) -> Option<uuid::Uuid> {
    quest_place(working_dir).map(|(id, _)| id)
}

pub(crate) fn quest_temp_name(id: uuid::Uuid) -> String {
    format!("{QUEST_TEMP_PREFIX}{}", &id.simple().to_string()[..12])
}

pub(crate) fn quest_temp_dir(working_dir: &Path) -> Option<PathBuf> {
    if job_user().is_some() {
        return None;
    }
    Some(private_temp_root().join(quest_temp_name(quest_id(working_dir)?)))
}

pub(crate) const QUEST_TARGET_DIR: &str = "cargo-target";

const QUEST_SHELL_ENV: [&str; 5] = ["TMP", "TEMP", "CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG"];

const LEAN_CARGO_ENV: [(&str, &str); 2] = [("CARGO_INCREMENTAL", "0"), ("CARGO_PROFILE_DEV_DEBUG", "0")];

pub(crate) fn quest_target_dir(working_dir: &Path) -> Option<PathBuf> {
    quest_temp_dir(working_dir).map(|dir| dir.join(QUEST_TARGET_DIR))
}

pub(crate) fn prepare_quest_temp_dir(working_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let Some(dir) = quest_temp_dir(working_dir) else {
        return Ok(None);
    };
    prepare_private_temp_dir(&dir, working_dir)?;
    Ok(Some(dir.join(QUEST_TARGET_DIR)))
}

pub(crate) fn owned_temp_dir(working_dir: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(private_temp_dir(working_dir)?).ok()
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let found = std::fs::symlink_metadata(dir)?;
        if !found.is_dir() || found.uid() != current_uid() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} already exists and is not a folder of this user, so it cannot be a private temp folder", dir.display()),
            ));
        }
        if found.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
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
        command.env("TMPDIR", &dir).env("TMP", &dir).env("TEMP", &dir).env("HOME", dir);
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
    let dir = short_runner_dir(&std::env::temp_dir(), wanted)?;
    let name = wanted.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "socket".into());
    let short = dir.join(name);
    if short.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("no short enough place for the socket {}", wanted.display())));
    }
    Ok(short)
}

pub(crate) fn launcher_exe() -> Option<String> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    shell_safe_exe(&exe, &std::env::temp_dir())
}

pub(crate) fn launcher_alias_dir() -> Option<PathBuf> {
    let temp = std::env::temp_dir();
    PathBuf::from(launcher_exe()?).parent().filter(|dir| dir.starts_with(&temp)).map(Path::to_path_buf)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GatePaths {
    pub readable: Vec<PathBuf>,
    pub sockets: Vec<PathBuf>,
}

pub(crate) fn gate_paths() -> GatePaths {
    let root = std::env::var_os("WORLD_RUNNER_ROOT").filter(|root| !root.is_empty()).map(PathBuf::from);
    gate_paths_for(root.as_deref(), launcher_alias_dir())
}

pub(crate) fn gate_paths_for(runner_root: Option<&Path>, launcher_alias: Option<PathBuf>) -> GatePaths {
    let sockets: Vec<PathBuf> = runner_root.and_then(|root| crate::browser::socket_for(root).ok()).map(|socket| both_spellings(&socket)).unwrap_or_default();
    let readable = launcher_alias.iter().flat_map(|dir| both_spellings(dir)).chain(sockets.iter().cloned()).collect();
    GatePaths { readable, sockets }
}

fn shell_safe_exe(exe: &Path, temp: &Path) -> Option<String> {
    if let Some(word) = shell_word(exe) {
        return Some(word);
    }
    let alias = short_runner_dir(temp, exe).ok()?.join(exe.file_name()?);
    link_to(&alias, exe).ok()?;
    shell_word(&alias)
}

fn shell_word(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)).then(|| text.to_string())
}

fn link_to(alias: &Path, target: &Path) -> std::io::Result<()> {
    if std::fs::read_link(alias).is_ok_and(|current| current == target) {
        return Ok(());
    }
    let staged = alias.with_extension(format!("link-{}", uuid::Uuid::new_v4().simple()));
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &staged)?;
    std::fs::rename(&staged, alias).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

fn short_runner_dir(temp: &Path, key: &Path) -> std::io::Result<PathBuf> {
    let hash = key.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = temp.join(format!("loka-{:012x}", hash & 0xffff_ffff_ffff));
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
    Ok(dir)
}

#[cfg(unix)]
pub(crate) fn runner_uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn steer_interrupt_after() -> Duration {
    Duration::from_secs(env_secs("WORLD_STEER_INTERRUPT_SECS", 180))
}

pub fn background_wait_cap() -> Option<Duration> {
    match env_secs("WORLD_BACKGROUND_WAIT_SECS", 1200) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
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
pub(crate) fn process_exe(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u8; 4096];
    let len = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(buf)))
}

#[cfg(target_os = "linux")]
pub(crate) fn process_exe(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process_exe(_pid: u32) -> Option<PathBuf> {
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

pub fn stderr_tail(pipe: Option<tokio::process::ChildStderr>, redactor: Redactor) -> std::sync::Arc<std::sync::Mutex<String>> {
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
                *text = redactor.redact(&text);
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
        environment.remove(ROOM_ANSWER_ENV);
        environment
    }

    pub fn room_answer(&self) -> bool {
        self.environment.get(ROOM_ANSWER_ENV).map(String::as_str) == Some("1")
    }

    pub fn redactor(&self) -> Redactor {
        let mut values: Vec<String> = self.child_environment().into_values().collect();
        if let Some(Ok(mcp)) = self.mcp_config_json.as_deref().map(serde_json::from_str) {
            crate::remote_runner::collect_json_strings(&mcp, &mut values);
        }
        values.extend(host_secrets().iter().cloned());
        Redactor::new(values)
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
        .chain(QUEST_SHELL_ENV.iter().map(|name| name.to_string()))
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

#[cfg(test)]
pub(crate) fn passed_on(name: &str) -> bool {
    inherits(name, &loka_shared::providers::env_names().collect::<Vec<_>>(), true)
}

const RUNTIME_ENV: [&str; 8] = [crate::browser::LAUNCHER_ENV, crate::browser::ROOT_ENV, crate::browser::SOCKET_ENV, crate::screen::LAUNCHER_ENV, crate::apps::LAUNCHER_ENV, crate::apps::LEASE_ENV, loka_shared::command_rules::ENV, loka_shared::approvals::ENV];

fn runtime_name(name: &str) -> bool {
    RUNTIME_ENV.contains(&name)
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
    let target_chosen = cmd.as_std().get_envs().any(|(name, value)| name == "CARGO_TARGET_DIR" && value.is_some());
    let working_dir = cmd.as_std().get_current_dir().map(Path::to_path_buf);
    if let Some(dir) = private_temp_dir(working_dir.as_deref().unwrap_or(Path::new(""))) {
        prepare_private_temp_dir(&dir, working_dir.as_deref().unwrap_or(Path::new("")))?;
        if working_dir.as_deref().and_then(quest_temp_dir).is_some() {
            if !target_chosen {
                cmd.env("CARGO_TARGET_DIR", dir.join(QUEST_TARGET_DIR));
            }
            for (name, lean) in LEAN_CARGO_ENV {
                if !cmd.as_std().get_envs().any(|(set, value)| set == name && value.is_some()) {
                    cmd.env(name, lean);
                }
            }
        }
        set_temp_env(cmd, &dir);
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

pub fn host_redactor() -> Redactor {
    Redactor::new(host_secrets())
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
            cmd.to_string()
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

pub const TOOL_INPUT_MAX_CHARS: usize = 120;

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

    fn set_by_runs() -> Vec<&'static str> {
        super::LEAN_CARGO_ENV
            .iter()
            .map(|(name, _)| *name)
            .chain(super::RUNTIME_ENV)
            .chain(["CARGO_TARGET_DIR", "CLAUDE_CODE_TMPDIR", "GIT_CONFIG_COUNT", "GIT_TEMPLATE_DIR", crate::cmd_hub::HUB_ENV, crate::cmd_hub::REAL_SHELL_ENV])
            .chain([crate::cmd_hub::Hook::ClaudeShell, crate::cmd_hub::Hook::Shell].map(crate::cmd_hub::Hook::variable).into_iter().filter(|name| *name != "SHELL"))
            .collect()
    }

    #[test]
    fn every_variable_a_quest_run_sets_is_one_tests_start_without() {
        for name in set_by_runs() {
            assert!(crate::test_env::from_quest_run(name), "{name}");
        }
        assert_eq!(crate::cmd_hub::REAL_SHELL_ENV, crate::test_env::REAL_SHELL);
        assert_eq!(super::PRIVATE_TEMP_ROOT_ENV, crate::test_env::PRIVATE_TEMP_ROOT);
        assert_eq!(crate::test_env::QUEST_TEMP_PREFIXES, [super::QUEST_TEMP_PREFIX, super::SESSION_TEMP_PREFIX]);
        for name in super::HOST_SESSION_ENV {
            assert!(crate::test_env::from_quest_run(name), "{name} belongs to the Claude session hosting the quest");
        }
    }

    #[test]
    fn tests_keep_every_variable_clis_get_from_the_host() {
        let set = set_by_runs();
        for name in super::INHERITED_ENV.iter().filter(|name| !set.contains(name)) {
            assert!(super::passed_on(name), "{name}");
            assert!(!crate::test_env::from_quest_run(name), "{name} reaches CLIs in production, so tests keep it");
        }
    }

    #[test]
    fn tests_that_spawn_clis_and_git_pass_inside_a_quest_run_environment() {
        let base = std::env::temp_dir().join(format!("quest-run-env-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&base).unwrap();
        let global = base.join("gitconfig");
        std::fs::write(&global, "[user]\n\tname = Leaked Quest\n\temail = leak@example.com\n[branch]\n\tautoSetupMerge = false\n").unwrap();
        let quest_temp = super::private_temp_root().join(super::quest_temp_name(uuid::Uuid::new_v4())).join("claude-501");
        let tests = [
            "claude::tests::quest_claude_runs_get_their_own_writable_temp_dir",
            "claude::sandbox::live_tests::owner_quest_branches_off_a_remote_branch_never_write_tracking_to_the_shared_config",
            "git::workspace::tests::owner_identity_comes_from_gh_repo_level_only",
            "git::workspace::tests::missing_owner_identity_falls_back_without_global_writes",
            "test_env::tests::the_test_process_starts_without_quest_run_variables",
        ];
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(tests)
            .args(["--exact", "--test-threads=1"])
            .env("CLAUDE_CODE_TMPDIR", &quest_temp)
            .env("CLAUDE_CODE_SHELL", base.join("bash"))
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "branch.autoSetupMerge")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_CONFIG_KEY_1", "user.name")
            .env("GIT_CONFIG_VALUE_1", "Leaked Quest")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_PARAMETERS", "'user.email=leak@example.com'")
            .env("GIT_DIR", base.join("not-a-repo"))
            .env("GIT_WORK_TREE", base.join("not-a-tree"))
            .env("GIT_TEMPLATE_DIR", "")
            .env("CARGO_TARGET_DIR", base.join("cargo-target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .env(crate::cmd_hub::HUB_ENV, base.join("hub.sock"))
            .env(crate::cmd_hub::REAL_SHELL_ENV, "/bin/sh")
            .env(loka_shared::approvals::ENV, "[]")
            .env("WORLD_TASK_SANDBOX", "off")
            .env("KARNA_TASK_SANDBOX", "off")
            .output()
            .unwrap();
        std::fs::remove_dir_all(&base).ok();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains(&format!("test result: ok. {} passed", tests.len())), "{stdout}");
    }

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
        assert_eq!(one, super::private_temp_root().join(super::quest_temp_name(first)).to_string_lossy().to_string());
        assert!(std::path::Path::new(&one).is_dir());
        assert_eq!(temp_of(&tree(first).join("src")).as_deref(), Some(one.as_str()));
        assert!(one.len() < 100 && !one.contains(' '));
        let _ = std::fs::remove_dir_all(&one);
        let _ = std::fs::remove_dir_all(&two);
    }

    #[test]
    fn private_temp_roots_stay_short_enough_for_claude_shell_temp_and_unix_sockets() {
        use std::path::PathBuf;
        let shell_temp = |dir: &std::path::Path| format!("{}/claude-{}", dir.display(), super::current_uid());
        for preferred in [
            "/var/folders/hx/nz8b2hjn2z31tnxfzn48rfl80000gn/T",
            "/var/folders/hx/nz8b2hjn2z31tnxfzn48rfl80000gn/T/loka-q-0123456789ab/claude-501",
            "/home/someone/a/long/place/for/temp",
            "/tmp",
        ] {
            let root = super::private_temp_root_in(PathBuf::from(preferred));
            let dir = root.join(super::quest_temp_name(uuid::Uuid::new_v4()));
            assert!(shell_temp(&dir).len() <= 44, "Claude Code falls back to the shared /tmp/claude-<uid> past 44 bytes: {}", shell_temp(&dir));
            assert!(format!("{}/claude-501/cc-socks/12345.sock", dir.display()).len() < 104, "{preferred}");
        }
        assert_eq!(super::private_temp_root_in(PathBuf::from("/var/folders/hx/nz8b2hjn2z31tnxfzn48rfl80000gn/T")), PathBuf::from("/tmp"));
        assert_eq!(super::private_temp_root_in(PathBuf::from("/tmp")), PathBuf::from("/tmp"));
        assert_eq!(super::private_temp_root_in(PathBuf::from("/var/tmp")), PathBuf::from("/var/tmp"));
        assert!(super::private_temp_homes().contains(&std::env::temp_dir()), "folders made under the old root are still found and cleaned");
    }

    #[test]
    fn room_answers_get_a_private_neutral_directory() {
        let session = uuid::Uuid::new_v4();
        let private_main = std::env::temp_dir().join(format!("private-main-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&private_main).unwrap();
        std::fs::write(private_main.join("CLAUDE.md"), "private sentinel").unwrap();
        let room = super::room_answer_dir(session).unwrap();
        assert!(room.ends_with(loka_shared::world::ROOM_ANSWER_WORKSPACE));
        assert!(!room.starts_with(&private_main));
        assert!(!room.ancestors().any(|parent| parent.join("CLAUDE.md").exists() || parent.join("AGENTS.md").exists()));
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&room).unwrap().permissions()) & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(super::private_temp_root().join(super::session_temp_name(session)));
        let _ = std::fs::remove_dir_all(private_main);
    }

    #[test]
    fn a_folder_someone_else_made_never_becomes_a_private_temp_dir() {
        let base = std::env::temp_dir().join(format!("private-dir-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&base).unwrap();
        let fresh = base.join("fresh");
        super::ensure_private_dir(&fresh).unwrap();
        let loose = base.join("loose");
        std::fs::create_dir(&loose).unwrap();
        std::fs::set_permissions(&loose, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        super::ensure_private_dir(&loose).unwrap();
        let mode = |dir: &std::path::Path| std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(dir).unwrap().permissions()) & 0o777;
        assert_eq!((mode(&fresh), mode(&loose)), (0o700, 0o700));
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let planted = base.join("planted");
        std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
        assert!(super::ensure_private_dir(&planted).is_err(), "a symlink planted at the name is refused");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn every_cli_exports_and_grants_the_same_quest_temp_dir() {
        use serde_json::{json, Value};
        let quest = uuid::Uuid::new_v4();
        let cwd = std::path::PathBuf::from(format!("/Users/a/Library/Application Support/dev.ygpt.loka/runner/workspaces/tasks/{quest}/world"));
        let dir = super::private_temp_root().join(super::quest_temp_name(quest));
        let mut command = tokio::process::Command::new("true");
        command.current_dir(&cwd);
        configure_child_environment(&mut command, &BTreeMap::new()).unwrap();
        let env = child_env(&command);
        for name in ["TMPDIR", "TMP", "TEMP"] {
            assert_eq!(env[name].as_deref(), Some(dir.to_str().unwrap()), "{name}: every CLI and every shell it starts gets the quest dir");
        }
        assert_eq!(env["CARGO_TARGET_DIR"].as_deref(), Some(dir.join(super::QUEST_TARGET_DIR).to_str().unwrap()));
        let grants = super::both_spellings(&dir);
        assert!(grants.contains(&dir) && grants.contains(&std::fs::canonicalize(&dir).unwrap()));

        let mut claude = json!({ "sandbox": { "filesystem": { "allowWrite": [] }, "network": {} } });
        crate::claude::sandbox::allow_private_dir(&mut claude, &dir);
        let listed = |value: &Value| -> Vec<String> { value.as_array().unwrap().iter().filter_map(|path| path.as_str().map(String::from)).collect() };
        let granted: Vec<String> = grants.iter().map(|grant| grant.display().to_string()).collect();
        assert_eq!(listed(&claude["sandbox"]["filesystem"]["allowWrite"]), granted, "claude writes");
        assert_eq!(listed(&claude["sandbox"]["network"]["allowUnixSockets"]), granted, "claude sockets");

        let policy = crate::cursor::sandbox::policy(crate::claude::sandbox::Mode::Auto, &cwd, &[], Some(&dir)).unwrap();
        let cursor = listed(&policy["additionalReadwritePaths"]);
        assert!(granted.iter().all(|grant| cursor.contains(grant)), "cursor writes (its policy has no separate socket switch): {cursor:?}");

        let spec = crate::sandbox_wrap::Spec { working_dir: cwd.clone(), ..Default::default() };
        let wrapped = crate::sandbox_wrap::paths(&spec, &BTreeMap::new(), Some(&dir));
        let real = std::fs::canonicalize(&dir).unwrap();
        assert!(wrapped.writable.contains(&real) && wrapped.sockets.contains(&real), "omp and opencode wrapper: {wrapped:?}");
        let profile = crate::sandbox_wrap::seatbelt_profile(&wrapped);
        assert!(profile.contains(&format!("(allow network-bind (local unix-socket (subpath \"{}\")", real.display())), "{profile}");
        assert!(profile.contains(&format!("(allow network-outbound (remote unix-socket (subpath \"{}\")", real.display())), "{profile}");
        let binds = crate::sandbox_wrap::bwrap_args(&wrapped, |_| Some(true));
        assert!(binds.windows(2).any(|pair| pair[0] == "--bind" && pair[1] == real.as_os_str()), "{binds:?}");

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let roots = runtime.block_on(crate::codex::writable_roots(&cwd));
        assert!(grants.iter().all(|grant| roots.contains(grant)), "codex writes: {roots:?}");
        let (include, _) = super::shell_environment_patterns(&BTreeMap::new());
        for name in ["TMPDIR", "TMP", "TEMP", "CARGO_TARGET_DIR"] {
            assert!(include.iter().any(|kept| kept == name), "codex shells keep {name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
        let temp = child_env(&elsewhere).get("TMPDIR").cloned().flatten();
        assert_eq!(temp, std::env::var("TMPDIR").ok(), "a run outside a quest keeps the temp dir it inherits");
    }

    fn temp_vars(command: &tokio::process::Command) -> Vec<Option<String>> {
        ["TMPDIR", "TMP", "TEMP"].iter().map(|name| child_env(command).get(*name).cloned().flatten()).collect()
    }

    fn configured_in(session: Option<uuid::Uuid>, cwd: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("true");
        command.current_dir(cwd);
        let mut configure = || configure_child_environment(&mut command, &BTreeMap::new()).unwrap();
        match session {
            Some(id) => super::SESSION_TEMP.sync_scope(id, configure),
            None => configure(),
        }
        command
    }

    #[test]
    fn tmpdir_tmp_and_temp_all_name_the_same_private_folder() {
        let quest = uuid::Uuid::new_v4();
        let session = uuid::Uuid::new_v4();
        let in_quest = temp_vars(&configured_in(None, &format!("/r/workspaces/tasks/{quest}/world")));
        let in_folder = temp_vars(&configured_in(Some(session), "/Users/a/Projects/app"));
        let quest_dir = super::private_temp_root().join(super::quest_temp_name(quest)).to_string_lossy().to_string();
        let session_dir = super::private_temp_root().join(super::session_temp_name(session)).to_string_lossy().to_string();
        assert_eq!(in_quest, vec![Some(quest_dir.clone()); 3]);
        assert_eq!(in_folder, vec![Some(session_dir.clone()); 3]);
        let _ = std::fs::remove_dir_all(quest_dir);
        let _ = std::fs::remove_dir_all(session_dir);
    }

    #[test]
    fn folder_and_quick_sessions_get_one_folder_each_that_a_resume_keeps() {
        let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let temp_of = |id: uuid::Uuid, cwd: &str| temp_vars(&configured_in(Some(id), cwd))[0].clone().expect("session temp dir");
        let one = temp_of(first, "/Users/a/Projects/app");
        let two = temp_of(second, "/Users/a/Projects/app");
        assert_ne!(one, two, "two sessions in one folder never share a temp dir");
        assert_eq!(temp_of(first, "/Users/a/Projects/app"), one, "a resumed session keeps its folder");
        assert_eq!(temp_of(first, "/Users/a/Projects/app/src"), one);
        assert!(one.ends_with(&super::session_temp_name(first)) && std::path::Path::new(&one).is_dir());
        let owner = super::private_temp_owner(std::path::Path::new(&one)).expect("owner marker");
        assert_eq!(owner.0, first);
        assert!(super::private_temp_owner(&super::private_temp_root().join(super::session_temp_name(second)).join("nested")).is_none());
        assert!(child_env(&configured_in(Some(first), "/Users/a/Projects/app")).get("CARGO_TARGET_DIR").cloned().flatten().is_none(), "a person's own folder keeps its own build output");
        let _ = std::fs::remove_dir_all(one);
        let _ = std::fs::remove_dir_all(two);
    }

    #[test]
    fn a_quest_temp_folder_names_its_quest_and_runner_so_it_is_found_without_its_checkout() {
        let quest = uuid::Uuid::new_v4();
        let root = std::path::Path::new("/Users/a/Library/Application Support/dev.ygpt.loka/runner");
        let working_dir = root.join("workspaces").join("tasks").join(quest.to_string()).join("world");
        assert_eq!(super::quest_place(&working_dir), Some((quest, root.to_path_buf())));
        assert_eq!(super::quest_place(&root.join("workspaces").join("tasks").join(quest.to_string()).join("world-2").join("src")), Some((quest, root.to_path_buf())));
        assert_eq!(super::quest_place(std::path::Path::new(&format!("tasks/{quest}/app"))).map(|(id, _)| id), Some(quest));
        let target = super::prepare_quest_temp_dir(&working_dir).unwrap().expect("quest build folder");
        let folder = target.parent().unwrap().to_path_buf();
        assert_eq!(super::private_temp_owner(&folder), Some((quest, root.to_path_buf())));
        let other = super::private_temp_root().join(super::quest_temp_name(uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&other).unwrap();
        std::fs::copy(folder.join(super::PRIVATE_TEMP_OWNER), other.join(super::PRIVATE_TEMP_OWNER)).unwrap();
        assert_eq!(super::private_temp_owner(&other), None, "a marker copied into another quest's folder names nobody");
        let _ = std::fs::remove_dir_all(folder);
        let _ = std::fs::remove_dir_all(other);
    }

    #[test]
    fn follow_ups_and_reviews_of_a_quest_use_the_quests_folder_even_inside_a_session() {
        let (quest, session) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let command = configured_in(Some(session), &format!("/r/workspaces/tasks/{quest}/world"));
        let expected = super::private_temp_root().join(super::quest_temp_name(quest)).to_string_lossy().to_string();
        assert_eq!(temp_vars(&command), vec![Some(expected.clone()); 3]);
        assert!(child_env(&command).get("CARGO_TARGET_DIR").cloned().flatten().is_some());
        let _ = std::fs::remove_dir_all(expected);
    }

    #[test]
    fn runs_with_no_quest_and_no_session_keep_the_shared_temp_dir() {
        let command = configured_in(None, "/Users/a/Projects/app");
        for (name, value) in ["TMPDIR", "TMP", "TEMP"].into_iter().zip(temp_vars(&command)) {
            if let Some(value) = value {
                assert_eq!(Some(value), std::env::var(name).ok(), "a run outside a quest keeps the {name} it inherits");
            }
        }
    }

    #[test]
    fn odd_paths_never_name_a_temp_dir_outside_the_temp_root() {
        let id = uuid::Uuid::new_v4();
        let temp = super::private_temp_root();
        for cwd in [
            format!("/r/workspaces/tasks/../../etc/{id}"),
            format!("/r/workspaces/tasks/{id}/../../../x"),
            format!("/r/workspaces/tasks/{}/world", id.to_string().to_uppercase()),
            format!("/r/workspaces/tasks/{{{id}}}/world"),
            "/r/workspaces/tasks/../x".to_string(),
            "/r/workspaces/tasks//world".to_string(),
            String::new(),
        ] {
            if let Some(dir) = super::private_temp_dir(std::path::Path::new(&cwd)) {
                assert_eq!(dir.parent(), Some(temp.as_path()), "{cwd}");
                let name = dir.file_name().unwrap().to_string_lossy().to_string();
                assert!(name.starts_with(super::QUEST_TEMP_PREFIX) && name.len() == super::QUEST_TEMP_PREFIX.len() + 12 && name.chars().skip(7).all(|c| c.is_ascii_hexdigit()), "{cwd}: {name}");
            }
        }
        let session = uuid::Uuid::new_v4();
        let dir = super::SESSION_TEMP.sync_scope(session, || super::private_temp_dir(std::path::Path::new("/r/workspaces/tasks/../x"))).unwrap();
        assert_eq!(dir.parent(), Some(temp.as_path()));
        assert!(dir.ends_with(super::session_temp_name(session)));
    }

    #[test]
    fn wrapped_clis_set_all_three_temp_variables_through_one_helper() {
        let mut command = tokio::process::Command::new("true");
        super::set_temp_env(&mut command, std::path::Path::new("/run/private"));
        assert_eq!(temp_vars(&command), vec![Some("/run/private".to_string()); 3]);
    }

    #[test]
    fn quest_runs_build_into_their_own_target_dir_inside_their_temp_dir() {
        let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let target_of = |id: uuid::Uuid, granted: &BTreeMap<String, String>| {
            let mut command = tokio::process::Command::new("true");
            command.current_dir(format!("/r/workspaces/tasks/{id}/world"));
            configure_child_environment(&mut command, granted).unwrap();
            let target = child_env(&command).get("CARGO_TARGET_DIR").cloned().flatten();
            let _ = std::fs::remove_dir_all(super::private_temp_root().join(super::quest_temp_name(id)));
            (target, command)
        };
        let none = BTreeMap::new();
        let (one, _) = target_of(first, &none);
        let (two, _) = target_of(second, &none);
        let expected = |id: uuid::Uuid| super::private_temp_root().join(super::quest_temp_name(id)).join("cargo-target").to_string_lossy().to_string();
        assert_eq!(one, Some(expected(first)));
        assert_eq!(two, Some(expected(second)));
        let granted = BTreeMap::from([("CARGO_TARGET_DIR".to_string(), "/granted".to_string())]);
        let (kept, _) = target_of(first, &granted);
        assert_eq!(kept.as_deref(), Some("/granted"));
        let mut elsewhere = tokio::process::Command::new("true");
        elsewhere.current_dir("/r/workspaces/agents/main");
        configure_child_environment(&mut elsewhere, &none).unwrap();
        assert!(child_env(&elsewhere).get("CARGO_TARGET_DIR").cloned().flatten().is_none(), "only quest runs get one");
    }

    #[test]
    fn quest_cargo_builds_skip_incremental_state_and_debug_info_unless_granted() {
        let id = uuid::Uuid::new_v4();
        let lean = |granted: &BTreeMap<String, String>, cwd: &str, name: &str| {
            let mut command = tokio::process::Command::new("true");
            command.current_dir(cwd);
            configure_child_environment(&mut command, granted).unwrap();
            child_env(&command).get(name).cloned().flatten()
        };
        let quest = format!("/r/workspaces/tasks/{id}/world");
        for name in ["CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG"] {
            assert_eq!(lean(&BTreeMap::new(), &quest, name).as_deref(), Some("0"), "{name}");
            let granted = BTreeMap::from([(name.to_string(), "1".to_string())]);
            assert_eq!(lean(&granted, &quest, name).as_deref(), Some("1"), "a granted {name} wins");
            assert!(lean(&BTreeMap::new(), "/r/workspaces/agents/main", name).is_none(), "only quest runs are lean");
        }
        let _ = std::fs::remove_dir_all(super::private_temp_root().join(super::quest_temp_name(id)));
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
        for name in ["WORLD_SECRETS_KEY", super::ROOM_ANSWER_ENV] {
            let mut command = tokio::process::Command::new("true");
            let environment = BTreeMap::from([(name.to_string(), "value".to_string())]);
            assert!(configure_child_environment(&mut command, &environment).is_err(), "{name}");
        }
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

    #[test]
    fn gates_list_the_browser_socket_and_launcher_alias_in_both_spellings() {
        let gates = super::gate_fixture::listening();
        assert!(!gates.socket.starts_with(&gates.base), "a runner root with a space moves the socket into the temp folder: {}", gates.socket.display());
        for path in [&gates.socket, &gates.alias] {
            for spelling in [path.clone(), std::fs::canonicalize(path).unwrap()] {
                assert!(gates.paths.readable.contains(&spelling), "{} missing in {:?}", spelling.display(), gates.paths.readable);
            }
        }
        assert_eq!(gates.paths.sockets, super::both_spellings(&gates.socket));
        assert!(!gates.paths.sockets.contains(&gates.alias));
        assert_eq!(super::gate_paths_for(None, None), super::GatePaths::default());
    }

    #[test]
    fn both_spellings_name_a_file_that_is_not_there_yet() {
        let dir = std::env::temp_dir();
        let missing = dir.join(format!("loka-missing-{}.sock", uuid::Uuid::new_v4().simple()));
        let spellings = super::both_spellings(&missing);
        assert_eq!(spellings[0], missing);
        assert_eq!(spellings.last().unwrap(), &std::fs::canonicalize(&dir).unwrap().join(missing.file_name().unwrap()));
    }

    #[test]
    fn a_launcher_inside_an_app_folder_with_spaces_is_still_one_shell_word() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("loka-launcher-{}", uuid::Uuid::new_v4().simple()));
        let temp = base.join("temp");
        std::fs::create_dir_all(&temp).unwrap();
        let plain = base.join("bin").join("loka-agent");
        let spaced = base.join("Application Support").join("Loka.app").join("Contents").join("MacOS").join("loka-agent");
        for exe in [&plain, &spaced] {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, "#!/bin/sh\necho \"ran $1 $2\"\n").unwrap();
            std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(super::shell_safe_exe(&plain, &temp).as_deref(), plain.to_str());
        assert!(std::fs::read_dir(&temp).unwrap().next().is_none(), "a plain path needs no alias");
        let launcher = super::shell_safe_exe(&spaced, &temp).unwrap();
        assert_eq!(super::shell_word(Path::new(&launcher)).as_deref(), Some(launcher.as_str()));
        assert!(launcher.starts_with(temp.to_str().unwrap()), "{launcher}");
        assert_eq!(std::fs::read_link(&launcher).unwrap(), spaced);
        let ran = std::process::Command::new("/bin/sh").arg("-c").arg(format!("{launcher} browser abc")).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "ran browser abc\n");
        assert_eq!(super::shell_safe_exe(&spaced, &temp).as_deref(), Some(launcher.as_str()), "stable across calls");
        std::fs::remove_file(&launcher).unwrap();
        std::os::unix::fs::symlink(&plain, &launcher).unwrap();
        assert_eq!(super::shell_safe_exe(&spaced, &temp).as_deref(), Some(launcher.as_str()));
        assert_eq!(std::fs::read_link(&launcher).unwrap(), spaced, "a stale alias is pointed back at the app");
        let spaced_temp = base.join("temp with space");
        std::fs::create_dir_all(&spaced_temp).unwrap();
        assert_eq!(super::shell_safe_exe(&spaced, &spaced_temp), None, "an alias that would need quoting is never offered");
        std::fs::remove_dir_all(&base).unwrap();
    }

    use super::{
        collect_stderr, session_id_from_init, summarize_tool_output, JobUid,
        FREE_JOB_UIDS, JOB_UID,
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
    async fn every_cli_that_cannot_find_its_session_is_told_apart_from_other_failures() {
        for (backend, says, wrap, id) in [
            ("claude", "No conversation found with session ID: 9f1c2e4a-0000-4000-8000-000000000001", "Claude Code exited with code 1: {}", "9f1c2e4a-0000-4000-8000-000000000001"),
            ("codex", "Error: thread/resume: thread/resume failed: no rollout found for thread id 019a0000-1111-7222-8333-444455556666 (code -32600)", "Codex CLI exited with code 1: {}", "019a0000-1111-7222-8333-444455556666"),
            ("omp", "Error: Session \"01a21d98-c1f1-7311-ba20-3dde5fef60fa\" not found.\nRun `omp --resume` without an argument to pick from recent sessions, or `omp` to start a new one.", "{}: omp exited before it was ready", "01a21d98-c1f1-7311-ba20-3dde5fef60fa"),
            ("cursor", "Error: Session \"5d1e0c3a-aaaa-4bbb-8ccc-dddddddddddd\" not found", "Cursor Agent exited with code 1: {}", "5d1e0c3a-aaaa-4bbb-8ccc-dddddddddddd"),
            ("opencode", "Error: Session not found: ses_7Xy9AbCdEf", "opencode CLI exited with code 1: {}", "ses_7Xy9AbCdEf"),
        ] {
            let mut child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg("printf '%s\\n' \"$SAYS\" >&2; exit 1")
                .env("SAYS", says)
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let tail = super::stderr_tail(child.stderr.take(), super::Redactor::default());
            assert!(!child.wait().await.unwrap().success());
            tokio::time::sleep(Duration::from_millis(200)).await;
            let printed = tail.lock().unwrap().trim().to_string();
            assert_eq!(printed, says, "{backend}");
            let error = wrap.replace("{}", &printed);
            assert!(loka_shared::cli_errors::is_session_missing(&error), "{backend}: {error}");
            assert_eq!(loka_shared::cli_errors::lost_session_id(&error).as_deref(), Some(id), "{backend}");
            let other = wrap.replace("{}", "Error: 401 invalid api key");
            assert!(!loka_shared::cli_errors::is_session_missing(&other), "{backend}");
        }
    }

    #[tokio::test]
    async fn a_secret_at_the_start_of_the_stderr_window_is_not_left_in_pieces() {
        let secret = "sk-live-0123456789abcdef";
        for kept_after in [2usize, 11, 23, 24, 30] {
            let junk = 4000 - kept_after;
            let text = format!("{}{secret}{}", "j".repeat(junk), "k".repeat(kept_after));
            let mut child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg("printf '%s' \"$TEXT\" >&2")
                .env("TEXT", &text)
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let tail = super::stderr_tail(child.stderr.take(), super::Redactor::new([secret]));
            child.wait().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let shown = tail.lock().unwrap().clone();
            for start in 0..=secret.len() - loka_shared::redaction::MIN_SECRET_BYTES {
                assert!(!shown.contains(&secret[start..start + loka_shared::redaction::MIN_SECRET_BYTES]), "kept_after {kept_after}: a piece of the secret is in the stderr tail");
            }
        }
    }

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
    fn events_are_redacted_before_they_are_cut() {
        let secret = "sk-live-0123456789abcdef";
        let redactor = super::Redactor::new([secret]);
        let output = format!("{}{secret}", "a".repeat(3000 - 6));
        let command = format!("{}{secret}", "b".repeat(super::TOOL_INPUT_MAX_CHARS - 6));
        let super::StreamEvent::ToolResult { output: shown, .. } =
            super::StreamEvent::ToolResult { tool: "Bash".into(), output }.redacted_clip(&redactor, 3000)
        else {
            panic!("result expected")
        };
        let super::StreamEvent::ToolUse { input_summary, .. } =
            super::StreamEvent::ToolUse { tool: "Bash".into(), input_summary: command }.redacted_clip(&redactor, 4000)
        else {
            panic!("use expected")
        };
        assert_eq!(shown.chars().count(), 3000);
        assert!(shown.ends_with("[REDAC"));
        assert!(input_summary.ends_with("[REDAC"));
        assert!(!shown.contains("sk-") && !input_summary.contains("sk-"));
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

pub fn compacts_first(compact_first: bool, opts: &CliOptions<'_>) -> bool {
    resumes_to_compact(compact_first, opts.resume, opts.fork, opts.session_id.is_some(), opts.rewind_to.is_some())
}

pub fn resumes_to_compact(compact_first: bool, resume: bool, fork: bool, has_session: bool, rewinding: bool) -> bool {
    compact_first && resume && !fork && has_session && !rewinding
}

pub async fn run_compacting(backend: &str, opts: CliOptions<'_>, compact_first: bool) -> Result<CliResult> {
    if compacts_first(compact_first, &opts) {
        let compaction = CliOptions {
            working_dir: opts.working_dir,
            prompt: COMPACT_PROMPT,
            system_prompt: opts.system_prompt,
            allowed_tools: opts.allowed_tools,
            max_turns: 1,
            model: opts.model,
            mcp_config_json: opts.mcp_config_json.clone(),
            session_id: opts.session_id,
            resume: true,
            event_tx: None,
            image_paths: Vec::new(),
            fork: false,
            lean: opts.lean,
            effort: opts.effort,
            fast: opts.fast,
            environment: opts.environment.clone(),
            mcp_has_secrets: opts.mcp_has_secrets,
            sensitive: opts.sensitive,
            rewind_to: None,
            approval: opts.approval,
            steer: SteerSlot::default(),
            omp_models: opts.omp_models.clone(),
            provider_keys: opts.provider_keys.clone(),
            readable_files: opts.readable_files.clone(),
        };
        let compacted = match run(backend, compaction).await {
            Ok(compacted) => {
                info!(backend, "Compacted the session before sending the next request");
                Some(compacted)
            }
            Err(error) => {
                warn!(backend, error = %loka_shared::cli_errors::chain(&error), "Compacting before the next request failed, sending it anyway");
                None
            }
        };
        return run(backend, opts).await.map(|result| match compacted {
            Some(compacted) => with_compaction(backend, result, &compacted),
            None => result,
        });
    }
    run(backend, opts).await
}

pub fn with_compaction(backend: &str, mut result: CliResult, compaction: &CliResult) -> CliResult {
    if loka_shared::backends::cost_per_run(backend) {
        result.cost_usd += compaction.cost_usd;
        result.usage.input_tokens += compaction.usage.input_tokens;
        result.usage.cached_input_tokens += compaction.usage.cached_input_tokens;
        result.usage.output_tokens += compaction.usage.output_tokens;
    }
    result
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
pub(crate) mod gate_fixture {
    use std::path::PathBuf;

    pub(crate) struct Gates {
        pub base: PathBuf,
        pub socket: PathBuf,
        pub alias: PathBuf,
        pub paths: super::GatePaths,
        pub listener: std::os::unix::net::UnixListener,
    }

    impl Drop for Gates {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket);
            if let Some(dir) = self.socket.parent() {
                let _ = std::fs::remove_dir(dir);
            }
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    pub(crate) fn listening() -> Gates {
        let base = std::env::temp_dir().join(format!("loka-gates-{}", uuid::Uuid::new_v4().simple()));
        let root = base.join("Application Support").join("runner");
        let alias = base.join("alias");
        std::fs::create_dir_all(root.join("leases")).unwrap();
        std::fs::create_dir_all(&alias).unwrap();
        let socket = crate::browser::socket_for(&root).unwrap();
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let paths = super::gate_paths_for(Some(&root), Some(alias.clone()));
        Gates { base, socket, alias, paths, listener }
    }
}

#[cfg(test)]
mod parent_pid_tests {
    #[test]
    fn parent_pid_matches_getppid() {
        assert_eq!(super::parent_pid(std::process::id()), Some(unsafe { libc::getppid() } as u32));
        assert_eq!(super::parent_pid(u32::MAX - 7), None);
    }
}

#[cfg(test)]
pub(crate) mod straddle {
    use super::{Redactor, StreamEvent};

    pub const SECRET: &str = "sk-live-0123456789abcdef";

    pub fn around(pad: usize) -> String {
        format!("{}{SECRET}{}", "x".repeat(pad), "z".repeat(40))
    }

    pub fn pads() -> Vec<usize> {
        [super::TOOL_INPUT_MAX_CHARS, 200, 300, 2_000, 3_000, 4_000]
            .into_iter()
            .flat_map(|limit| limit - SECRET.len() - 1..=limit + 1)
            .collect()
    }

    pub fn texts(events: &[StreamEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolUse { input_summary, .. } => Some(input_summary.as_str()),
                StreamEvent::ToolResult { output, .. } => Some(output.as_str()),
                StreamEvent::AssistantText(text) | StreamEvent::Error(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn assert_whole_then_hidden(events: Vec<StreamEvent>, expected: usize, label: &str, pad: usize) {
        let whole = texts(&events).into_iter().filter(|text| text.contains(SECRET)).count();
        assert_eq!(whole, expected, "{label} cut the text before redaction at pad {pad}");
        let redactor = Redactor::new([SECRET]);
        for limit in [2_000, 4_000] {
            for event in events.iter().cloned() {
                let shown = event.redacted_clip(&redactor, limit);
                for text in texts(std::slice::from_ref(&shown)) {
                    for start in 0..=SECRET.len() - loka_shared::redaction::MIN_SECRET_BYTES {
                        assert!(!text.contains(&SECRET[start..start + loka_shared::redaction::MIN_SECRET_BYTES]), "{label} leaked a piece of the secret at pad {pad}, limit {limit}");
                    }
                }
            }
        }
    }

    pub fn drain(mut receiver: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }
}
