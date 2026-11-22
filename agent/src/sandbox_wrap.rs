use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::warn;

use crate::claude::sandbox::{self, Mode};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const BWRAP: &str = "bwrap";

#[derive(Default, Clone)]
pub(crate) struct Spec {
    pub working_dir: PathBuf,
    pub read_only: bool,
    pub readable: Vec<PathBuf>,
    pub state: Vec<PathBuf>,
}

pub(crate) struct RunTemp(PathBuf);

impl Drop for RunTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Paths {
    pub hidden: Vec<PathBuf>,
    pub readable: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub owner_temp: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Seatbelt,
    Bubblewrap,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn expand(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => home.map(|home| home.join(rest)),
        None => Some(PathBuf::from(path)),
    }
}

fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if parent != path => resolve(parent).join(name),
        _ => path.to_path_buf(),
    }
}

fn dedup(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn paths(spec: &Spec, granted: &BTreeMap<String, String>, run_temp: Option<&Path>) -> Paths {
    let home = home();
    let mut hidden: Vec<PathBuf> = sandbox::HIDDEN_PATHS.iter().filter_map(|p| expand(p, home.as_deref())).collect();
    hidden.extend(home.as_ref().map(|home| home.join(sandbox::OWNER_SSH)));
    let hub_runs_servers = crate::mcp_hub::get().is_some();
    hidden.extend(crate::cli::runner_private_paths().into_iter().filter(|path| hub_runs_servers || !path.ends_with(crate::cli::MCP_AUTH_DIR)));
    for name in crate::cli::CREDENTIAL_FILE_ENV {
        for value in [granted.get(*name).map(OsString::from), std::env::var_os(name)].into_iter().flatten() {
            if !value.is_empty() {
                hidden.push(PathBuf::from(value));
            }
        }
    }
    hidden.extend(std::env::var_os("WORLD_SLACK_KEY_FILE").filter(|v| !v.is_empty()).map(PathBuf::from));
    hidden.extend(sandbox::app_session_file());
    let temp = std::env::temp_dir();
    if !home.as_deref().is_some_and(|home| resolve(home).starts_with(resolve(&temp))) {
        hidden.push(temp);
    }
    let mut readable: Vec<PathBuf> = sandbox::owner_ssh_readable(home.as_deref());
    readable.extend(spec.readable.iter().cloned());
    readable.push(spec.working_dir.clone());
    let git_roots: Vec<PathBuf> = sandbox::git_write_roots(&spec.working_dir).into_iter().map(PathBuf::from).collect();
    readable.extend(git_roots.iter().cloned());
    readable.extend(crate::mcp_hub::get().map(|hub| hub.socket().to_path_buf()));
    readable.extend(std::env::var_os("SSH_AUTH_SOCK").filter(|v| !v.is_empty()).map(PathBuf::from));
    let mut writable: Vec<PathBuf> = spec.state.iter().cloned().chain(run_temp.map(Path::to_path_buf)).collect();
    if !spec.read_only {
        writable.push(spec.working_dir.clone());
        writable.extend(git_roots);
        writable.extend(home.as_deref().map(sandbox::writable_in).unwrap_or_default());
        writable.extend(home.iter().flat_map(|home| sandbox::OWNER_WRITABLE_HOME.iter().map(|rel| home.join(rel))));
    }
    let owner_temp = if spec.read_only { Vec::new() } else { sandbox::user_temp_dirs() };
    let hidden = dedup(hidden.iter().map(|p| resolve(p)).collect());
    let writable = dedup(writable.iter().map(|p| resolve(p)).collect());
    Paths { hidden, readable: dedup(readable.iter().map(|p| resolve(p)).collect()), writable, owner_temp: dedup(owner_temp) }
}

fn quote(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{text}\"")
}

fn regex_escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| if "\\.^$|?*+()[]{}\"".contains(c) { vec!['\\', c] } else { vec![c] })
        .collect()
}

fn subpaths(path: &Path) -> String {
    let mut spellings = vec![format!("(subpath {})", quote(path))];
    for root in ["/private/var", "/private/tmp", "/private/etc"] {
        if let Ok(rest) = path.strip_prefix(root) {
            spellings.push(format!("(subpath {})", quote(&Path::new(&root["/private".len()..]).join(rest))));
        }
    }
    spellings.join(" ")
}

