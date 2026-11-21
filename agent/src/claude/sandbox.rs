use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub(crate) const HIDDEN_PATHS: &[&str] = &[
    "~/.aws",
    "~/.azure",
    "~/.config/gcloud",
    "~/.kube",
    "~/.docker/config.json",
    "~/.netrc",
    "~/.pgpass",
    "~/.npmrc",
    "~/.pypirc",
    "~/.cargo/credentials.toml",
    "~/.claude/.credentials.json",
    "~/.codex/auth.json",
    "~/Library/Keychains",
    "~/.agent-browser",
    "~/Library/Cookies",
    "~/Library/Application Support/dev.ygpt.loka/slack-connection.key",
    "~/Library/Application Support/dev.ygpt.loka/desktop.json",
    "~/Library/Application Support/dev.ygpt.loka/local.json",
    "~/Library/Application Support/dev.ygpt.loka/app-session.key",
    "~/Library/Application Support/dev.ygpt.loka/postgres",
    "~/Library/Application Support/dev.ygpt.loka/pids",
    "~/Library/Application Support/dev.ygpt.loka/runner/leases",
    "~/Library/Application Support/dev.ygpt.loka/runner/runner.log",
    "~/Library/Logs/Loka",
    "~/Library/Application Support/Slack",
    "~/Library/Containers/com.tinyspeck.slackmacgap",
    "~/Library/Application Support/Google/Chrome",
    "~/Library/Application Support/Firefox",
];

const WRITABLE_CACHES: &[&str] = &[
    "~/.cargo/registry",
    "~/.cargo/git",
    "~/.npm",
    "~/.bun/install/cache",
    "~/.cache",
    "~/.pnpm-store",
    "~/.yarn/berry",
    "~/.gradle",
    "~/.m2",
    "~/go/pkg",
    "~/Library/Caches",
    "~/Library/pnpm/store",
];

pub(crate) fn writable_in(home: &Path) -> Vec<PathBuf> {
    WRITABLE_CACHES.iter().filter_map(|p| p.strip_prefix("~/")).map(|rel| home.join(rel)).collect()
}

pub(crate) const GITHUB_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "uploads.github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
];

const HIDDEN_ENV: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_HEADERS",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "OPENROUTER_API_KEY",
    "FAL_KEY",
    "XAI_API_KEY",
    "GEMINI_API_KEY",
    "APNS_KEY",
    "APNS_KEY_ID",
    "RESEND_API_KEY",
    "SENDGRID_API_KEY",
    "LINEAR_API_KEY",
    "CLICKUP_API_TOKEN",
    "GITHUB_WEBHOOK_SECRET",
    "LINEAR_WEBHOOK_SECRET",
    "CLICKUP_WEBHOOK_SECRET",
    "AUTH_GOOGLE_SECRET",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "WORLD_SLACK_KEY_FILE",
    "WORLD_SLACK_CONNECTION_KEY",
    "WORLD_RUNNER_LEASE_FILE",
    "LOKA_APP_SESSION_SECRET",
    "KARNA_APP_SESSION_SECRET",
];

pub(crate) fn app_session_file() -> Option<PathBuf> {
    loka_shared::loka_env::var("APP_SESSION_FILE").filter(|v| !v.trim().is_empty()).map(PathBuf::from)
}

const PROCESS_INFO: &[&str] = &["Read(//proc)", "Read(//proc/**)"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Auto,
    Strict,
}

pub fn mode() -> Mode {
    parse_mode(std::env::var("WORLD_TASK_SANDBOX").ok().as_deref())
}

fn parse_mode(value: Option<&str>) -> Mode {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("off" | "false" | "0" | "none") => Mode::Off,
        Some("strict" | "on" | "true" | "1") => Mode::Strict,
        _ => Mode::Auto,
    }
}

