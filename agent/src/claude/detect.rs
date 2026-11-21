use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use tracing::{info, warn};

pub(crate) const NOT_FOUND: &str = "Claude Code is not installed on this device, or Loka cannot find the claude command. Install it (curl -fsSL https://claude.ai/install.sh | bash) or put it on the PATH, then restart the Loka app or runner.";

const REQUIRED_FLAGS: &[&str] = &["--tools", "--setting-sources", "--strict-mcp-config", "--fork-session", "--system-prompt", "--output-format"];
const EFFORT_ORDER: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const REFUSED_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const HELP_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct Launch {
    pub program: PathBuf,
    pub path: Option<OsString>,
    pub caps: Arc<Caps>,
    pub auto_refused: bool,
}

impl Launch {
    #[cfg(test)]
    pub(crate) fn assume(program: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), path: None, caps: Arc::new(Caps::unknown()), auto_refused: false }
    }

    pub(crate) fn configure(&self, cmd: &mut tokio::process::Command) {
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
    }
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn version_key(name: &str) -> Vec<u64> {
    name.trim_start_matches('v').split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

fn newest_nvm_bin(nvm: &Path) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(nvm.join("versions/node"))
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("bin"))
        .filter(|bin| executable(&bin.join("claude")))
        .collect();
    versions.sort_by_key(|bin| bin.parent().and_then(Path::file_name).map(|name| version_key(&name.to_string_lossy())).unwrap_or_default());
    versions.pop()
}

