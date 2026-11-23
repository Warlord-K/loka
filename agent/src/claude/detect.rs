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
    pub plan_signed_in: bool,
    pub uses_plan: bool,
}

impl Launch {
    #[cfg(test)]
    pub(crate) fn assume(program: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), path: None, caps: Arc::new(Caps::unknown()), auto_refused: false, plan_signed_in: false, uses_plan: false }
    }

    pub(crate) fn configure(&self, cmd: &mut tokio::process::Command) {
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserSettings {
    ReadByCli,
    AppliedByLoka,
}

pub(crate) struct SignIn {
    env: ClaudeEnv,
    plan: bool,
}

impl SignIn {
    pub(crate) fn settle(cmd: &mut tokio::process::Command, source: UserSettings, launch: &Launch) -> Self {
        let env = ClaudeEnv::of(cmd, source);
        let plan = plan_wins(&env, launch.plan_signed_in);
        if source == UserSettings::AppliedByLoka {
            for (name, value) in env.settings.iter().filter(|(name, _)| !(plan && API_KEY_ENV.contains(&name.as_str()))) {
                cmd.env(name, value);
            }
        }
        if plan {
            for name in API_KEY_ENV {
                cmd.env_remove(name);
            }
        }
        Self { env, plan }
    }

    pub(crate) fn reads_user_settings(&self) -> bool {
        self.env.source == UserSettings::ReadByCli
    }

    pub(crate) fn settings(&self, settings: Option<serde_json::Value>) -> Option<serde_json::Value> {
        let blanked: Vec<&str> = if self.plan && self.reads_user_settings() {
            API_KEY_ENV.iter().copied().filter(|name| self.env.settings.contains_key(*name)).collect()
        } else {
            Vec::new()
        };
        let extra = (self.env.source == UserSettings::AppliedByLoka).then_some(&self.env.extra).filter(|extra| !extra.is_empty());
        if blanked.is_empty() && extra.is_none() {
            return settings;
        }
        let mut settings = settings.unwrap_or_else(|| serde_json::json!({}));
        for (key, value) in extra.into_iter().flatten() {
            settings[key] = value.clone();
        }
        for name in blanked {
            if !settings["env"].is_object() {
                settings["env"] = serde_json::json!({});
            }
            settings["env"][name] = serde_json::json!("");
        }
        Some(settings)
    }
}

pub(crate) const API_KEY_ENV: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];
pub(crate) const PLAN_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const PLAN_AUTH_METHODS: &[&str] = &["claude.ai", "oauth_token"];
const SIGN_IN_TTL: Duration = Duration::from_secs(5 * 60);

const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";
const ANTHROPIC_HOST: &str = "api.anthropic.com";

const CLOUD_ROUTES: &[&str] = &["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"];