const DEV_WRITES: &str = "^/dev/(null|zero|random|urandom|stdin|stdout|stderr|ptmx|dtracehelper|autofs_nowait|tty[^/]*|fd/[0-9]+)$";

pub(crate) fn seatbelt_profile(paths: &Paths) -> String {
    let mut rules = vec!["(version 1)".to_string(), "(allow default)".to_string(), "(deny file-write* (subpath \"/\"))".to_string()];
    rules.push(format!("(allow file-write* (regex #\"{DEV_WRITES}\"))"));
    for path in &paths.writable {
        rules.push(format!("(allow file-write* {})", subpaths(path)));
    }
    for path in &paths.hidden {
        let reopened = paths.writable.iter().chain(&paths.readable).any(|p| p.starts_with(path) && p != path);
        let reads = if reopened { "file-read*" } else { "file-read-data" };
        rules.push(format!("(deny {reads} file-write* {})", subpaths(path)));
    }
    for path in &paths.readable {
        rules.push(format!("(allow file-read* {})", subpaths(path)));
    }
    for path in paths.writable.iter().filter(|w| paths.hidden.iter().any(|h| w.starts_with(h))) {
        rules.push(format!("(allow file-read* file-write* {})", subpaths(path)));
    }
    let mut parents: Vec<PathBuf> = Vec::new();
    for path in paths.writable.iter().chain(&paths.readable) {
        for hidden in paths.hidden.iter().filter(|h| path.starts_with(h) && path != *h) {
            parents.extend(path.ancestors().skip(1).take_while(|a| a.starts_with(hidden)).map(Path::to_path_buf));
        }
    }
    for parent in dedup(parents) {
        rules.push(format!("(allow file-read* {})", subpaths(&parent).replace("(subpath ", "(literal ")));
    }
    for hidden in paths.hidden.iter().filter(|h| h.ends_with("T")) {
        for spelling in [hidden.display().to_string(), hidden.display().to_string().trim_start_matches("/private").to_string()] {
            rules.push(format!("(allow file-read* file-write* (regex #\"^{}/xcrun_db\"))", regex_escape(&spelling)));
        }
    }
    for dir in &paths.owner_temp {
        for name in sandbox::OWNER_TEMP_NAMES {
            let pattern = name.split('*').map(regex_escape).collect::<Vec<_>>().join(".*");
            for spelling in [dir.display().to_string(), dir.display().to_string().trim_start_matches("/private").to_string()] {
                rules.push(format!("(allow file-read* file-write* (regex #\"^{}/{pattern}\"))", regex_escape(&spelling)));
            }
        }
    }
    rules.join("\n")
}