pub(crate) fn search_dirs(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    let home = env("HOME").filter(|home| !home.is_empty()).map(PathBuf::from);
    let mut extra: Vec<PathBuf> = env("PNPM_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).into_iter().collect();
    for name in ["VOLTA_HOME", "BUN_INSTALL"] {
        extra.extend(env(name).filter(|v| !v.is_empty()).map(|v| PathBuf::from(v).join("bin")));
    }
    let writable = home.as_deref().map(super::sandbox::writable_in).unwrap_or_default();
    if let Some(home) = &home {
        for rel in [
            ".claude/local",
            ".local/bin",
            ".npm-global/bin",
            ".volta/bin",
            ".bun/bin",
            "Library/pnpm",
            ".local/share/pnpm",
            ".yarn/bin",
            ".asdf/shims",
            ".local/share/mise/shims",
            ".nodenv/shims",
        ] {
            extra.push(home.join(rel));
        }
    }
    let nvm = env("NVM_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".nvm")));
    extra.extend(nvm.as_deref().and_then(newest_nvm_bin));
    extra.push(PathBuf::from("/opt/homebrew/bin"));
    extra.push(PathBuf::from("/usr/local/bin"));
    for dir in extra {
        if !dirs.contains(&dir) && !writable.iter().any(|root| dir.starts_with(root)) {
            dirs.push(dir);
        }
    }
    dirs
}

pub(crate) fn locate_with(env: &dyn Fn(&str) -> Option<String>) -> Option<(PathBuf, Option<OsString>)> {
    let on_path: Vec<PathBuf> = env("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    let found = search_dirs(env).into_iter().map(|dir| dir.join("claude")).find(|bin| executable(bin))?;
    let dir = found.parent().map(Path::to_path_buf).unwrap_or_default();
    let path = (!on_path.contains(&dir)).then(|| std::env::join_paths(std::iter::once(dir).chain(on_path)).ok()).flatten();
    Some((found, path))
}

pub(crate) fn locate() -> Option<(PathBuf, Option<OsString>)> {
    locate_with(&|name| std::env::var(name).ok())
}

pub(crate) fn installed() -> bool {
    locate().is_some()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Caps {
    pub version: Option<String>,
    known: bool,
    flags: HashSet<String>,
    modes: Vec<String>,
    efforts: Vec<String>,
}

fn quoted(text: &str) -> Vec<String> {
    text.split('"').skip(1).step_by(2).map(str::to_string).collect()
}

fn after<'a>(text: &'a str, marker: &str, within: usize) -> Option<&'a str> {
    let at = text.find(marker)? + marker.len();
    let end = text[at..].char_indices().nth(within).map(|(i, _)| at + i).unwrap_or(text.len());
    Some(&text[at..end])
}

impl Caps {
    pub(crate) fn unknown() -> Self {
        Self::default()
    }

    pub(crate) fn parse(version: Option<String>, help: &str) -> Self {
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        let flags: HashSet<String> = flat
            .split(' ')
            .filter(|token| token.starts_with("--") && token.len() > 2)
            .map(|token| token.trim_end_matches([',', '.', ')', ':']).split('=').next().unwrap_or("").to_string())
            .collect();
        if !flags.contains("--print") && !flags.contains("--output-format") {
            return Self { version, ..Self::unknown() };
        }
        let modes = after(&flat, "--permission-mode <mode>", 400)
            .and_then(|segment| segment.split_once("(choices:"))
            .map(|(_, rest)| quoted(rest.split(')').next().unwrap_or("")))
            .unwrap_or_default();
        let efforts = after(&flat, "--effort <level>", 200)
            .and_then(|segment| segment.split_once('('))
            .map(|(_, rest)| {
                rest.split(')')
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .map(|level| level.trim().to_string())
                    .filter(|level| EFFORT_ORDER.contains(&level.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        Self { version, known: true, flags, modes, efforts }
    }

    pub(crate) fn has(&self, flag: &str) -> bool {
        !self.known || self.flags.contains(flag)
    }

    pub(crate) fn mode(&self, mode: &str) -> bool {
        !self.known || self.modes.is_empty() || self.modes.iter().any(|m| m == mode)
    }

    pub(crate) fn effort<'a>(&self, level: &'a str) -> Option<&'a str> {
        if !self.has("--effort") {
            return None;
        }
        if self.efforts.is_empty() || self.efforts.iter().any(|e| e == level) {
            return Some(level);
        }
        let rank = EFFORT_ORDER.iter().position(|e| *e == level)?;
        EFFORT_ORDER[..rank].iter().rev().find(|e| self.efforts.iter().any(|known| known == *e)).copied()
    }

    pub(crate) fn outdated(&self) -> Option<String> {
        let missing: Vec<&str> = REQUIRED_FLAGS.iter().copied().filter(|flag| !self.has(flag)).collect();
        if missing.is_empty() {
            return None;
        }
        Some(format!(
            "Claude Code {} is too old for Loka (it has no {}). Update it on this device with `claude update`, then try again.",
            self.version.as_deref().unwrap_or("on this device"),
            missing.join(", ")
        ))
    }
}

type CapsCache = Mutex<Option<((PathBuf, Option<SystemTime>), Arc<Caps>)>>;

fn caps_cache() -> &'static CapsCache {
    static CACHE: std::sync::OnceLock<CapsCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

async fn capture(launch: &Launch, arg: &str) -> Option<String> {
    let mut cmd = crate::cli::job_command(&launch.program);
    crate::cli::configure_child_environment(&mut cmd, &BTreeMap::new()).ok()?;
    launch.configure(&mut cmd);
    cmd.arg(arg).kill_on_drop(true).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let out = tokio::time::timeout(HELP_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

async fn caps_for(launch: &Launch) -> Arc<Caps> {
    let key = (launch.program.clone(), std::fs::metadata(&launch.program).and_then(|m| m.modified()).ok());
    if let Some((cached, caps)) = caps_cache().lock().unwrap().as_ref() {
        if *cached == key {
            return caps.clone();
        }
    }
    let (version, help) = tokio::join!(capture(launch, "--version"), capture(launch, "--help"));
    let version = version.and_then(|v| v.split_whitespace().next().map(str::to_string));
    let caps = Arc::new(match help {
        Some(help) => Caps::parse(version, &help),
        None => Caps { version, ..Caps::unknown() },
    });
    info!(version = caps.version.as_deref().unwrap_or("unknown"), known = caps.known, "Detected Claude Code");
    *caps_cache().lock().unwrap() = Some((key, caps.clone()));
    caps
}

type AutoCache = Mutex<HashMap<(u64, String, bool), Instant>>;

fn auto_cache() -> &'static AutoCache {
    static CACHE: std::sync::OnceLock<AutoCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const ACCOUNT_KEYS: &[&str] = &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CONFIG_DIR"];

fn account(opts: &crate::cli::CliOptions<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let env = opts.child_environment();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in ACCOUNT_KEYS {
        env.get(*name).hash(&mut hasher);
    }
    hasher.finish()
}

pub(crate) fn note_auto_refused(opts: &crate::cli::CliOptions<'_>) {
    warn!(model = opts.model, fast = opts.fast, "Claude Code refused auto mode; later auto runs of this account fail until the owner picks another model or approval");
    auto_cache().lock().unwrap().insert((account(opts), opts.model.to_string(), opts.fast), Instant::now());
}

fn auto_refused(opts: &crate::cli::CliOptions<'_>) -> bool {
    auto_cache()
        .lock()
        .unwrap()
        .get(&(account(opts), opts.model.to_string(), opts.fast))
        .is_some_and(|at| at.elapsed() < REFUSED_TTL)
}

pub(crate) fn wants_auto(opts: &crate::cli::CliOptions<'_>) -> bool {
    !opts.lean && opts.approval == crate::cli::Approval::Auto && super::auto_mode_supported(opts.model)
}

pub(crate) async fn detected() -> Result<Launch> {
    let (program, path) = locate().ok_or_else(|| anyhow::anyhow!(NOT_FOUND))?;
    let mut launch = Launch { program, path, caps: Arc::new(Caps::unknown()), auto_refused: false };
    launch.caps = caps_for(&launch).await;
    if let Some(message) = launch.caps.outdated() {
        anyhow::bail!(message);
    }
    Ok(launch)
}

pub(crate) fn version() -> Option<String> {
    caps_cache().lock().unwrap().as_ref().and_then(|(_, caps)| caps.version.clone())
}

pub(crate) async fn prepare(opts: &crate::cli::CliOptions<'_>) -> Result<Launch> {
    let mut launch = detected().await?;
    launch.auto_refused = wants_auto(opts) && (!launch.caps.mode("auto") || auto_refused(opts));
    Ok(launch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = r#"Usage: claude [options] [command] [prompt]
  --effort <level>                      Effort level for the current session
                                        (low, medium, high, xhigh, max)
  --fork-session                        When resuming, create a new session ID
  --output-format <format>              Output format (only works with --print)
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: "acceptEdits", "auto",
                                        "bypassPermissions", "manual",
                                        "dontAsk", "plan")
  -p, --print                           Print response and exit
  --setting-sources <sources>           Comma-separated list
  --strict-mcp-config                   Only use MCP servers from --mcp-config
  --system-prompt <prompt>              System prompt to use for the session
  --tools <tools...>                    Specify the list of available tools"#;

    #[test]
    fn lookup_never_searches_folders_sandboxed_jobs_can_write() {
        let env = |name: &str| match name {
            "HOME" => Some("/Users/sam".to_string()),
            "PATH" => Some("/usr/bin".to_string()),
            _ => None,
        };
        let dirs = search_dirs(&env);
        let writable = crate::claude::sandbox::writable_in(Path::new("/Users/sam"));
        for dir in dirs.iter().filter(|d| d.as_path() != Path::new("/usr/bin")) {
            assert!(!writable.iter().any(|root| dir.starts_with(root)), "{} is writable inside the sandbox", dir.display());
        }
        assert!(dirs.contains(&PathBuf::from("/Users/sam/.yarn/bin")));
        assert!(dirs.contains(&PathBuf::from("/Users/sam/Library/pnpm")));
    }

    #[test]
    fn reads_flags_modes_and_effort_levels_from_help() {
        let caps = Caps::parse(Some("2.1.280".into()), HELP);
        assert!(caps.outdated().is_none());
        assert!(caps.mode("auto") && caps.mode("dontAsk") && !caps.mode("default"));
        assert_eq!(caps.effort("xhigh"), Some("xhigh"));
        assert!(caps.has("--mcp-config"));
        assert!(!caps.has("--no-session-persistence"));
    }

    #[test]
    fn older_clis_degrade_instead_of_failing() {
        let old = HELP
            .replace("(low, medium, high, xhigh, max)", "(low, medium, high)")
            .replace("\"auto\",", "")
            .replace("\"dontAsk\", ", "");
        let caps = Caps::parse(Some("2.0.9".into()), &old);
        assert!(!caps.mode("auto"));
        assert_eq!(caps.effort("max"), Some("high"));
        assert_eq!(caps.effort("xhigh"), Some("high"));
        assert_eq!(caps.effort("low"), Some("low"));
        let ancient = Caps::parse(Some("1.0.3".into()), "Usage: claude\n  -p, --print  Print\n  --output-format <f>  x\n  --system-prompt <p>  x");
        assert_eq!(ancient.effort("high"), None);
        let message = ancient.outdated().unwrap();
        assert!(message.contains("1.0.3") && message.contains("--tools") && message.contains("claude update"));
        assert!(!message.contains('\u{2014}'));
        let unknown = Caps::parse(None, "something else entirely");
        assert!(unknown.outdated().is_none() && unknown.mode("auto") && unknown.effort("max") == Some("max"));
    }

    fn work_options(dir: &Path) -> crate::cli::CliOptions<'_> {
        crate::cli::CliOptions {
            working_dir: dir,
            prompt: "Use the Write tool to create done.txt containing the word ok, then reply with the word done.",
            system_prompt: None,
            allowed_tools: None,
            max_turns: 4,
            model: "sonnet",
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
            approval: crate::cli::Approval::Auto,
            steer: Default::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files: Vec::new(),
        }
    }

    #[test]
    fn auto_refusals_stay_with_the_account_that_hit_them() {
        let work = std::env::temp_dir();
        let mut mine = work_options(&work);
        mine.model = "refusal-test-model";
        mine.provider_keys.insert("ANTHROPIC_API_KEY".into(), "sk-ant-mine".into());
        let mut theirs = work_options(&work);
        theirs.model = "refusal-test-model";
        theirs.provider_keys.insert("ANTHROPIC_API_KEY".into(), "sk-ant-theirs".into());
        note_auto_refused(&mine);
        assert!(auto_refused(&mine));
        assert!(!auto_refused(&theirs), "one owner's refusal never flips another owner's runs to full access");
    }

    #[tokio::test]
    #[ignore]
    async fn live_refused_auto_mode_is_caught_and_the_retry_edits_files() {
        let root = std::env::temp_dir().join(format!("claude-auto-{}", uuid::Uuid::new_v4()));
        let config = root.join("config");
        let work = root.join("work");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(config.join("settings.json"), r#"{"disableAutoMode":"disable"}"#).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &config);
        auto_cache().lock().unwrap().clear();

        let launch = prepare(&work_options(&work)).await.unwrap();
        println!("version {:?}", launch.caps.version);
        assert!(!launch.auto_refused);
        let first = match crate::claude::run(work_options(&work)).await {
            Ok(result) => panic!("the refused run should stop, it said: {}", result.output),
            Err(error) => error.to_string(),
        };
        println!("first: {first}");
        assert_eq!(loka_shared::cli_errors::classify(&first).map(|f| f.kind), Some(loka_shared::cli_errors::FailureKind::Permissions));
        assert!(!work.join("done.txt").exists());

        assert!(prepare(&work_options(&work)).await.unwrap().auto_refused);
        let result = crate::claude::run(work_options(&work)).await.unwrap();
        println!("second: {}", result.output);
        let written = std::fs::read_to_string(work.join("done.txt")).unwrap_or_default();
        auto_cache().lock().unwrap().clear();
        let _ = std::fs::remove_dir_all(&root);
        assert!(written.contains("ok"), "the fallback run could not write: {}", result.output);
    }

    #[cfg(unix)]
    #[test]
    fn finds_claude_outside_a_gui_path_and_puts_its_dir_first() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("claude-locate-{}", uuid::Uuid::new_v4()));
        for version in ["v18.20.1", "v22.3.0", "v9.1.0"] {
            let bin = home.join(".nvm/versions/node").join(version).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("claude"), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home_str = home.display().to_string();
        let env = |name: &str| match name {
            "HOME" => Some(home_str.clone()),
            "PATH" => Some("/nonexistent/bin:/usr/bin".to_string()),
            _ => None,
        };
        let (found, path) = locate_with(&env).unwrap();
        let newest = home.join(".nvm/versions/node/v22.3.0/bin");
        assert_eq!(found, newest.join("claude"));
        let path = path.unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs, vec![newest.clone(), PathBuf::from("/nonexistent/bin"), PathBuf::from("/usr/bin")]);
        let on_path = format!("{}:/usr/bin", newest.display());
        let env = |name: &str| match name {
            "HOME" => Some(home_str.clone()),
            "PATH" => Some(on_path.clone()),
            _ => None,
        };
        assert_eq!(locate_with(&env).unwrap().1, None);
        std::fs::set_permissions(newest.join("claude"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(locate_with(&env).unwrap().0, home.join(".nvm/versions/node/v18.20.1/bin/claude"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