pub(crate) fn route_is_first_party(get: &dyn Fn(&str) -> Option<String>) -> bool {
    let switched_on = |name: &str| get(name).is_some_and(|value| !matches!(value.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"));
    if CLOUD_ROUTES.iter().any(|name| switched_on(name)) {
        return false;
    }
    match get(BASE_URL_ENV).map(|url| url.trim().to_string()).filter(|url| !url.is_empty()) {
        None => true,
        Some(url) => url::Url::parse(&url).ok().and_then(|url| url.host_str().map(|host| host.eq_ignore_ascii_case(ANTHROPIC_HOST))) == Some(true),
    }
}

pub(crate) fn host_route_lookup(granted: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> {
    let settings = string_values(super::user_auth_settings().and_then(|mut settings| settings.remove("env")));
    let granted = granted.clone();
    move |name: &str| settings.get(name).cloned().or_else(|| granted.get(name).cloned()).or_else(|| std::env::var(name).ok())
}

pub(crate) struct ClaudeEnv {
    process: HashMap<String, String>,
    settings: HashMap<String, String>,
    extra: serde_json::Map<String, serde_json::Value>,
    source: UserSettings,
}

fn string_values(env: Option<serde_json::Value>) -> HashMap<String, String> {
    env.and_then(|env| env.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, value)| Some((name, value.as_str()?.to_string())))
        .collect()
}

impl ClaudeEnv {
    pub(crate) fn of(cmd: &tokio::process::Command, source: UserSettings) -> Self {
        let process = crate::key_proxy::env_of(cmd);
        let file = super::config_dir_from(&|name| process.get(name).cloned()).map(|dir| dir.join("settings.json"));
        let (settings, extra) = match (source, file) {
            (UserSettings::ReadByCli, Some(path)) => (string_values(super::settings_at(&path).and_then(|mut settings| settings.remove("env"))), serde_json::Map::new()),
            (UserSettings::AppliedByLoka, Some(path)) => {
                let mut auth = super::auth_settings_at(&path).unwrap_or_default();
                (string_values(auth.remove("env")), auth)
            }
            _ => Default::default(),
        };
        Self { process, settings, extra, source }
    }

    #[cfg(test)]
    fn from_pairs(process: &[(&str, &str)], settings: &[(&str, &str)]) -> Self {
        let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
        Self { process: map(process), settings: map(settings), extra: Default::default(), source: UserSettings::ReadByCli }
    }

    fn merged(&self) -> HashMap<String, String> {
        let mut env = self.process.clone();
        env.extend(self.settings.iter().map(|(name, value)| (name.clone(), value.clone())));
        env
    }

    fn get(&self, name: &str) -> Option<String> {
        self.settings.get(name).or_else(|| self.process.get(name)).cloned()
    }

    fn has(&self, name: &str) -> bool {
        self.get(name).is_some_and(|v| !v.trim().is_empty())
    }

    fn config_dir(&self) -> Option<PathBuf> {
        super::config_dir_from(&|name| self.process.get(name).cloned())
    }

    pub(crate) fn first_party_route(&self) -> bool {
        route_is_first_party(&|name| self.get(name))
    }

    fn routes_to_a_gateway(&self) -> bool {
        self.get(BASE_URL_ENV).filter(|url| !url.trim().is_empty()).is_some_and(|url| {
            url::Url::parse(url.trim()).ok().and_then(|url| url.host_str().map(|host| host.eq_ignore_ascii_case(ANTHROPIC_HOST))) != Some(true)
        })
    }

    fn key_could_lose_to_the_plan(&self) -> bool {
        API_KEY_ENV.iter().any(|name| self.has(name)) && !self.routes_to_a_gateway()
    }
}

pub(crate) fn plan_wins(env: &ClaudeEnv, stored_plan_login: bool) -> bool {
    env.key_could_lose_to_the_plan() && (env.has(PLAN_TOKEN_ENV) || stored_plan_login)
}

fn needs_sign_in_check(env: &ClaudeEnv) -> bool {
    env.key_could_lose_to_the_plan() && !env.has(PLAN_TOKEN_ENV)
}

pub(crate) fn plan_auth_method(status: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(status.trim())
        .ok()
        .and_then(|status| status.get("authMethod").and_then(serde_json::Value::as_str).map(|method| PLAN_AUTH_METHODS.contains(&method)))
        .unwrap_or(false)
}

const SIGN_IN_ROUTES: &[&str] = &[BASE_URL_ENV, "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"];

type SignInKey = (PathBuf, Option<PathBuf>, Vec<Option<String>>);
type SignInCache = Mutex<HashMap<SignInKey, (Instant, bool)>>;

fn sign_in_cache() -> &'static SignInCache {
    static CACHE: std::sync::OnceLock<SignInCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

async fn stored_plan_login(launch: &Launch, env: &ClaudeEnv) -> bool {
    let key = (launch.program.clone(), env.config_dir(), SIGN_IN_ROUTES.iter().map(|name| env.get(name)).collect());
    if let Some((at, signed_in)) = sign_in_cache().lock().unwrap().get(&key) {
        if at.elapsed() < SIGN_IN_TTL {
            return *signed_in;
        }
    }
    let signed_in = probe_plan_login(launch, env).await;
    sign_in_cache().lock().unwrap().insert(key, (Instant::now(), signed_in));
    signed_in
}

async fn probe_plan_login(launch: &Launch, env: &ClaudeEnv) -> bool {
    let mut cmd = crate::cli::job_command(&launch.program);
    cmd.env_clear().envs(env.merged());
    for name in API_KEY_ENV.iter().chain([&PLAN_TOKEN_ENV]) {
        cmd.env_remove(name);
    }
    cmd.args(["--setting-sources", "", "auth", "status", "--json"]).kill_on_drop(true).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    match tokio::time::timeout(HELP_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => plan_auth_method(&String::from_utf8_lossy(&out.stdout)),
        _ => {
            warn!("Could not ask Claude Code whether it is signed in with a plan");
            false
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
    let mut launch = Launch { program, path, caps: Arc::new(Caps::unknown()), auto_refused: false, plan_signed_in: false, uses_plan: false };
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
    let cmd = super::run_command(opts, &launch)?;
    let env = ClaudeEnv::of(&cmd, super::user_settings(opts, &cmd));
    launch.plan_signed_in = needs_sign_in_check(&env) && stored_plan_login(&launch, &env).await;
    launch.uses_plan = plan_wins(&env, launch.plan_signed_in);
    Ok(launch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_party_route_counts_as_one() {
        let route = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, String> = pairs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
            route_is_first_party(&|name| map.get(name).cloned())
        };
        assert!(route(&[]));
        assert!(route(&[("ANTHROPIC_BASE_URL", "https://api.anthropic.com")]));
        assert!(route(&[("ANTHROPIC_BASE_URL", "https://API.anthropic.com/")]));
        assert!(route(&[("CLAUDE_CODE_USE_BEDROCK", "0")]));
        assert!(!route(&[("ANTHROPIC_BASE_URL", "http://localhost:4000")]));
        assert!(!route(&[("ANTHROPIC_BASE_URL", "https://openrouter.ai/api")]));
        assert!(!route(&[("CLAUDE_CODE_USE_BEDROCK", "1")]));
        assert!(!route(&[("CLAUDE_CODE_USE_VERTEX", "true")]));
        assert!(!route(&[("CLAUDE_CODE_USE_FOUNDRY", "1")]));
    }

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
    fn plan_sign_in_is_read_from_the_cli_auth_method() {
        assert!(plan_auth_method(r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty"}"#));
        assert!(plan_auth_method(r#"{"loggedIn": true, "authMethod": "oauth_token"}"#));
        assert!(!plan_auth_method(r#"{"loggedIn": true, "authMethod": "api_key"}"#));
        assert!(!plan_auth_method(r#"{"loggedIn": true, "authMethod": "third_party", "apiProvider": "bedrock"}"#));
        assert!(!plan_auth_method(r#"{"loggedIn": false, "authMethod": "none"}"#));
        assert!(!plan_auth_method("Not logged in"));
    }

    #[test]
    fn the_sign_in_is_checked_only_when_a_key_would_otherwise_win() {
        let with = |names: &[&str]| ClaudeEnv::from_pairs(&names.iter().map(|name| (*name, "set")).collect::<Vec<_>>(), &[]);
        assert!(!needs_sign_in_check(&with(&[])), "neither");
        assert!(!needs_sign_in_check(&with(&["CLAUDE_CODE_OAUTH_TOKEN"])), "plan token only");
        assert!(!needs_sign_in_check(&with(&["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"])), "the token already wins");
        assert!(needs_sign_in_check(&with(&["ANTHROPIC_API_KEY"])), "key only");
        assert!(needs_sign_in_check(&with(&["ANTHROPIC_AUTH_TOKEN"])), "bearer token only");
        assert!(plan_wins(&with(&["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]), false));
        assert!(plan_wins(&with(&["ANTHROPIC_API_KEY"]), true));
        assert!(!plan_wins(&with(&["ANTHROPIC_API_KEY"]), false));
        assert!(!plan_wins(&with(&["CLAUDE_CODE_OAUTH_TOKEN"]), true), "nothing to withhold");
        let routed = |url: &str| ClaudeEnv::from_pairs(&[("ANTHROPIC_BASE_URL", url), ("ANTHROPIC_AUTH_TOKEN", "set"), ("CLAUDE_CODE_OAUTH_TOKEN", "set")], &[]);
        assert!(!plan_wins(&routed("https://gateway.example.com/v1"), true), "a gateway keeps its own credentials, so the plan login never goes to it");
        assert!(!needs_sign_in_check(&routed("http://127.0.0.1:4000")));
        assert!(plan_wins(&routed("https://api.anthropic.com"), false), "the Anthropic API itself is no gateway");
        let settings_gateway = ClaudeEnv::from_pairs(
            &[("ANTHROPIC_AUTH_TOKEN", "set"), ("ANTHROPIC_BASE_URL", "https://api.anthropic.com")],
            &[("ANTHROPIC_BASE_URL", "https://gateway.example.com")],
        );
        assert!(!plan_wins(&settings_gateway, true), "a gateway set in settings.json wins over the process env, as in Claude Code");
        assert!(!needs_sign_in_check(&settings_gateway));
    }

    #[test]
    fn a_gateway_from_the_job_users_settings_keeps_its_token() {
        let root = std::env::temp_dir().join(format!("claude-gateway-{}", uuid::Uuid::new_v4()));
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("settings.json"), r#"{"env": {"ANTHROPIC_BASE_URL": "https://llm-gateway.example.com"}}"#).unwrap();
        let run = |plan_signed_in: bool| {
            let mut cmd = tokio::process::Command::new("claude");
            cmd.env("CLAUDE_CONFIG_DIR", &config).env("ANTHROPIC_AUTH_TOKEN", "gateway-token");
            let env = ClaudeEnv::of(&cmd, UserSettings::ReadByCli);
            SignIn::settle(&mut cmd, UserSettings::ReadByCli, &Launch { plan_signed_in, ..Launch::assume("claude") });
            let kept = cmd.as_std().get_envs().any(|(name, value)| name == "ANTHROPIC_AUTH_TOKEN" && value.is_some());
            (needs_sign_in_check(&env), kept)
        };
        assert_eq!(run(true), (false, true), "the plan login is never sent to a gateway named only in settings.json");
        std::fs::write(config.join("settings.json"), r#"{"env": {"ANTHROPIC_BASE_URL": "https://api.anthropic.com"}}"#).unwrap();
        assert_eq!(run(true), (true, false), "the Anthropic API in settings.json still lets the plan win");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_remembered_sign_in_is_kept_apart_per_provider_route() {
        let dir = std::env::temp_dir().join(format!("claude-sign-in-routes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude");
        std::fs::write(&program, "#!/bin/sh\nif [ -n \"$CLAUDE_CODE_USE_BEDROCK\" ]; then printf '%s\\n' '{\"authMethod\":\"third_party\"}'; else printf '%s\\n' '{\"authMethod\":\"claude.ai\"}'; fi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = dir.join("config");
        let config = config.to_string_lossy();
        let bedrock = ClaudeEnv::from_pairs(&[("PATH", "/usr/bin:/bin"), ("CLAUDE_CONFIG_DIR", &config), ("ANTHROPIC_API_KEY", "set"), ("CLAUDE_CODE_USE_BEDROCK", "1")], &[]);
        let direct = ClaudeEnv::from_pairs(&[("PATH", "/usr/bin:/bin"), ("CLAUDE_CONFIG_DIR", &config), ("ANTHROPIC_API_KEY", "set")], &[]);
        let launch = Launch::assume(program);
        assert!(!stored_plan_login(&launch, &bedrock).await);
        assert!(stored_plan_login(&launch, &direct).await, "a Bedrock answer for one run is never reused for a direct one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sign_in_is_remembered_per_config_dir_of_the_job_user() {
        let job = ClaudeEnv::from_pairs(&[("HOME", "/runner/tmp/2001")], &[]);
        let other_job = ClaudeEnv::from_pairs(&[("HOME", "/runner/tmp/2002")], &[]);
        let shared = ClaudeEnv::from_pairs(&[("HOME", "/runner/tmp/2001"), ("CLAUDE_CONFIG_DIR", "/persist/claude")], &[]);
        assert_eq!(job.config_dir(), Some(PathBuf::from("/runner/tmp/2001/.claude")));
        assert_ne!(job.config_dir(), other_job.config_dir());
        assert_eq!(shared.config_dir(), Some(PathBuf::from("/persist/claude")), "an explicit config dir wins over the job home");
    }

    #[tokio::test]
    async fn the_stored_sign_in_check_asks_the_cli_without_any_key() {
        let dir = std::env::temp_dir().join(format!("claude-sign-in-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let plan = fake(
            "plan",
            r#"if [ -n "$ANTHROPIC_API_KEY$ANTHROPIC_AUTH_TOKEN$CLAUDE_CODE_OAUTH_TOKEN" ] || [ "$1 $2 $3" != "--setting-sources  auth" ]; then printf '%s\n' '{"loggedIn":true,"authMethod":"api_key"}'; elif [ "$ANTHROPIC_SMALL_FAST_MODEL" != "haiku" ]; then printf '%s\n' '{"loggedIn":false}'; else printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai"}'; fi"#,
        );
        let signed_out = fake("signed-out", r#"printf '%s\n' '{"loggedIn":false,"authMethod":"none"}'"#);
        let broken = fake("broken", "exit 1");
        let env = ClaudeEnv::from_pairs(
            &[("PATH", "/usr/bin:/bin"), ("ANTHROPIC_API_KEY", "sk-ant-api03-key"), ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-token")],
            &[("ANTHROPIC_AUTH_TOKEN", "sk-ant-api03-from-settings"), ("ANTHROPIC_SMALL_FAST_MODEL", "haiku")],
        );
        assert!(probe_plan_login(&Launch::assume(plan), &env).await, "a stored plan login is found, asked on the final env without any key or token and without re-reading settings.json");
        assert!(!probe_plan_login(&Launch::assume(signed_out), &env).await, "no plan login, so the key stays the fallback");
        assert!(!probe_plan_login(&Launch::assume(broken), &env).await, "an unreadable answer keeps the key");
        let _ = std::fs::remove_dir_all(&dir);
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