fn signing_key() -> Option<&'static str> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| {
        let out = crate::cli::job_std_command("git")
            .args(["config", "--global", "--get", "user.signingkey"])
            .output()
            .ok()?;
        let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!key.is_empty()).then_some(key)
    })
    .as_deref()
}

fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = crate::cli::job_std_command("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (!path.starts_with(dir)).then_some(path)
}

pub fn git_write_roots(dir: &Path) -> Vec<String> {
    let mut dirs = vec![dir.to_path_buf()];
    if !dir.join(".git").exists() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.join(".git").exists()));
        }
    }
    let mut roots: Vec<String> = dirs.iter().filter_map(|d| git_common_dir(d)).map(|p| p.display().to_string()).collect();
    roots.sort();
    roots.dedup();
    roots
}

pub fn github_token(environment: &BTreeMap<String, String>) -> Option<String> {
    const TTL: Duration = Duration::from_secs(600);
    static CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let set = |name: &str| environment.contains_key(name) || std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("GH_TOKEN") || set("GITHUB_TOKEN") {
        return None;
    }
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, token)) = cache.as_ref() {
        if at.elapsed() < TTL {
            return token.clone();
        }
    }
    let token = crate::cli::job_std_command("gh")
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|t| !t.is_empty());
    *cache = Some((Instant::now(), token.clone()));
    token
}

pub(crate) fn ssh_exceptions(home: Option<&str>) -> Vec<String> {
    ssh_readable(signing_key(), home)
}

fn ssh_readable(signing_key: Option<&str>, home: Option<&str>) -> Vec<String> {
    let expand = |path: &str| match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => path.to_string(),
    };
    let mut paths = vec![expand("~/.ssh/known_hosts"), expand("~/.ssh/config")];
    if let Some(public) = signing_key.map(expand).map(|key| format!("{}.pub", key.trim_end_matches(".pub"))) {
        if Path::new(&public).is_file() {
            paths.push(public);
        }
    }
    paths
}

fn agent_sockets(socket: Option<&Path>) -> Vec<String> {
    let Some(socket) = socket.filter(|s| !s.as_os_str().is_empty()) else { return Vec::new() };
    let mut sockets = vec![socket.display().to_string()];
    if let Some(real) = std::fs::canonicalize(socket).ok().map(|p| p.display().to_string()).filter(|p| *p != sockets[0]) {
        sockets.push(real);
    }
    sockets
}