pub(crate) fn bwrap_args(paths: &Paths, exists: impl Fn(&Path) -> Option<bool>) -> Vec<OsString> {
    let mut args: Vec<OsString> =
        ["--die-with-parent", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"].iter().map(OsString::from).collect();
    let mut push = |flag: &str, from: &Path, to: &Path| {
        args.push(flag.into());
        args.push(from.into());
        if !to.as_os_str().is_empty() {
            args.push(to.into());
        }
    };
    let under_hidden = |path: &Path| paths.hidden.iter().any(|h| path.starts_with(h));
    for path in paths.writable.iter().filter(|p| !under_hidden(p)) {
        if exists(path).is_some() {
            push("--bind", path, path);
        }
    }
    for path in &paths.hidden {
        match exists(path) {
            Some(true) => push("--tmpfs", path, Path::new("")),
            Some(false) => push("--ro-bind", Path::new("/dev/null"), path),
            None => {}
        }
    }
    for path in paths.readable.iter().filter(|p| under_hidden(p)) {
        if exists(path).is_some() {
            push("--ro-bind", path, path);
        }
    }
    for path in paths.writable.iter().filter(|p| under_hidden(p)) {
        if exists(path).is_some() {
            push("--bind", path, path);
        }
    }
    args
}

fn on_disk(path: &Path) -> Option<bool> {
    std::fs::metadata(path).ok().map(|m| m.is_dir())
}

fn available() -> Option<Kind> {
    static FOUND: OnceLock<Option<Kind>> = OnceLock::new();
    *FOUND.get_or_init(|| {
        if cfg!(target_os = "macos") {
            return Path::new(SANDBOX_EXEC).is_file().then_some(Kind::Seatbelt);
        }
        let probe = crate::cli::job_std_command(BWRAP)
            .args(["--die-with-parent", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(probe, Ok(status) if status.success()).then_some(Kind::Bubblewrap)
    })
}

fn split_job_prefix(std: &std::process::Command) -> (Vec<OsString>, OsString, Vec<OsString>) {
    let args: Vec<OsString> = std.get_args().map(OsString::from).collect();
    if std.get_program() == "setpriv" {
        if let Some(end) = args.iter().position(|a| a == "--") {
            if let Some(program) = args.get(end + 1) {
                return (args[..end].to_vec(), program.clone(), args[end + 2..].to_vec());
            }
        }
    }
    (Vec::new(), std.get_program().to_owned(), args)
}

fn rebuild(original: &tokio::process::Command, wrapper: &str, wrapper_args: Vec<OsString>) -> tokio::process::Command {
    let std = original.as_std();
    let (_, program, args) = split_job_prefix(std);
    let mut cmd = crate::cli::job_command(wrapper);
    cmd.args(wrapper_args).arg(program).args(args);
    cmd.env_clear();
    for (name, value) in std.get_envs() {
        if let Some(value) = value {
            cmd.env(name, value);
        }
    }
    if let Some(dir) = std.get_current_dir() {
        cmd.current_dir(dir);
    }
    cmd.kill_on_drop(true);
    cmd
}

fn run_temp() -> Result<RunTemp> {
    let dir = crate::cli::temp_dir().join(format!("loka-sandbox-tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).context("Failed to create the sandbox temp dir")?;
    crate::cli::give_to_job_user(&dir)?;
    Ok(RunTemp(dir))
}

pub(crate) fn unavailable_error(backend: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{backend} needs an OS sandbox here, but {SANDBOX_EXEC} is missing")
    } else {
        format!("{backend} needs an OS sandbox here, but bwrap is missing or cannot create namespaces (install bubblewrap, or set WORLD_TASK_SANDBOX=off for trusted agents)")
    }
}

pub(crate) fn wrap(
    backend: &str,
    cmd: tokio::process::Command,
    spec: &Spec,
    granted: &BTreeMap<String, String>,
) -> Result<(tokio::process::Command, Option<RunTemp>)> {
    let mode = sandbox::mode();
    if mode == Mode::Off && !spec.read_only {
        return Ok((cmd, None));
    }
    let Some(kind) = available() else {
        if mode == Mode::Strict || spec.read_only {
            anyhow::bail!(unavailable_error(backend));
        }
        warn!(backend, "No OS sandbox is available here, running without one");
        return Ok((cmd, None));
    };
    let temp = run_temp()?;
    let paths = paths(spec, granted, Some(&temp.0));
    let mut wrapped = match kind {
        Kind::Seatbelt => rebuild(&cmd, SANDBOX_EXEC, vec!["-p".into(), seatbelt_profile(&paths).into()]),
        Kind::Bubblewrap => rebuild(&cmd, BWRAP, bwrap_args(&paths, on_disk).into_iter().chain([OsString::from("--")]).collect()),
    };
    wrapped.env("TMPDIR", &temp.0);
    crate::cli::drop_default_quest_target(&mut wrapped);
    Ok((wrapped, Some(temp)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Paths {
        Paths {
            hidden: vec![PathBuf::from("/home/a/.aws"), PathBuf::from("/home/a/.ssh"), PathBuf::from("/tmp")],
            readable: vec![PathBuf::from("/home/a/.ssh/known_hosts"), PathBuf::from("/tmp/run/config.json")],
            writable: vec![PathBuf::from("/work/repo"), PathBuf::from("/tmp/run")],
            owner_temp: Vec::new(),
        }
    }

    #[test]
    fn owner_runs_may_write_only_the_named_xcode_and_swift_temp_entries() {
        let temp = PathBuf::from("/private/var/folders/ab/cd/T");
        let owner = seatbelt_profile(&Paths { owner_temp: vec![temp.clone()], hidden: vec![temp.clone()], ..sample() });
        for name in ["xcrun_db", "TemporaryDirectory\\..*", "swift-.*", "com\\.apple\\.dt\\..*"] {
            assert!(owner.contains(&format!("(allow file-read* file-write* (regex #\"^/private/var/folders/ab/cd/T/{name}\"))")), "{name} missing in {owner}");
            assert!(owner.contains(&format!("(allow file-read* file-write* (regex #\"^/var/folders/ab/cd/T/{name}\"))")), "{name} missing in {owner}");
        }
        assert!(!owner.contains("(allow file-write* (subpath \"/private/var/folders/ab/cd/T\"))"));
        assert!(!owner.contains("loka-outbox"));
        let guest = seatbelt_profile(&Paths { hidden: vec![temp], ..sample() });
        assert!(!guest.contains("TemporaryDirectory"));
    }

    #[test]
    fn owner_temp_rules_reach_inside_the_named_entries_and_nowhere_else() {
        let temp = PathBuf::from("/private/var/folders/ab/cd/T");
        let owner = seatbelt_profile(&Paths { owner_temp: vec![temp.clone()], hidden: vec![temp], ..sample() });
        let rules: Vec<regex_lite::Regex> = owner
            .lines()
            .filter_map(|line| line.strip_prefix("(allow file-read* file-write* (regex #\"")?.strip_suffix("\"))"))
            .map(|pattern| regex_lite::Regex::new(pattern).unwrap())
            .collect();
        let allowed = |path: &str| rules.iter().any(|rule| rule.is_match(path));
        for root in ["/private/var/folders/ab/cd/T", "/var/folders/ab/cd/T"] {
            for inside in [
                "TemporaryDirectory.Ab12",
                "TemporaryDirectory.Ab12/a",
                "TemporaryDirectory.Ab12/SourcePackages/checkouts/x/Package.resolved",
                "swift-build-1/x/y",
                "com.apple.dt.XCBuild/a/b",
                "xcrun_db-abc",
            ] {
                assert!(allowed(&format!("{root}/{inside}")), "{root}/{inside} refused");
            }
            for outside in ["", "/x", "/loka-outbox/a", "/TemporaryDirectoryAb/a", "/other/TemporaryDirectory.a/b"] {
                assert!(!allowed(&format!("{root}{outside}")), "{root}{outside} allowed");
            }
        }
        assert!(!allowed("/private/var/folders/zz/yy/T/TemporaryDirectory.a/b"));
    }

    #[test]
    fn seatbelt_denies_hidden_paths_after_granting_writes() {
        let profile = seatbelt_profile(&sample());
        let pos = |needle: &str| profile.find(needle).unwrap_or_else(|| panic!("{needle} missing in {profile}"));
        assert!(pos("(deny file-write* (subpath \"/\"))") < pos("(allow file-write* (subpath \"/work/repo\"))"));
        assert!(pos("(allow file-write* (subpath \"/work/repo\"))") < pos("(deny file-read-data file-write* (subpath \"/home/a/.aws\"))"));
        assert!(pos("(deny file-read* file-write* (subpath \"/tmp\"))") < pos("(allow file-read* (subpath \"/tmp/run/config.json\"))"));
        assert!(pos("(deny file-read* file-write* (subpath \"/tmp\"))") < pos("(allow file-read* file-write* (subpath \"/tmp/run\"))"));
        assert!(pos("(deny file-read* file-write* (subpath \"/home/a/.ssh\"))") < pos("(allow file-read* (subpath \"/home/a/.ssh/known_hosts\"))"));
        assert!(!profile.contains("(allow file-read* (subpath \"/tmp\"))"));
        assert!(profile.contains("(allow file-read* (literal \"/tmp\"))"), "{profile}");
        assert!(!profile.contains("(allow file-write* (subpath \"/dev\"))"), "only device nodes tools need stay writable");
        assert!(profile.contains("(regex #\"^/dev/(null|"));
    }

    #[test]
    fn device_writes_cover_terminals_and_null_but_not_disks() {
        let dev = regex_lite::Regex::new(DEV_WRITES).unwrap();
        for ok in ["/dev/null", "/dev/ttys003", "/dev/tty", "/dev/fd/3", "/dev/ptmx", "/dev/urandom", "/dev/dtracehelper"] {
            assert!(dev.is_match(ok), "{ok}");
        }
        for no in ["/dev/disk0", "/dev/rdisk1s1", "/dev/fd/3/x", "/dev/auditpipe", "/dev/bpf0"] {
            assert!(!dev.is_match(no), "{no}");
        }
    }

    #[test]
    fn seatbelt_covers_both_spellings_of_private_dirs() {
        let paths = Paths { hidden: vec![PathBuf::from("/private/var/folders/x/T")], readable: vec![], writable: vec![], owner_temp: Vec::new() };
        assert!(seatbelt_profile(&paths).contains("(subpath \"/private/var/folders/x/T\") (subpath \"/var/folders/x/T\")"));
    }

    #[test]
    fn seatbelt_quotes_paths() {
        let paths = Paths { hidden: vec![PathBuf::from("/a\"b\\c")], readable: vec![], writable: vec![], owner_temp: Vec::new() };
        assert!(seatbelt_profile(&paths).contains("(subpath \"/a\\\"b\\\\c\")"));
    }

    #[test]
    fn bwrap_hides_then_reopens_nested_paths() {
        let exists = |p: &Path| match p.to_str().unwrap() {
            "/home/a/.ssh/known_hosts" | "/tmp/run/config.json" => Some(false),
            "/home/a/.aws" => None,
            _ => Some(true),
        };
        let args: Vec<String> = bwrap_args(&sample(), exists).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let joined = args.join(" ");
        assert!(joined.starts_with("--die-with-parent --ro-bind / / --dev /dev --proc /proc"));
        let pos = |needle: &str| joined.find(needle).unwrap_or_else(|| panic!("{needle} missing in {joined}"));
        assert!(pos("--bind /work/repo /work/repo") < pos("--tmpfs /tmp"));
        assert!(pos("--tmpfs /tmp") < pos("--ro-bind /tmp/run/config.json /tmp/run/config.json"));
        assert!(pos("--tmpfs /tmp") < pos("--bind /tmp/run /tmp/run"));
        assert!(pos("--tmpfs /home/a/.ssh") < pos("--ro-bind /home/a/.ssh/known_hosts"));
        assert!(!joined.contains(".aws"), "missing paths are skipped: {joined}");
    }

    #[test]
    fn read_only_runs_write_only_their_state() {
        let dir = std::env::temp_dir().join(format!("wrap-ro-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state");
        let spec = Spec { working_dir: dir.join("work"), read_only: true, state: vec![state.clone()], ..Default::default() };
        let paths = paths(&spec, &BTreeMap::new(), None);
        assert!(paths.writable.iter().all(|p| p.ends_with("state")), "{:?}", paths.writable);
        let spec = Spec { read_only: false, ..spec };
        let paths = super::paths(&spec, &BTreeMap::new(), None);
        assert!(paths.writable.iter().any(|p| p.ends_with("work")));
        assert!(paths.hidden.iter().any(|p| p.ends_with(".aws")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn task_sandbox_hides_local_settings_and_the_app_session() {
        std::env::set_var("LOKA_APP_SESSION_FILE", "/run/loka-session/app-session.key");
        let spec = Spec { working_dir: std::env::temp_dir(), read_only: false, readable: Vec::new(), state: Vec::new() };
        let paths = paths(&spec, &BTreeMap::new(), None);
        assert!(paths.hidden.iter().any(|p| p.ends_with("dev.ygpt.loka/local.json")));
        assert!(paths.hidden.iter().any(|p| p.ends_with("dev.ygpt.loka/app-session.key")));
        assert!(paths.hidden.iter().any(|p| p.ends_with("loka-session/app-session.key")));
        let profile = seatbelt_profile(&paths);
        assert!(profile.contains("dev.ygpt.loka/local.json\"))"), "{profile}");
        assert!(profile.contains("loka-session/app-session.key\"))"), "{profile}");
    }

    #[test]
    fn owner_runs_read_only_signing_keys_in_ssh_and_keep_teleport() {
        let spec = Spec { working_dir: std::env::temp_dir(), read_only: false, readable: Vec::new(), state: Vec::new() };
        let owner = paths(&spec, &BTreeMap::new(), None);
        assert!(owner.hidden.iter().any(|p| p.ends_with(".ssh")));
        assert!(owner.readable.iter().any(|p| p.ends_with(".ssh/loka_signing")));
        assert!(owner.readable.iter().any(|p| p.ends_with(".ssh/known_hosts")));
        assert!(!owner.readable.iter().any(|p| p.ends_with(".ssh/id_ed25519") || p.ends_with(".ssh")));
        assert!(owner.writable.iter().any(|p| p.ends_with(".ssh/known_hosts")));
        assert!(owner.writable.iter().any(|p| p.ends_with(".tsh")));
        assert!(owner.hidden.iter().any(|p| p.ends_with(".aws")));
    }

    #[test]
    fn job_user_prefix_is_kept_outside_the_sandbox() {
        let mut std = std::process::Command::new("setpriv");
        std.args(["--reuid=2000", "--regid=1000", "--clear-groups", "--", "omp", "--print"]);
        let (prefix, program, args) = split_job_prefix(&std);
        assert_eq!(prefix.len(), 3);
        assert_eq!(program, "omp");
        assert_eq!(args, vec![OsString::from("--print")]);
        let plain = std::process::Command::new("opencode");
        assert_eq!(split_job_prefix(&plain).1, "opencode");
    }


    const ECHO_SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    method = m["method"]
    if method == "initialize":
        r = {"protocolVersion": m["params"].get("protocolVersion", "2024-11-05"), "capabilities": {"tools": {}}, "serverInfo": {"name": "probe", "version": "1"}}
    elif method == "tools/list":
        r = {"tools": [{"name": "secret_word", "description": "Returns the secret word", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        open(sys.argv[1], "a").write("called\n")
        r = {"content": [{"type": "text", "text": "banana42"}], "isError": False}
    else:
        r = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}) + "\n")
    sys.stdout.flush()
"#;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("wrap-{name}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn live_backend() -> (String, String) {
        (
            std::env::var("PARITY_BACKEND").expect("set PARITY_BACKEND to omp or opencode"),
            std::env::var("PARITY_MODEL").expect("set PARITY_MODEL"),
        )
    }

    async fn task(
        work: &Path,
        prompt: &str,
        mcp: Option<String>,
        mut environment: BTreeMap<String, String>,
        approval: crate::cli::Approval,
        pids: crate::mcp_hub::Pids,
    ) -> crate::cli::CliResult {
        let (backend, model) = live_backend();
        let (events, mut seen) = tokio::sync::mpsc::unbounded_channel();
        if let Ok(key) = std::env::var("FAL_KEY") {
            environment.insert("FAL_KEY".into(), key);
        }
        let opts = crate::cli::CliOptions {
            working_dir: work,
            prompt,
            system_prompt: None,
            allowed_tools: None,
            max_turns: 8,
            model: &model,
            mcp_config_json: mcp,
            session_id: None,
            resume: false,
            event_tx: Some(events),
            image_paths: Vec::new(),
            fork: false,
            lean: false,
            effort: None,
            fast: false,
            environment,
            mcp_has_secrets: false,
            sensitive: false,
            rewind_to: None,
            approval,
            steer: Default::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files: Vec::new(),
        };
        let result = crate::cli::record_spawns(pids, crate::cli::run(&backend, opts)).await.unwrap();
        while let Ok(event) = seen.try_recv() {
            match event {
                crate::cli::StreamEvent::ToolUse { tool, input_summary } => eprintln!("WRAP tool {tool}: {input_summary}"),
                crate::cli::StreamEvent::ToolResult { tool, output } => eprintln!("WRAP result {tool}: {}", output.replace('\n', " | ")),
                _ => {}
            }
        }
        eprintln!("WRAP {backend} output={:?}", result.output.chars().take(1500).collect::<String>());
        result
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_sandboxed_task_reaches_the_mcp_hub() {
        let bin = std::env::var("LOKA_AGENT_BIN").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../target/debug/loka-agent").to_string());
        let hub_dir = scratch("hub");
        let work = scratch("hub-work");
        let script = hub_dir.0.join("echo.py");
        let marker = hub_dir.0.join("calls.txt");
        std::fs::write(&script, ECHO_SERVER).unwrap();
        let hub = crate::mcp_hub::start(hub_dir.0.join("hub.sock")).await.unwrap();
        let pids: crate::mcp_hub::Pids = Default::default();
        let agent = uuid::Uuid::new_v4();
        let mut servers = serde_json::Map::new();
        servers.insert(
            "probe".into(),
            serde_json::json!({ "command": "/usr/bin/python3", "args": ["-I", script.display().to_string(), marker.display().to_string()] }),
        );
        let prepared = hub
            .prepare(
                crate::mcp_hub::Job { isolation: crate::mcp_hub::isolation_for(None, agent), agent, cwd: &work.0, pids: pids.clone(), passthrough: &[] },
                &servers,
            )
            .await
            .unwrap();
        let mut mcp = serde_json::json!({ "mcpServers": prepared.servers });
        mcp["mcpServers"]["probe"]["command"] = serde_json::json!(bin);
        task(
            &work.0,
            "Call the probe secret_word tool once and reply with exactly what it returned. If MCP tools are shown to you as xd:// files, run it by writing {} to xd://mcp__probe_secret_word with your write tool.",
            Some(mcp.to_string()),
            BTreeMap::new(),
            Default::default(),
            pids,
        )
        .await;
        drop(prepared);
        assert!(std::fs::read_to_string(&marker).unwrap_or_default().contains("called"), "the sandboxed CLI reached the MCP server through the hub");
    }

    #[test]
    #[ignore]
    fn live_owner_seatbelt_signs_with_the_loka_key_and_pushes() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let key = std::env::var("SIGN_TEST_KEY").map(PathBuf::from).unwrap_or_else(|_| home.join(".ssh/loka_signing"));
        let remote = std::env::var("PUSH_TEST_REMOTE").unwrap_or_else(|_| "https://github.com/Warlord-K/loka.git".into());
        let over_https = remote.starts_with("https://");
        let token = std::env::var("GH_TOKEN").ok().filter(|token| !token.is_empty()).or_else(|| {
            let out = std::process::Command::new("gh").args(["auth", "token"]).output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|token| !token.is_empty())
        });
        assert!(!over_https || token.is_some(), "set GH_TOKEN or sign in with gh to push over https");
        let branch = format!("loka-sandbox-proof-{}", uuid::Uuid::new_v4().simple());
        let work = scratch("push-proof");
        let run_temp = scratch("push-proof-tmp");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git").arg("-C").arg(&work.0).args(args).status().unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.email=a@b", "-c", "user.name=a", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "init"]);
        let tree = work.0.join("tree");
        git(&["worktree", "add", "-q", tree.to_str().unwrap(), "-b", "task"]);
        let spec = Spec { working_dir: tree.clone(), read_only: false, readable: Vec::new(), state: Vec::new() };
        let profile = seatbelt_profile(&paths(&spec, &BTreeMap::new(), Some(&run_temp.0)));
        let checks: String = ["id_rsa", "id_ed25519", "id_ecdsa"]
            .iter()
            .filter(|name| home.join(".ssh").join(name).exists())
            .map(|name| format!("head -c 0 ~/.ssh/{name} 2>/dev/null && echo '{name} readable' || echo '{name} denied'\n"))
            .collect();
        let push = if over_https {
            format!("git -c credential.helper= -c 'credential.helper=!f() {{ echo username=x-access-token; echo \"password=$GH_TOKEN\"; }}; f' push -q {remote} HEAD:refs/heads/{branch}")
        } else {
            format!("GIT_SSH_COMMAND=\"ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=$TMPDIR/known_hosts\" git push -q {remote} HEAD:refs/heads/{branch}")
        };
        let script = format!(
            "git -c user.email=a@b -c user.name=a -c gpg.format=ssh -c user.signingkey={key}.pub commit -q --allow-empty -S -m signed && echo \"signed: $(git cat-file -p HEAD | grep -c '^gpgsig')\"\n\
             {checks}\
             {push} 2>&1 && echo \"pushed: $(git rev-parse --short HEAD) -> {branch}\"\n",
            key = key.display(),
        );
        let mut command = std::process::Command::new(SANDBOX_EXEC);
        command.current_dir(&tree).env("TMPDIR", &run_temp.0).env("GIT_TERMINAL_PROMPT", "0");
        if let Some(token) = &token {
            command.env("GH_TOKEN", token);
        }
        let out = command.args(["-p", &profile, "sh", "-c", &script]).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let landed = std::process::Command::new("git").arg("-C").arg(&tree).args(["ls-remote", &remote, &format!("refs/heads/{branch}")]).env("GH_TOKEN", token.clone().unwrap_or_default()).output().unwrap();
        let head = std::process::Command::new("git").arg("-C").arg(&tree).args(["rev-parse", "HEAD"]).output().unwrap();
        let landed = String::from_utf8_lossy(&landed.stdout).starts_with(String::from_utf8_lossy(&head.stdout).trim());
        if text.contains("pushed:") {
            let mut delete = std::process::Command::new("git");
            delete.arg("-C").arg(&tree).env("GH_TOKEN", token.clone().unwrap_or_default()).env("GIT_TERMINAL_PROMPT", "0");
            if over_https {
                delete.args(["-c", "credential.helper=", "-c", "credential.helper=!f() { echo username=x-access-token; echo \"password=$GH_TOKEN\"; }; f"]);
            }
            let _ = delete.args(["push", "-q", "--delete", &remote, &branch]).status();
        }
        println!("{text}landed on remote: {landed}");
        assert!(text.contains("signed: 1"), "the karna key could not sign inside the sandbox: {text}");
        assert!(!text.contains("readable"), "a private key in ~/.ssh was readable: {text}");
        assert!(text.contains("pushed:") && landed, "the sandboxed push did not land: {text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_sandboxed_task_commits_but_cannot_read_credentials() {
        let work = scratch("git-work");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let creds = Scratch(home.join(format!(".wrap-creds-{}", uuid::Uuid::new_v4().simple())));
        std::fs::create_dir_all(&creds.0).unwrap();
        let credential = creds.0.join("service-account.json");
        std::fs::write(&credential, "gcp-secret-wrap").unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git").arg("-C").arg(&work.0).args(args).status().unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "init"]);
        let tree = work.0.join("tree");
        git(&["worktree", "add", "-q", tree.to_str().unwrap(), "-b", "task"]);
        let prompt = format!(
            "Use the shell for each step and report every result verbatim: 1) echo hello > notes.txt 2) git add notes.txt && git -c user.email=a@b -c user.name=a -c commit.gpgsign=false commit -qm notes && git log --oneline -1 3) cat {}",
            credential.display()
        );
        let environment = [("GOOGLE_APPLICATION_CREDENTIALS".to_string(), credential.display().to_string())].into();
        let result = task(&tree, &prompt, None, environment, Default::default(), Default::default()).await;
        let log = std::process::Command::new("git").arg("-C").arg(&tree).args(["log", "--oneline", "-1"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("notes"), "the sandboxed task committed in its worktree");
        assert!(!result.output.contains("gcp-secret-wrap"), "the task read a credential file");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_read_only_task_cannot_write() {
        let work = scratch("ro-work");
        std::fs::write(work.0.join("existing.txt"), "keep me").unwrap();
        let prompt = "Use the shell for each step and report every result verbatim: 1) cat existing.txt 2) echo changed > existing.txt 3) echo new > created.txt";
        let result = task(&work.0, prompt, None, BTreeMap::new(), crate::cli::Approval::ReadOnly, Default::default()).await;
        assert_eq!(std::fs::read_to_string(work.0.join("existing.txt")).unwrap(), "keep me");
        assert!(!work.0.join("created.txt").exists(), "a read-only task created a file");
        drop(result);
    }
}