pub fn settings(
    mode: Mode,
    granted: &BTreeMap<String, String>,
    mcp_config_path: Option<&Path>,
    writable: &[String],
    fast: bool,
) -> Option<Value> {
    if mode == Mode::Off {
        return if fast { Some(json!({ "fastMode": true })) } else { None };
    }
    let mut deny_read: Vec<String> = HIDDEN_PATHS.iter().map(|p| p.to_string()).collect();
    deny_read.push("~/.ssh".into());
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    let allow_read = ssh_readable(signing_key(), home.as_deref());
    let mut sockets = agent_sockets(std::env::var_os("SSH_AUTH_SOCK").as_deref().map(Path::new));
    if let Some(hub) = crate::cmd_hub::get() {
        sockets.extend(agent_sockets(Some(hub.socket())));
    }
    deny_read.extend(crate::cli::runner_private_paths().iter().map(|path| path.display().to_string()));
    for name in crate::cli::CREDENTIAL_FILE_ENV {
        for value in [granted.get(*name).map(std::ffi::OsString::from), std::env::var_os(name)].into_iter().flatten() {
            if !value.is_empty() {
                deny_read.push(Path::new(&value).display().to_string());
            }
        }
    }
    if let Some(key) = std::env::var_os("WORLD_SLACK_KEY_FILE").filter(|v| !v.is_empty()) {
        deny_read.push(Path::new(&key).display().to_string());
    }
    if let Some(file) = app_session_file() {
        deny_read.push(file.display().to_string());
    }
    if let Some(path) = mcp_config_path {
        deny_read.push(path.display().to_string());
    }
    let mut hidden_env: Vec<&str> = HIDDEN_ENV.iter().copied().chain(loka_shared::providers::env_names()).collect();
    hidden_env.sort_unstable();
    hidden_env.dedup();
    let env: Vec<Value> = hidden_env
        .into_iter()
        .filter(|name| !granted.contains_key(*name))
        .map(|name| json!({ "name": name, "mode": "deny" }))
        .collect();
    let allow_write: Vec<String> = WRITABLE_CACHES.iter().map(|p| p.to_string()).chain(writable.iter().cloned()).collect();
    let mut sandbox = json!({
        "enabled": true,
        "failIfUnavailable": mode == Mode::Strict,
        "allowUnsandboxedCommands": false,
        "enableWeakerNetworkIsolation": true,
        "filesystem": {
            "denyRead": deny_read,
            "allowRead": allow_read,
            "allowWrite": allow_write,
        },
        "credentials": { "envVars": env },
    });
    let mut excluded: Vec<String> = Vec::new();
    if let Some(launcher) = granted.get(crate::browser::LAUNCHER_ENV) {
        excluded.extend(crate::browser::excluded_commands(launcher));
    }
    if let Some(launcher) = granted.get(crate::screen::LAUNCHER_ENV) {
        excluded.extend(crate::screen::excluded_commands(launcher));
    }
    if let Some(launcher) = granted.get(crate::apps::LAUNCHER_ENV) {
        excluded.extend(crate::apps::excluded_commands(launcher));
    }
    if !excluded.is_empty() {
        sandbox["excludedCommands"] = json!(excluded);
    }
    sandbox["network"] = json!({ "allowedDomains": GITHUB_HOSTS });
    if !sockets.is_empty() {
        sandbox["network"]["allowUnixSockets"] = json!(sockets);
    }
    let read_rules: Vec<String> = sandbox["filesystem"]["denyRead"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|p| {
            let p = p.trim_end_matches('/');
            let p = if p.starts_with('/') { format!("/{p}") } else { p.to_string() };
            [format!("Read({p})"), format!("Read({p}/**)")]
        })
        .chain(PROCESS_INFO.iter().map(|rule| rule.to_string()))
        .collect();
    let mut settings = json!({ "sandbox": sandbox, "permissions": { "deny": read_rules } });
    if fast {
        settings["fastMode"] = json!(true);
    }
    Some(settings)
}

pub(crate) const OWNER_SSH: &str = ".ssh";

pub(crate) const OWNER_WRITABLE_HOME: &[&str] = &[".tsh", ".ssh/known_hosts"];

pub(crate) const OWNER_SIGNING_KEYS: &[&str] = &[
    ".ssh/karna_signing",
    ".ssh/karna_signing.pub",
    ".ssh/loka_signing",
    ".ssh/loka_signing.pub",
];

pub(crate) const PACKAGE_HOSTS: &[&str] = &[
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "static.rust-lang.org",
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "repo.yarnpkg.com",
    "pypi.org",
    "files.pythonhosted.org",
    "proxy.golang.org",
    "sum.golang.org",
];

pub(crate) fn owner_ssh_readable(home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> =
        ssh_exceptions(home.and_then(Path::to_str)).into_iter().map(PathBuf::from).collect();
    if let Some(home) = home {
        paths.extend(OWNER_SIGNING_KEYS.iter().map(|rel| home.join(rel)));
    }
    paths.sort();
    paths.dedup();
    paths
}

pub fn open_for_owner(settings: &mut Value) {
    let Some(sandbox) = settings.get_mut("sandbox").filter(|s| s.is_object()) else {
        return;
    };
    sandbox["network"]["allowLocalBinding"] = json!(true);
    if let Some(domains) = sandbox["network"]["allowedDomains"].as_array_mut() {
        domains.extend(PACKAGE_HOSTS.iter().map(|host| Value::String(host.to_string())));
    }
    if let Some(read) = sandbox["filesystem"]["allowRead"].as_array_mut() {
        for rel in OWNER_SIGNING_KEYS {
            let path = Value::String(format!("~/{rel}"));
            if !read.contains(&path) {
                read.push(path);
            }
        }
    }
    if let Some(write) = sandbox["filesystem"]["allowWrite"].as_array_mut() {
        write.extend(OWNER_WRITABLE_HOME.iter().map(|rel| Value::String(format!("~/{rel}"))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_jobs_bind_local_ports_and_teleport_but_read_only_signing_keys_in_ssh() {
        let mut owner = settings(Mode::Auto, &BTreeMap::new(), None, &[], false).unwrap();
        open_for_owner(&mut owner);
        assert_eq!(owner["sandbox"]["network"]["allowLocalBinding"], json!(true));
        let domains = owner["sandbox"]["network"]["allowedDomains"].as_array().unwrap();
        assert!(GITHUB_HOSTS.iter().all(|h| domains.iter().any(|d| d == h)));
        assert!(domains.iter().any(|d| d == "index.crates.io"));
        assert!(domains.iter().any(|d| d == "registry.npmjs.org"));
        assert!(denies(&owner, "~/.ssh/id_ed25519"));
        assert!(denies(&owner, "~/.aws/credentials"));
        let read = owner["sandbox"]["filesystem"]["allowRead"].as_array().unwrap();
        assert!(read.iter().any(|p| p == "~/.ssh/karna_signing"));
        assert!(read.iter().any(|p| p == "~/.ssh/karna_signing.pub"));
        assert!(!read.iter().any(|p| p == "~/.ssh/id_ed25519" || p == "~/.ssh"));
        let write = owner["sandbox"]["filesystem"]["allowWrite"].as_array().unwrap();
        assert!(write.iter().any(|p| p == "~/.tsh"));
        assert!(write.iter().any(|p| p == "~/.ssh/known_hosts"));
        let mut fast_only = json!({ "fastMode": true });
        open_for_owner(&mut fast_only);
        assert_eq!(fast_only, json!({ "fastMode": true }));
    }

    #[test]
    fn parses_modes_with_auto_as_default() {
        assert_eq!(parse_mode(None), Mode::Auto);
        assert_eq!(parse_mode(Some("OFF")), Mode::Off);
        assert_eq!(parse_mode(Some("strict")), Mode::Strict);
        assert_eq!(parse_mode(Some("whatever")), Mode::Auto);
    }

    #[test]
    fn owner_jobs_stay_sandboxed_unless_the_sandbox_is_turned_off() {
        assert_eq!(parse_mode(None), Mode::Auto);
        assert_eq!(parse_mode(Some("auto")), Mode::Auto);
    }

    #[test]
    fn owner_ssh_readable_lists_only_config_known_hosts_and_signing_keys() {
        let home = Path::new("/Users/someone");
        let paths = owner_ssh_readable(Some(home));
        assert!(paths.contains(&home.join(".ssh/karna_signing")));
        assert!(paths.contains(&home.join(".ssh/known_hosts")));
        assert!(paths.contains(&home.join(".ssh/config")));
        assert!(!paths.contains(&home.join(".ssh/id_ed25519")));
        assert!(!paths.contains(&home.join(".ssh")));
    }

    #[test]
    fn off_keeps_only_fast_mode() {
        assert!(settings(Mode::Off, &BTreeMap::new(), None, &[], false).is_none());
        assert_eq!(settings(Mode::Off, &BTreeMap::new(), None, &[], true).unwrap(), json!({ "fastMode": true }));
    }

    #[test]
    fn claude_sandbox_hides_local_settings_and_the_app_session() {
        let s = settings(Mode::Strict, &BTreeMap::new(), None, &[], false).unwrap();
        let deny: Vec<&str> = s["sandbox"]["filesystem"]["denyRead"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for app in ["dev.ygpt.loka"] {
            assert!(deny.contains(&format!("~/Library/Application Support/{app}/local.json").as_str()), "{app}");
            assert!(deny.contains(&format!("~/Library/Application Support/{app}/app-session.key").as_str()), "{app}");
        }
        let env: Vec<&str> = s["sandbox"]["credentials"]["envVars"].as_array().unwrap().iter().filter_map(|v| v["name"].as_str()).collect();
        assert!(env.contains(&"LOKA_APP_SESSION_SECRET"));
    }

    #[test]
    fn hides_secrets_but_keeps_granted_keys_and_the_mcp_file() {
        let granted = BTreeMap::from([("FAL_KEY".to_string(), "x".to_string())]);
        let s = settings(Mode::Strict, &granted, Some(Path::new("/tmp/mcp.json")), &[], true).unwrap();
        let sandbox = &s["sandbox"];
        assert_eq!(sandbox["failIfUnavailable"], json!(true));
        assert_eq!(sandbox["allowUnsandboxedCommands"], json!(false));
        let env: Vec<&str> = sandbox["credentials"]["envVars"].as_array().unwrap().iter().filter_map(|v| v["name"].as_str()).collect();
        assert!(env.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!env.contains(&"FAL_KEY"));
        let deny: Vec<&str> = sandbox["filesystem"]["denyRead"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(deny.contains(&"/tmp/mcp.json"));
        assert!(deny.contains(&"~/.aws"));
        assert!(env.contains(&"GOOGLE_APPLICATION_CREDENTIALS"));
        let with_file = BTreeMap::from([("GOOGLE_APPLICATION_CREDENTIALS".to_string(), "/etc/loka/gcp.json".to_string())]);
        let hidden = settings(Mode::Strict, &with_file, None, &[], false).unwrap();
        assert!(hidden["sandbox"]["filesystem"]["denyRead"].as_array().unwrap().iter().any(|p| p == "/etc/loka/gcp.json"), "a credential file named in the env stays out of Bash");
        assert_eq!(s["fastMode"], json!(true));
        assert!(s["permissions"]["deny"].as_array().unwrap().iter().any(|r| r == "Read(~/.aws/**)"));
        assert!(s["permissions"]["deny"].as_array().unwrap().iter().any(|r| r == "Read(//proc/**)"));
        assert!(env.contains(&"OPENROUTER_API_KEY"));
        assert!(env.contains(&"CODEX_API_KEY"));
    }

    fn denies(s: &Value, path: &str) -> bool {
        let read = s["sandbox"]["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .any(|p| path == p || path.starts_with(&format!("{}/", p.trim_end_matches('/'))));
        let tool = s["permissions"]["deny"].as_array().unwrap().iter().filter_map(Value::as_str).any(|rule| {
            let p = rule.trim_start_matches("Read(").trim_end_matches(')');
            match p.strip_suffix("/**") {
                Some(dir) => path.starts_with(&format!("{dir}/")),
                None => path == p,
            }
        });
        read || tool
    }

    #[test]
    fn desktop_runner_workspace_stays_open_while_its_secrets_stay_hidden() {
        let s = settings(Mode::Auto, &BTreeMap::new(), Some(Path::new("/tmp/mcp.json")), &["/x/repo/.git".to_string()], false).unwrap();
        let app = "~/Library/Application Support/dev.ygpt.loka";
        for open in [
            format!("{app}/runner/workspaces/tasks/abc/repo/src/main.rs"),
            format!("{app}/runner/repos/owner/repo/.git/config"),
            format!("{app}/runner/skills/review/SKILL.md"),
            format!("{app}/runner/gitconfig"),
            format!("{app}/runner/uploads/a.png"),
        ] {
            assert!(!denies(&s, &open), "{open} should stay readable");
        }
        for hidden in [
            format!("{app}/slack-connection.key"),
            format!("{app}/desktop.json"),
            format!("{app}/runner/leases/s.json"),
            format!("{app}/runner/runner.log"),
            format!("{app}/local.json"),
            format!("{app}/postgres/PG_VERSION"),
            format!("{app}/pids/api.pid"),
            "~/Library/Logs/Loka/runner.log".to_string(),
        ] {
            assert!(denies(&s, &hidden), "{hidden} should be hidden");
        }
        assert!(s["permissions"]["deny"].as_array().unwrap().iter().any(|r| r == "Read(//tmp/mcp.json)"));
        assert!(s["sandbox"]["filesystem"]["allowWrite"].as_array().unwrap().iter().any(|p| p == "/x/repo/.git"));
        assert_eq!(s["sandbox"]["enableWeakerNetworkIsolation"], json!(true));
        assert!(denies(&s, "~/.ssh/id_ed25519"));
        let hosts = s["sandbox"]["network"]["allowedDomains"].as_array().unwrap();
        for host in ["github.com", "api.github.com", "codeload.github.com", "objects.githubusercontent.com"] {
            assert!(hosts.iter().any(|h| h == host), "{host} should be reachable");
        }
        let dir = std::env::temp_dir().join(format!("sandbox-ssh-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        std::fs::write(dir.join(".ssh/signing.pub"), "k").unwrap();
        let home = dir.display().to_string();
        let readable = ssh_readable(Some("~/.ssh/signing.pub"), Some(&home));
        assert_eq!(
            readable,
            [".ssh/known_hosts", ".ssh/config", ".ssh/signing.pub"].map(|p| format!("{home}/{p}"))
        );
        std::fs::write(dir.join(".ssh/id_ed25519"), "k").unwrap();
        std::fs::write(dir.join(".ssh/id_ed25519.pub"), "k").unwrap();
        let main_key = ssh_readable(Some("~/.ssh/id_ed25519"), Some(&home));
        assert!(main_key.contains(&format!("{home}/.ssh/id_ed25519.pub")));
        assert!(!main_key.contains(&format!("{home}/.ssh/id_ed25519")), "a configured signing key must never expose its private half");
        assert!(!ssh_readable(Some("~/.ssh/id_ed25519.pub"), Some(&home)).contains(&format!("{home}/.ssh/id_ed25519")));
        assert_eq!(ssh_readable(Some("~/.ssh/missing"), Some(&home)).len(), 2);
        let link = dir.join("link");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/agent.sock"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), &link).unwrap();
        let sockets = agent_sockets(Some(&link.join("agent.sock")));
        assert_eq!(sockets.len(), 2);
        assert_eq!(sockets[0], link.join("agent.sock").display().to_string());
        assert_eq!(Path::new(&sockets[1]), std::fs::canonicalize(dir.join("real/agent.sock")).unwrap());
        assert!(agent_sockets(None).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore]
    fn live_sandboxed_task_can_commit_in_a_worktree_but_not_read_secrets() {
        let root = std::env::temp_dir().join(format!("loka-sbx-{}", uuid::Uuid::new_v4()));
        let main = root.join("main");
        let tree = root.join("tree");
        std::fs::create_dir_all(&main).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "--no-gpg-sign", "-m", "init"]);
        git(&main, &["worktree", "add", "-q", "-b", "work", tree.to_str().unwrap()]);
        let mcp = root.join("mcp.json");
        std::fs::write(&mcp, "mcp-secret-value").unwrap();
        let settings = settings(Mode::Strict, &BTreeMap::new(), Some(&mcp), &git_write_roots(&tree), false).unwrap();
        let prompt = format!(
            "Use only the Bash tool. Run each command separately and quote each output verbatim: 1) cat {} 2) echo \"tok=$CLAUDE_CODE_OAUTH_TOKEN$FAL_KEY\" 3) echo hi > f.txt && git add f.txt && git -c user.email=t@t -c user.name=t commit -q --no-gpg-sign -m add && echo committed 4) mkdir -p ~/.cache/loka-sbx && echo cache-ok",
            mcp.display()
        );
        let out = std::process::Command::new("claude")
            .current_dir(&tree)
            .env("FAL_KEY", "fal-leak")
            .args(["-p", "--dangerously-skip-permissions", "--model", "haiku", "--output-format", "text", "--settings"])
            .arg(settings.to_string())
            .arg(&prompt)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        println!("{text}");
        assert!(!text.contains("mcp-secret-value"));
        assert!(!text.contains("fal-leak"));
        let log = std::process::Command::new("git").current_dir(&tree).args(["log", "--oneline"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("add"), "commit in worktree failed: {text}");
        let _ = std::fs::remove_dir_all(dirs_cache().join("loka-sbx"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore]
    fn live_sandboxed_task_works_under_the_desktop_app_support_dir() {
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        let app = home.join("Library/Application Support/dev.ygpt.loka");
        let root = app.join("runner/workspaces").join(format!("sbx-{}", uuid::Uuid::new_v4()));
        let main = root.join("repos/main");
        let origin = std::process::Command::new("git")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["remote", "get-url", "origin"])
            .output()
            .unwrap();
        let origin = String::from_utf8_lossy(&origin.stdout).trim().to_string();
        let tree = root.join("tasks/my task/repo");
        std::fs::create_dir_all(&main).unwrap();
        let secret = app.join("desktop.json");
        let planted = !secret.exists();
        if planted {
            std::fs::write(&secret, "{\"probe\": \"desktop-secret\"}").unwrap();
        }
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "--no-gpg-sign", "-m", "init"]);
        git(&main, &["remote", "add", "origin", &origin]);
        git(&main, &["worktree", "add", "-q", "-b", "work", tree.to_str().unwrap()]);
        std::fs::write(tree.join("a.txt"), "hello\n").unwrap();
        let settings = settings(Mode::Strict, &BTreeMap::new(), None, &git_write_roots(&tree), false).unwrap();
        let prompt = format!(
            "Use the Read tool on a.txt and quote it. Then use only the Bash tool, one command at a time, quoting each output verbatim: 1) cat {:?} 2) echo bye >> a.txt && git add a.txt && git -c user.email=t@t -c user.name=t commit -q --no-gpg-sign -m edit && echo committed 3) git push --dry-run origin work 2>&1 && echo pushed 4) gh api user --jq .login > gh.txt. Your final reply must start with the exact first line of a.txt as you read it, then list each command output.",
            secret.display()
        );
        let out = std::process::Command::new("claude")
            .current_dir(&tree)
            .envs(github_token(&BTreeMap::new()).map(|t| ("GH_TOKEN", t)))
            .args(["-p", "--dangerously-skip-permissions", "--model", "haiku", "--output-format", "text", "--settings"])
            .arg(settings.to_string())
            .arg(&prompt)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        println!("{text}");
        let log = std::process::Command::new("git").current_dir(&tree).args(["log", "--oneline"]).output().unwrap();
        let committed = String::from_utf8_lossy(&log.stdout).contains("edit");
        let gh = std::fs::read_to_string(tree.join("gh.txt")).unwrap_or_default();
        if planted {
            let _ = std::fs::remove_file(&secret);
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(text.contains("hello"), "Read tool blocked in the worktree: {text}");
        assert!(!text.contains("desktop-secret") && !text.contains("runner_id"), "desktop.json leaked: {text}");
        assert!(committed, "commit failed: {text}");
        assert!(text.contains("pushed") && text.contains("[new branch]"), "push failed: {text}");
        assert!(!gh.trim().is_empty(), "gh could not reach GitHub: {text}");
    }

    #[test]
    #[ignore]
    fn live_owner_task_signs_a_merge_commit_with_a_key_file_and_an_agent() {
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        let key_file = std::env::var("SIGN_TEST_KEY").map(std::path::PathBuf::from).unwrap_or_else(|_| home.join(".ssh/loka_signing"));
        assert!(key_file.is_file(), "set SIGN_TEST_KEY to a passphrase-less private key under ~/.ssh");
        let root = std::env::temp_dir().join(format!("loka-sign-{}", uuid::Uuid::new_v4()));
        let main = root.join("main");
        let tree = root.join("tree");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let run = |dir: &Path, program: &str, args: &[&str]| {
            let out = std::process::Command::new(program).current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "{program} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let plain = ["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"];
        run(&main, "git", &["init", "-q", "-b", "main"]);
        run(&main, "git", &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "init"]].concat());
        run(&main, "git", &["branch", "side"]);
        run(&main, "git", &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "on main"]].concat());
        run(&main, "git", &["worktree", "add", "-q", tree.to_str().unwrap(), "side"]);
        run(&tree, "git", &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "on side"]].concat());
        let agent_key = agent_dir.join("k");
        run(&agent_dir, "ssh-keygen", &["-q", "-t", "ed25519", "-N", "", "-f", agent_key.to_str().unwrap()]);
        let socket = std::path::PathBuf::from(format!("/tmp/ks-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        let started = run(&agent_dir, "ssh-agent", &["-a", socket.to_str().unwrap()]);
        let agent_pid = started.split("SSH_AGENT_PID=").nth(1).and_then(|rest| rest.split(';').next()).unwrap_or_default().to_string();
        let add = std::process::Command::new("ssh-add").env("SSH_AUTH_SOCK", &socket).arg(&agent_key).output().unwrap();
        assert!(add.status.success(), "ssh-add: {}", String::from_utf8_lossy(&add.stderr));
        let agent_pub = std::fs::read_to_string(agent_key.with_extension("pub")).unwrap();
        std::fs::remove_file(&agent_key).unwrap();
        std::env::set_var("SSH_AUTH_SOCK", &socket);
        let mut s = settings(Mode::Strict, &BTreeMap::new(), None, &git_write_roots(&tree), false).unwrap();
        open_for_owner(&mut s);
        let script = format!(
            "set -e\ngit -c user.email=t@t -c user.name=t -c gpg.format=ssh -c user.signingkey={key}.pub merge --no-ff -S -m merged main\ngit -c user.email=t@t -c user.name=t -c gpg.format=ssh -c 'user.signingkey=key::{agent}' commit -q --allow-empty -S -m by-agent\necho signed-both\n",
            key = key_file.display(),
            agent = agent_pub.trim(),
        );
        std::fs::write(tree.join("sign.sh"), script).unwrap();
        let out = std::process::Command::new("claude")
            .current_dir(&tree)
            .env("SSH_AUTH_SOCK", &socket)
            .args(["-p", "--dangerously-skip-permissions", "--model", "haiku", "--output-format", "text", "--settings"])
            .arg(s.to_string())
            .arg("Use only the Bash tool. Run exactly `sh sign.sh 2>&1` once and quote its full output verbatim.")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        println!("{text}");
        let signed = |rev: &str| {
            let body = run(&tree, "git", &["cat-file", "-p", rev]);
            body.lines().any(|line| line.starts_with("gpgsig "))
        };
        let parents = run(&tree, "git", &["log", "-1", "--format=%P", "HEAD~1"]);
        let merge_signed = signed("HEAD~1");
        let agent_signed = signed("HEAD");
        let _ = std::process::Command::new("kill").arg(&agent_pid).output();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(parents.split_whitespace().count(), 2, "no merge commit: {text}");
        assert!(merge_signed, "merge commit unsigned with the key file: {text}");
        assert!(agent_signed, "commit unsigned through the ssh agent: {text}");
    }

    fn dirs_cache() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".cache")
    }
}
