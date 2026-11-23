pub(crate) mod detect;
pub(crate) mod sandbox;

use anyhow::{Context, Result};
use detect::Launch;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

pub use crate::cli::Timing;

use crate::cli::{
    collect_stderr, configure_child_environment, give_to_job_user, job_command, session_id_from_init,
    summarize_tool_input, summarize_tool_output, CliOptions, CliResult, StreamEvent,
};

const AGENT_SYSTEM_PROMPT: &str = "\
You are Loka, an autonomous coding agent. You work independently without \
human interaction during execution. Never use AskUserQuestion — there is no human \
in the loop. If you are uncertain, make the best judgment call and document your \
reasoning in a code comment or commit message.\n\n\
Bias to action: when one part is blocked, find another way or finish everything \
around it, state your assumptions and report what is left and why instead of \
stopping early. Stay careful only with permissions the person must give, \
credentials, destructive or irreversible steps and budgets.\n\n\
Follow existing code patterns and conventions in each repository. \
Read CLAUDE.md if it exists for project-specific instructions.\n\n\
Git commit rules:\n\
- Use Conventional Commits: type(scope): description\n\
- Types: feat, fix, refactor, test, chore, perf, ci\n\
- NEVER add Co-Authored-By trailers to commits\n\
- NEVER add Signed-off-by trailers to commits\n\n\
Permissions:\n\
- When the person you work for, or one of their rules, allows something such as force-pushing or rewriting history, do it; that is the approval. Prefer --force-with-lease.\n\
- Rewriting your own quest branch and pushing it with --force-with-lease needs no approval. Force pushing any other branch does.\n\
- If a sandbox blocks a host your work needs, re-run the command with that host in allowed_domains.";

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    Ok(run_timed(opts).await?.0)
}

pub async fn run_timed(opts: CliOptions<'_>) -> Result<(CliResult, Timing)> {
    crate::cli::check_model(opts.model)?;
    let launch = detect::prepare(&opts).await?;
    let mcp_config_path = if let Some(mcp_json) = opts.mcp_config_json.as_deref() {
        Some(write_mcp_config_temp_file(mcp_json).await?)
    } else {
        None
    };

    run_inner(&opts, mcp_config_path.as_ref().map(|file| file.path.as_path()), &launch).await
}

pub(crate) fn spawn(cmd: &mut Command) -> Result<tokio::process::Child> {
    cmd.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(detect::NOT_FOUND)
        } else {
            anyhow::Error::new(error).context("Failed to start Claude Code")
        }
    })
}

pub(crate) fn refusal(reader: &TurnReader, opts: &CliOptions<'_>) -> Option<anyhow::Error> {
    let actual = reader.refused_mode.as_deref()?;
    Some(match reader.expect_mode {
        Some("auto") => {
            detect::note_auto_refused(opts);
            anyhow::anyhow!(
                "Claude Code would not start in auto mode (it fell back to {actual} mode, where it cannot edit files or run commands). {AUTO_UNAVAILABLE_FIX}"
            )
        }
        _ => anyhow::anyhow!(
            "Claude Code would not start with full access (it fell back to {actual} mode, where it cannot edit files or run commands). This device's Claude settings turn full access off (disableBypassPermissionsMode). Allow it there, or set the agent's approval to Read only."
        ),
    })
}

pub(crate) fn note_error(reader: &TurnReader, opts: &CliOptions<'_>) {
    if reader.expect_mode == Some("auto") && reader.result_text.to_ascii_lowercase().contains("auto mode is unavailable") {
        detect::note_auto_refused(opts);
    }
}

pub(crate) struct TempMcpConfig {
    path: PathBuf,
}

impl AsRef<Path> for TempMcpConfig {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempMcpConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) async fn write_mcp_config_temp_file(mcp_json: &str) -> Result<TempMcpConfig> {
    let path = crate::cli::temp_dir().join(format!("loka-claude-mcp-{}.json", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let file = options.open(&path).await.with_context(|| {
        format!("Failed to create Claude MCP config temp file: {}", path.display())
    })?;
    let temp = TempMcpConfig { path };
    let mut file = file;
    file.write_all(mcp_json.as_bytes()).await.with_context(|| {
        format!("Failed to write Claude MCP config temp file: {}", temp.path.display())
    })?;
    file.flush().await.with_context(|| {
        format!("Failed to flush Claude MCP config temp file: {}", temp.path.display())
    })?;
    drop(file);
    give_to_job_user(&temp.path)?;
    Ok(temp)
}

pub(crate) async fn user_line(prompt: &str, paths: &[std::path::PathBuf]) -> Result<String> {
    use base64::Engine;
    let mut content = vec![serde_json::json!({ "type": "text", "text": prompt })];
    for path in paths {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read image {}", path.display()))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let media_type = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/png",
        };
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }));
    }
    let line = serde_json::json!({ "type": "user", "message": { "role": "user", "content": content } });
    Ok(format!("{line}\n"))
}

const OWN_TEMP_CLEANUP: &str = "Own Temp Cleanup: deleting files and folders this session created for its own work is ordinary cleanup and allowed, including with `rm -rf` or `find -delete`, when the paths are inside this working directory, inside this session's own TMPDIR, or exact /tmp or /var/folders paths this session made earlier (scratch clones, build output, test fixtures, logs, downloaded archives). Name the paths literally. A glob over all of /tmp, the home directory, other people's or processes' files, and anything outside those places stay blocked.";

const REPO_SCRIPTS: &str = "Repository Scripts: running scripts that belong to the repository checked out in this working directory (for example `bash ./scripts/setup.sh`, `./bin/test`, make targets, package.json scripts) or that this session wrote itself for the task is ordinary development work and allowed. Every rule about what a script does (deleting outside the workspace, reading credentials, exfiltration, changing production) still judges the script's effects.";

const OWN_BRANCH_FORCE_PUSH: &str = "Own Branch Force Push: Loka checked out this working directory's branch for this session's quest or folder session, and its owner expects history on that branch to be rewritten when the work needs it (rebasing onto the base branch, squashing, amending, removing files that should never have been committed). Running `git push --force-with-lease` (or `--force-with-lease --force-if-includes`) of the branch currently checked out here to the same-named branch on its own remote is ordinary and allowed, as long as that branch is not the repository's default branch or a release or deploy branch. This exception covers only the destination and the overwrite of that one branch: every content rule (credential leakage, sensitive data, exfiltration) still judges what the push carries, and plain `--force`, deleting branches or tags, and force pushing any other branch stay blocked.";

fn owner_allow(environment: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut allow: Vec<String> = ["$defaults", OWN_BRANCH_FORCE_PUSH, OWN_TEMP_CLEANUP, REPO_SCRIPTS].map(String::from).to_vec();
    if let Some(raw) = environment.get(loka_shared::approvals::ENV) {
        allow.extend(loka_shared::approvals::parse_env(raw).iter().map(|action| loka_shared::approvals::classifier_entry(action)));
    }
    allow
}

const READ_ONLY_TOOLS: &str = "Read,Glob,Grep,LS,WebFetch,WebSearch,TodoWrite,Task,Bash(git status),Bash(ls:*)";

fn lean_read_settings(files: &[PathBuf], working_dir: &Path, fast: bool) -> serde_json::Value {
    let allow: Vec<String> = files
        .iter()
        .filter(|path| path.is_absolute())
        .map(|path| format!("Read(/{})", path.display()))
        .collect();
    let mut deny = vec!["Read(//proc/**)".to_string()];
    if working_dir.is_absolute() {
        deny.push(format!("Read(/{}/**)", working_dir.display()));
    }
    let mut settings = serde_json::json!({ "permissions": { "allow": allow, "deny": deny } });
    if fast {
        settings["fastMode"] = serde_json::json!(true);
    }
    settings
}

const AUTH_SETTINGS: &[&str] = &["apiKeyHelper", "awsAuthRefresh", "awsCredentialExport", "forceLoginMethod", "forceLoginOrgUUID"];
const AUTH_ENV_PREFIXES: &[&str] = &["ANTHROPIC_", "AWS_", "CLAUDE_CODE_USE_", "CLOUD_ML_REGION", "VERTEX_REGION_", "GOOGLE_", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "NODE_EXTRA_CA_CERTS"];

fn auth_settings_from(text: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let settings: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut picked = serde_json::Map::new();
    for key in AUTH_SETTINGS {
        if let Some(value) = settings.get(*key) {
            picked.insert(key.to_string(), value.clone());
        }
    }
    let env: serde_json::Map<String, serde_json::Value> = settings
        .get("env")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, value)| value.is_string() && AUTH_ENV_PREFIXES.iter().any(|prefix| name.starts_with(prefix)))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if !env.is_empty() {
        picked.insert("env".into(), serde_json::Value::Object(env));
    }
    (!picked.is_empty()).then_some(picked)
}

pub(crate) fn config_dir_from(value: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    value("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| value("HOME").filter(|v| !v.is_empty()).map(|home| PathBuf::from(home).join(".claude")))
}

fn user_settings_path() -> Option<PathBuf> {
    Some(config_dir_from(&|name| std::env::var(name).ok())?.join("settings.json"))
}

fn run_settings_path(cmd: &Command) -> Option<PathBuf> {
    let env = crate::key_proxy::env_of(cmd);
    Some(config_dir_from(&|name| env.get(name).cloned())?.join("settings.json"))
}

pub(crate) fn trusted_settings_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.is_file() && (metadata.uid() == 0 || metadata.uid() == crate::cli::runner_uid()) && metadata.mode() & 0o022 == 0
        })
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn settings_trusted(path: Option<&Path>) -> bool {
    crate::cli::job_user().is_none() || path.is_none_or(|path| !path.exists() || trusted_settings_file(path))
}

pub(crate) fn user_settings(opts: &CliOptions<'_>, cmd: &Command) -> detect::UserSettings {
    if opts.lean || !settings_trusted(run_settings_path(cmd).as_deref()) {
        detect::UserSettings::AppliedByLoka
    } else {
        detect::UserSettings::ReadByCli
    }
}

pub(crate) fn user_auth_settings() -> Option<serde_json::Map<String, serde_json::Value>> {
    auth_settings_at(&user_settings_path()?)
}

pub(crate) fn auth_settings_at(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    if !trusted_settings_file(path) {
        return None;
    }
    auth_settings_from(&std::fs::read_to_string(path).ok()?)
}

pub(crate) fn settings_at(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?.as_object().cloned()
}

fn without_attribution(settings: Option<serde_json::Value>) -> serde_json::Value {
    let mut settings = settings.unwrap_or_else(|| serde_json::json!({}));
    settings["includeCoAuthoredBy"] = serde_json::json!(false);
    settings["attribution"] = serde_json::json!({ "commit": "", "pr": "" });
    settings
}

fn mcp_server_rules(mcp_config_json: Option<&str>) -> String {
    mcp_config_json
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|config| config.get("mcpServers").and_then(serde_json::Value::as_object).cloned())
        .map(|servers| servers.keys().map(|name| format!("mcp__{name}")).collect::<Vec<_>>().join(","))
        .unwrap_or_default()
}

fn auto_mode_supported(model: &str) -> bool {
    !model.to_ascii_lowercase().contains("haiku")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Permission {
    LeanReads,
    Auto,
    AutoUnavailable,
    ReadOnly,
    Bypass,
}

pub(crate) const AUTO_UNAVAILABLE_FIX: &str = "Auto mode is not available for this Claude account, model or device settings, and Loka never switches to full access on its own. Pick a model with auto mode (Sonnet or Opus), set the agent's approval to Full to allow full access, or set it to Read only.";

fn permission(opts: &CliOptions<'_>, launch: &Launch) -> Permission {
    match (opts.lean, opts.approval) {
        (true, _) if !opts.readable_files.is_empty() => Permission::LeanReads,
        (false, crate::cli::Approval::Auto) if detect::wants_auto(opts) && !launch.auto_refused && launch.caps.mode("auto") => Permission::Auto,
        (false, crate::cli::Approval::Auto) => Permission::AutoUnavailable,
        (false, crate::cli::Approval::ReadOnly) => Permission::ReadOnly,
        (false, crate::cli::Approval::Full) | (true, _) => Permission::Bypass,
    }
}

pub(crate) fn expected_mode(opts: &CliOptions<'_>, launch: &Launch) -> Option<&'static str> {
    if opts.lean {
        return None;
    }
    match permission(opts, launch) {
        Permission::Auto => Some("auto"),
        Permission::Bypass => Some("bypassPermissions"),
        Permission::LeanReads | Permission::ReadOnly | Permission::AutoUnavailable => None,
    }
}

pub(crate) fn run_command(opts: &CliOptions<'_>, launch: &Launch) -> Result<Command> {
    let mut cmd = job_command(&launch.program);
    cmd.current_dir(opts.working_dir);
    cmd.kill_on_drop(true);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    opts.sign_commits(&mut cmd);
    launch.configure(&mut cmd);
    Ok(cmd)
}

pub(crate) fn command(opts: &CliOptions<'_>, mcp_config_path: Option<&Path>, launch: &Launch) -> Result<Command> {
    let mut cmd = run_command(opts, launch)?;
    let source = user_settings(opts, &cmd);
    let tool_search = !mcp_server_rules(opts.mcp_config_json.as_deref()).is_empty() && {
        let facts = crate::world::tool_catalog::ClaudeFacts { version: launch.caps.version.clone(), first_party: detect::ClaudeEnv::of(&cmd, source).first_party_route() };
        crate::world::tool_catalog::delivery_for("claude", opts.model, &facts) == crate::world::tool_catalog::Delivery::Native
    };
    let sign_in = detect::SignIn::settle(&mut cmd, source, launch);
    let private_temp = crate::cli::private_temp_dir(opts.working_dir);
    if let Some(dir) = &private_temp {
        cmd.env("CLAUDE_CODE_TMPDIR", dir);
    }

    let configured_effort = if opts.effort.is_none() {
        match std::env::var("WORLD_CLAUDE_EFFORT") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let effort = select_effort(opts.effort, configured_effort.as_deref(), opts.lean)?;

    if tool_search {
        cmd.env("ENABLE_TOOL_SEARCH", "true");
    }
    let mode = permission(opts, launch);
    let lean_reads = mode == Permission::LeanReads;
    let dont_ask = if launch.caps.mode("dontAsk") { "dontAsk" } else { "default" };
    cmd.arg("-p");
    match mode {
        Permission::LeanReads => {
            cmd.arg("--permission-mode").arg(dont_ask);
        }
        Permission::Auto => {
            cmd.arg("--permission-mode").arg("auto");
        }
        Permission::ReadOnly => {
            cmd.arg("--permission-mode").arg(dont_ask);
            cmd.arg("--allowedTools").arg(READ_ONLY_TOOLS);
        }
        Permission::Bypass => {
            cmd.arg("--dangerously-skip-permissions");
        }
        Permission::AutoUnavailable => {
            anyhow::bail!("Claude Code cannot start in auto mode for model {}. {AUTO_UNAVAILABLE_FIX}", opts.model);
        }
    }
    cmd.arg("--verbose");
    cmd.arg("--output-format").arg("stream-json");
    cmd.arg("--max-turns").arg(opts.max_turns.to_string());
    cmd.arg("--model").arg(opts.model);
    if let Some(level) = launch.caps.effort(effort) {
        cmd.arg("--effort").arg(level);
    }
    let sandbox_mode = if opts.lean { sandbox::Mode::Off } else { sandbox::mode() };
    let writable = if sandbox_mode == sandbox::Mode::Off { Vec::new() } else { sandbox::git_write_roots(opts.working_dir) };
    let settings = (!lean_reads).then(|| sandbox::settings(sandbox_mode, &opts.environment, mcp_config_path, &writable, opts.fast)).flatten();
    let settings = if opts.lean { settings } else { Some(without_attribution(settings)) };
    let skip_user_settings = !opts.lean && !sign_in.reads_user_settings();
    let settings = if lean_reads { settings } else { sign_in.settings(settings) };
    if let Some(mut settings) = settings {
        sandbox::open_for_owner(&mut settings);
        if let Some(dir) = &private_temp {
            sandbox::allow_private_dir(&mut settings, dir);
        }
        if sandbox_mode != sandbox::Mode::Off {
            cmd.env("GIT_TEMPLATE_DIR", "");
            sandbox::owner_git_env(&mut cmd);
        }
        if mode == Permission::Auto {
            settings["autoMode"] = serde_json::json!({ "allow": owner_allow(&opts.environment) });
        }
        cmd.arg("--settings").arg(settings.to_string());
        if let Some(token) = (sandbox_mode != sandbox::Mode::Off).then(|| sandbox::github_token(&opts.environment)).flatten() {
            cmd.env("GH_TOKEN", token);
        }
    }
    cmd.arg("--disallowed-tools").arg("AskUserQuestion");

    if let Some(sid) = opts.session_id {
        if opts.resume {
            cmd.arg("--resume").arg(sid);
            if let Some(at) = opts.rewind_to {
                cmd.arg("--resume-session-at").arg(at);
            }
            if opts.fork || opts.rewind_to.is_some() {
                cmd.arg("--fork-session");
            }
        } else {
            cmd.arg("--session-id").arg(sid);
        }
    }

    let system_prompt = if opts.lean {
        opts.system_prompt.unwrap_or_default().to_string()
    } else {
        opts.system_prompt
            .map(|s| format!("{AGENT_SYSTEM_PROMPT}\n\n{s}"))
            .unwrap_or_else(|| AGENT_SYSTEM_PROMPT.to_string())
    };
    if !system_prompt.is_empty() {
        cmd.arg("--system-prompt").arg(&system_prompt);
    }

    if lean_reads {
        cmd.arg("--tools").arg(if tool_search { "Read,ToolSearch" } else { "Read" });
        cmd.arg("--setting-sources").arg("");
        let read_settings = Some(lean_read_settings(&opts.readable_files, opts.working_dir, opts.fast));
        let settings = sign_in.settings(read_settings).unwrap_or_default();
        cmd.arg("--settings").arg(settings.to_string());
        let servers = mcp_server_rules(opts.mcp_config_json.as_deref());
        if !servers.is_empty() {
            cmd.arg("--allowedTools").arg(servers);
        }
    } else if opts.lean {
        cmd.arg("--tools").arg(if tool_search { "ToolSearch" } else { "" });
        cmd.arg("--setting-sources").arg("");
    } else if let Some(tools) = opts.allowed_tools.filter(|_| opts.approval != crate::cli::Approval::ReadOnly) {
        cmd.arg("--allowedTools").arg(tools);
    }
    if skip_user_settings {
        cmd.arg("--setting-sources").arg("project,local");
    }

    cmd.arg("--strict-mcp-config");
    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config").arg(path);
    }

    crate::key_proxy::shield(&mut cmd, &[crate::key_proxy::ANTHROPIC]);
    if !opts.lean {
        crate::cmd_hub::shield(&mut cmd, &opts.environment, crate::cmd_hub::Hook::ClaudeShell);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    Ok(cmd)
}

pub(crate) struct TurnReader {
    started: Instant,
    pub result_text: String,
    pub session_id: Option<String>,
    pub cost_usd: f64,
    pub usage: crate::cli::Usage,
    pub is_error_response: bool,
    pub timing: Timing,
    pub checkpoint: Option<String>,
    pub expect_mode: Option<&'static str>,
    pub refused_mode: Option<String>,
    tool_use_names: HashMap<String, String>,
    last_context_tokens: u64,
    live_calls: HashMap<String, crate::cli::Usage>,
}

impl TurnReader {
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            result_text: String::new(),
            session_id: None,
            cost_usd: 0.0,
            usage: crate::cli::Usage::default(),
            is_error_response: false,
            timing: Timing::default(),
            checkpoint: None,
            expect_mode: None,
            refused_mode: None,
            tool_use_names: HashMap::new(),
            last_context_tokens: 0,
            live_calls: HashMap::new(),
        }
    }

    pub(crate) fn expecting(mut self, mode: Option<&'static str>) -> Self {
        self.expect_mode = mode;
        self
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub(crate) fn feed(&mut self, json: &serde_json::Value, event_tx: Option<&crate::cli::EventSender>) -> bool {
        if let Some(sid) = session_id_from_init("claude", json) {
            if self.timing.init_ms.is_none() {
                self.timing.init_ms = Some(self.elapsed_ms());
            }
            if self.session_id.is_none() {
                self.session_id = Some(sid.to_string());
                if let Some(tx) = event_tx {
                    let _ = tx.send(StreamEvent::SessionStarted {
                        session_id: sid.to_string(),
                    });
                }
            }
        }

        let kind = json.get("type").and_then(|v| v.as_str());
        let startup = kind == Some("system")
            && matches!(json.get("subtype").and_then(|v| v.as_str()), Some("init" | "status"))
            && self.timing.first_output_ms.is_none();
        if startup && self.expect_mode.is_some_and(|expected| expected != "default") {
            if let Some(actual) = json.get("permissionMode").and_then(|v| v.as_str()).filter(|mode| *mode == "default") {
                self.refused_mode = Some(actual.to_string());
            }
        }
        if matches!(kind, Some("assistant" | "user")) {
            if let Some(uuid) = json.get("uuid").and_then(|v| v.as_str()) {
                self.checkpoint = Some(uuid.to_string());
            }
        }
        match kind {
            Some("assistant") => {
                let now = self.elapsed_ms();
                self.timing.first_output_ms.get_or_insert(now);
                self.timing.last_output_ms = Some(now);
                if let Some(usage) = json.pointer("/message/usage") {
                    let n = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    let context = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                    if context > 0 {
                        self.last_context_tokens = context;
                    }
                    let call = crate::cli::Usage {
                        input_tokens: n("input_tokens") + n("cache_creation_input_tokens"),
                        cached_input_tokens: n("cache_read_input_tokens"),
                        output_tokens: n("output_tokens"),
                        context_tokens: context,
                    };
                    let id = json.pointer("/message/id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    self.live_calls.insert(id, call);
                    if let Some(tx) = event_tx {
                        let _ = tx.send(StreamEvent::Usage(crate::cli::Usage::sum(self.live_calls.values())));
                    }
                }
                let Some(content) = json.pointer("/message/content").and_then(|v| v.as_array()) else {
                    return false;
                };
                for block in content {
                    if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                        continue;
                    }
                    self.timing.tool_calls += 1;
                    let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    if let Some(tool_use_id) = block.get("id").and_then(|v| v.as_str()) {
                        self.tool_use_names.insert(tool_use_id.to_string(), tool.to_string());
                    }
                    if let Some(tx) = event_tx {
                        let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        let _ = tx.send(StreamEvent::ToolUse {
                            tool: tool.to_string(),
                            input_summary: summarize_tool_input(tool, &input),
                        });
                    }
                }
            }
            Some("user") => {
                let Some(tx) = event_tx else {
                    return false;
                };
                let Some(content) = json.pointer("/message/content") else {
                    return false;
                };
                let blocks: Vec<&serde_json::Value> = if let Some(array) = content.as_array() {
                    array.iter().collect()
                } else {
                    vec![content]
                };
                for block in blocks {
                    if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let tool = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .and_then(|tool_use_id| self.tool_use_names.get(tool_use_id))
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    let output = block.get("content").map(summarize_tool_output).unwrap_or_default();
                    let output = output.trim().to_string();
                    if output.is_empty() {
                        continue;
                    }
                    let _ = tx.send(StreamEvent::ToolResult { tool, output });
                }
            }
            Some("result") => {
                self.timing.result_ms = Some(self.elapsed_ms());
                self.timing.api_ms = json.get("duration_api_ms").and_then(|v| v.as_u64());
                self.timing.model_turns = json.get("num_turns").and_then(|v| v.as_u64());
                self.result_text = json.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                    self.session_id = Some(sid.to_string());
                }
                self.cost_usd = json.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                self.is_error_response = is_error_result(json);
                if let Some(u) = json.get("usage") {
                    let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    self.usage = crate::cli::Usage {
                        input_tokens: n("input_tokens") + n("cache_creation_input_tokens"),
                        cached_input_tokens: n("cache_read_input_tokens"),
                        output_tokens: n("output_tokens"),
                        context_tokens: self.last_context_tokens,
                    };
                }
                return true;
            }
            _ => {}
        }
        false
    }

    pub(crate) fn into_result(self, exit_code: i32) -> CliResult {
        CliResult {
            output: self.result_text,
            session_id: self.session_id,
            cost_usd: self.cost_usd,
            exit_code,
            usage: self.usage,
            checkpoint: self.checkpoint,
        }
    }
}

async fn run_inner(opts: &CliOptions<'_>, mcp_config_path: Option<&Path>, launch: &Launch) -> Result<(CliResult, Timing)> {
    let started = Instant::now();
    let mut cmd = command(opts, mcp_config_path, launch)?;
    let expect = expected_mode(opts, launch);

    if let Some(steer) = opts.steer.take() {
        return run_steerable(opts, cmd, steer, started, expect).await;
    }
    if !opts.lean {
        let (_idle, steer) = tokio::sync::mpsc::unbounded_channel();
        return run_steerable(opts, cmd, steer, started, expect).await;
    }

    let stdin_payload = if opts.image_paths.is_empty() {
        opts.prompt.to_string()
    } else {
        cmd.arg("--input-format").arg("stream-json");
        info!(image_count = opts.image_paths.len(), "Attaching images to Claude Code");
        user_line(opts.prompt, &opts.image_paths).await?
    };

    info!(
        dir = %opts.working_dir.display(),
        model = opts.model,
        max_turns = opts.max_turns,
        "Invoking Claude Code"
    );

    let mut child = spawn(&mut cmd)?;
    crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_payload.as_bytes()).await?;
        drop(stdin);
    }

    let stderr_handle = {
        let stderr = child.stderr.take().expect("stderr piped");
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            reader.read_to_string(&mut buf).await.ok();
            buf
        })
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut reader = TurnReader::new(started).expecting(expect);
    reader.timing.spawned = true;
    reader.timing.spawn_ms = started.elapsed().as_millis() as u64;

    while let Some(line) = lines.next_line().await? {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let done = reader.feed(&json, opts.event_tx.as_ref());
        if let Some(error) = refusal(&reader, opts) {
            let _ = child.kill().await;
            return Err(error);
        }
        if done {
            break;
        }
    }

    if reader.timing.result_ms.is_some() && !reader.is_error_response {
        let uid = crate::cli::JobUid::current();
        tokio::spawn(async move {
            let drain = async {
                while let Ok(Some(_)) = lines.next_line().await {}
                child.wait().await
            };
            if tokio::time::timeout(std::time::Duration::from_secs(30), drain).await.is_err() {
                let _ = child.kill().await;
            }
            drop(stderr_handle);
            drop(uid);
        });
        info!(
            cost_usd = reader.cost_usd,
            session_id = reader.session_id.as_deref().unwrap_or("none"),
            "Claude Code completed"
        );
        let timing = reader.timing;
        return Ok((reader.into_result(0), timing));
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = collect_stderr(stderr_handle).await;

    debug!(exit_code, stderr_len = stderr.len(), "Claude Code finished");

    if !stderr.is_empty()
        && (opts.environment.is_empty() && opts.provider_keys.is_empty())
        && !opts.mcp_has_secrets
        && !opts.sensitive
    {
        debug!(stderr = %stderr, "Claude stderr");
    }

    if reader.is_error_response {
        note_error(&reader, opts);
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::Error(reader.result_text.clone()));
        }
        anyhow::bail!("Claude Code returned error: {}", reader.result_text);
    }

    if exit_code != 0 && reader.result_text.is_empty() {
        anyhow::bail!("Claude Code exited with code {exit_code}: {stderr}");
    }

    info!(
        exit_code,
        cost_usd = reader.cost_usd,
        session_id = reader.session_id.as_deref().unwrap_or("none"),
        "Claude Code completed"
    );

    let timing = reader.timing;
    Ok((reader.into_result(exit_code), timing))
}

fn is_error_result(json: &serde_json::Value) -> bool {
    match json.get("subtype").and_then(|v| v.as_str()) {
        Some("error_response") => true,
        Some("success") => json.get("is_error").and_then(|v| v.as_bool()) == Some(true),
        _ => false,
    }
}

fn interrupt_line() -> String {
    let line = serde_json::json!({
        "type": "control_request",
        "request_id": uuid::Uuid::new_v4().to_string(),
        "request": { "subtype": "interrupt" },
    });
    format!("{line}\n")
}

pub(crate) const BACKGROUND_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

const BACKGROUND_ASKS: u8 = 3;

#[derive(Default)]
pub(crate) struct Background {
    running: std::collections::BTreeMap<String, String>,
    waiting_since: Option<Instant>,
    ended_at: Option<Instant>,
    asked: bool,
    asks: u8,
    gave_up: bool,
    turn_limit: Option<u32>,
    turns: u32,
    turn_ids: std::collections::HashSet<String>,
}

impl Background {
    pub(crate) fn within(max_turns: u32) -> Self {
        Self { turn_limit: (max_turns > 0).then_some(max_turns), ..Self::default() }
    }

    fn out_of_turns(&self) -> bool {
        self.turn_limit.is_some_and(|limit| self.turns >= limit)
    }

    pub(crate) fn track(&mut self, json: &serde_json::Value) {
        match json.get("type").and_then(|v| v.as_str()) {
            Some("assistant") if json.get("parent_tool_use_id").is_none_or(serde_json::Value::is_null) => {
                self.ended_at = None;
                match json.pointer("/message/id").and_then(|v| v.as_str()) {
                    Some(id) if !self.turn_ids.insert(id.to_string()) => {}
                    _ => self.turns += 1,
                }
            }
            Some("system") => {
                let Some(id) = json.get("task_id").and_then(|v| v.as_str()) else {
                    return;
                };
                match json.get("subtype").and_then(|v| v.as_str()) {
                    Some("task_started") => {
                        let what = json.get("description").and_then(|v| v.as_str()).unwrap_or("a background task");
                        self.running.insert(id.to_string(), what.trim().chars().take(120).collect());
                    }
                    Some("task_notification") => {
                        self.running.remove(id);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pub(crate) fn keep_open(&mut self, now: Instant, cap: Option<std::time::Duration>) -> bool {
        if self.running.is_empty() || self.gave_up || cap.is_none() || self.out_of_turns() {
            self.waiting_since = None;
            self.ended_at = None;
            return false;
        }
        self.waiting_since.get_or_insert(now);
        self.ended_at = Some(now);
        self.asked = false;
        true
    }

    pub(crate) fn note(&mut self, now: Instant, cap: Option<std::time::Duration>) -> Option<String> {
        let (since, ended, cap) = (self.waiting_since?, self.ended_at?, cap?);
        if self.asked || self.out_of_turns() {
            return None;
        }
        let running: Vec<&str> = self.running.values().map(String::as_str).collect();
        if now.duration_since(since) >= cap {
            self.asked = true;
            self.gave_up = true;
            return Some(stop_waiting_note(&running, cap));
        }
        if running.is_empty() {
            self.asked = true;
            return Some(BACKGROUND_DONE_NOTE.to_string());
        }
        if self.asks < BACKGROUND_ASKS && now.duration_since(ended) >= BACKGROUND_GRACE {
            self.asked = true;
            self.asks += 1;
            return Some(background_note(&running));
        }
        None
    }
}

pub(crate) const BACKGROUND_DONE_NOTE: &str = "[background tasks finished]\nThe background tasks you were waiting on have finished. Read their output, then finish your answer exactly as your task asks, including any report or verdict block it asks for.";

pub(crate) fn background_note(running: &[&str]) -> String {
    format!(
        "[background tasks still running]\nYour turn ended while these background tasks were still running: {}. Loka keeps this run open for them, and they stop when the run ends. Wait for them in the foreground now (block on their output, do not start them again), read the results, then finish your answer exactly as your task asks, including any report or verdict block it asks for.",
        running.join("; ")
    )
}

pub(crate) fn stop_waiting_note(running: &[&str], cap: std::time::Duration) -> String {
    let what = if running.is_empty() { "Your background tasks".to_string() } else { format!("Your background tasks ({})", running.join("; ")) };
    format!(
        "[stop waiting]\n{what} ran for {} minutes, longer than Loka waits. Stop waiting for them. Finish your answer now with what you have, say which checks did not finish, and include any report or verdict block your task asks for.",
        cap.as_secs().div_ceil(60)
    )
}

pub(crate) struct SteerState {
    sent: usize,
    replayed: usize,
    interrupted: bool,
    tools: crate::cli::OpenTools,
    pub(crate) background: Background,
}

impl SteerState {
    pub(crate) fn new(max_turns: u32) -> Self {
        Self { sent: 1, replayed: 0, interrupted: false, tools: crate::cli::OpenTools::new(), background: Background::within(max_turns) }
    }

    pub(crate) fn oldest_tool(&self) -> Option<std::time::Duration> {
        self.tools.oldest()
    }

    pub(crate) fn stuck_longer_than(&self, limit: std::time::Duration) -> bool {
        self.tools.stuck_longer_than(limit)
    }

    pub(crate) fn track(&mut self, json: &serde_json::Value) {
        self.background.track(json);
        let content = json.pointer("/message/content").and_then(|v| v.as_array());
        match json.get("type").and_then(|v| v.as_str()) {
            Some("user") if json.get("isReplay").and_then(|v| v.as_bool()) == Some(true) => self.replayed += 1,
            Some("assistant") => {
                for block in content.into_iter().flatten() {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                            self.tools.start(id);
                        }
                    }
                }
            }
            Some("user") => {
                for block in content.into_iter().flatten() {
                    if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                        self.tools.finish(id);
                    }
                }
            }
            Some("result") => self.tools.clear(),
            _ => {}
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.sent > self.replayed
    }

    pub(crate) async fn write(&mut self, stdin: &mut tokio::process::ChildStdin, text: &str, interrupt: bool) -> Result<()> {
        let mut text = text.to_string();
        if interrupt {
            stdin.write_all(interrupt_line().as_bytes()).await?;
            self.interrupted = true;
            self.tools.clear();
            text = format!("{}\n\n{text}", crate::cli::INTERRUPTED_NOTE);
        }
        stdin.write_all(user_line(&text, &[]).await?.as_bytes()).await?;
        stdin.flush().await?;
        self.sent += 1;
        Ok(())
    }
}

async fn run_steerable(
    opts: &CliOptions<'_>,
    mut cmd: Command,
    mut steer: crate::cli::SteerReceiver,
    started: Instant,
    expect: Option<&'static str>,
) -> Result<(CliResult, Timing)> {
    cmd.arg("--input-format").arg("stream-json");
    cmd.arg("--replay-user-messages");
    info!(
        dir = %opts.working_dir.display(),
        model = opts.model,
        max_turns = opts.max_turns,
        "Invoking Claude Code with steering"
    );
    let mut child = spawn(&mut cmd)?;
    crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);
    let mut stdin = child.stdin.take().context("claude stdin missing")?;
    stdin.write_all(user_line(opts.prompt, &opts.image_paths).await?.as_bytes()).await?;
    stdin.flush().await?;
    let stderr = crate::cli::stderr_tail(child.stderr.take(), opts.redactor());
    let stdout = child.stdout.take().expect("stdout piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut reader = TurnReader::new(started).expecting(expect);
    reader.timing.spawned = true;
    reader.timing.spawn_ms = started.elapsed().as_millis() as u64;

    let interrupt_after = crate::cli::steer_interrupt_after();
    let hang_after = crate::cli::tool_hang_after();
    let background_cap = crate::cli::background_wait_cap();
    let mut state = SteerState::new(opts.max_turns);
    let mut outputs: Vec<String> = Vec::new();
    let mut watchdog = tokio::time::interval(std::time::Duration::from_secs(15));
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut finished = false;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                state.track(&json);
                let done = reader.feed(&json, opts.event_tx.as_ref());
                if let Some(error) = refusal(&reader, opts) {
                    crate::cli::close_steers(&mut steer);
                    let _ = child.kill().await;
                    return Err(error);
                }
                if !done {
                    continue;
                }
                if reader.is_error_response {
                    break;
                }
                if !reader.result_text.trim().is_empty() {
                    outputs.push(reader.result_text.clone());
                }
                while let Ok(next) = steer.try_recv() {
                    state.write(&mut stdin, &next.text, false).await?;
                    let _ = next.ack.send(true);
                }
                if !state.pending() {
                    if state.background.keep_open(Instant::now(), background_cap) {
                        info!("Claude turn ended with background tasks running, keeping the run open");
                        continue;
                    }
                    finished = true;
                    break;
                }
            }
            Some(next) = steer.recv() => {
                let interrupt = next.interrupt || state.tools.stuck_longer_than(interrupt_after);
                let written = state.write(&mut stdin, &next.text, interrupt).await;
                let _ = next.ack.send(written.is_ok());
                written?;
                info!(interrupt, "Steered running Claude session");
            }
            _ = watchdog.tick() => {
                if let (Some(limit), Some(age)) = (hang_after, state.tools.oldest()) {
                    if age >= limit {
                        warn!(secs = age.as_secs(), "Claude tool call looks stuck, interrupting");
                        state.write(&mut stdin, &crate::cli::hang_notice(age), true).await?;
                    }
                }
                if let Some(note) = state.background.note(Instant::now(), background_cap) {
                    info!("Asking Claude to finish after its background tasks");
                    state.write(&mut stdin, &note, false).await?;
                }
            }
        }
    }
    crate::cli::close_steers(&mut steer);
    drop(stdin);
    if state.interrupted {
        info!("Claude session was interrupted and resumed");
    }

    if finished {
        let uid = crate::cli::JobUid::current();
        tokio::spawn(async move {
            let drain = async {
                while let Ok(Some(_)) = lines.next_line().await {}
                child.wait().await
            };
            if tokio::time::timeout(std::time::Duration::from_secs(30), drain).await.is_err() {
                let _ = child.kill().await;
            }
            drop(uid);
        });
        reader.result_text = outputs.join("\n\n");
        let timing = reader.timing;
        return Ok((reader.into_result(0), timing));
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    if reader.is_error_response {
        note_error(&reader, opts);
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::Error(reader.result_text.clone()));
        }
        anyhow::bail!("Claude Code returned error: {}", reader.result_text);
    }
    if outputs.is_empty() {
        let tail = stderr.lock().unwrap().trim().to_string();
        anyhow::bail!("Claude Code exited with code {exit_code}: {tail}");
    }
    reader.result_text = outputs.join("\n\n");
    let timing = reader.timing;
    Ok((reader.into_result(exit_code), timing))
}

fn select_effort<'a>(
    explicit: Option<&'a str>,
    configured: Option<&'a str>,
    lean: bool,
) -> Result<&'a str> {
    if let Some(effort) = explicit {
        return Ok(effort);
    }
    if let Some(effort) = configured {
        anyhow::ensure!(
            matches!(effort, "low" | "medium" | "high" | "xhigh" | "max"),
            "invalid WORLD_CLAUDE_EFFORT `{effort}`; expected low, medium, high, xhigh, or max"
        );
        return Ok(effort);
    }
    Ok(if lean { "medium" } else { "high" })
}

#[cfg(test)]
mod tests {
    use super::detect::{Caps, Launch};
    use super::{auth_settings_from, command, expected_mode, is_error_result, refusal, select_effort, user_auth_settings, write_mcp_config_temp_file, Background, TurnReader, BACKGROUND_ASKS, BACKGROUND_DONE_NOTE, BACKGROUND_GRACE};
    use std::time::{Duration, Instant};
    use crate::cli::{Approval, CliOptions};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn options(lean: bool, readable_files: Vec<PathBuf>, fast: bool, approval: Approval, model: &'static str) -> CliOptions<'static> {
        CliOptions {
            working_dir: Path::new("/tmp"),
            prompt: "hi",
            system_prompt: Some("persona"),
            allowed_tools: None,
            max_turns: 2,
            model,
            mcp_config_json: Some(r#"{"mcpServers":{"world":{"command":"loka-agent"}}}"#.into()),
            session_id: None,
            resume: false,
            event_tx: None,
            image_paths: Vec::new(),
            fork: false,
            lean,
            effort: Some("low"),
            fast,
            environment: Default::default(),
            mcp_has_secrets: false,
            sensitive: false,
            rewind_to: None,
            approval,
            steer: Default::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files,
        }
    }

    fn args(opts: &CliOptions<'_>, launch: &Launch) -> Vec<String> {
        let cmd = command(opts, None, launch).expect("command builds");
        cmd.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    fn lean_args(readable_files: Vec<PathBuf>, fast: bool) -> Vec<String> {
        args(&options(true, readable_files, fast, Approval::Full, "sonnet"), &Launch::assume("claude"))
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).map(String::as_str)
    }

    #[test]
    fn work_runs_never_add_claude_attribution() {
        for approval in [Approval::Auto, Approval::Full] {
            let work = args(&options(false, Vec::new(), false, approval, "sonnet"), &Launch::assume("claude"));
            assert_eq!(work.iter().filter(|arg| *arg == "--settings").count(), 1);
            let settings: serde_json::Value = serde_json::from_str(after(&work, "--settings").unwrap()).unwrap();
            assert_eq!(settings["includeCoAuthoredBy"], serde_json::json!(false));
            assert_eq!(settings["attribution"], serde_json::json!({ "commit": "", "pr": "" }));
        }
    }

    #[test]
    fn owner_auto_runs_may_force_push_their_own_branch() {
        let auto = args(&options(false, Vec::new(), false, Approval::Auto, "sonnet"), &Launch::assume("claude"));
        let settings: serde_json::Value = serde_json::from_str(after(&auto, "--settings").unwrap()).unwrap();
        assert_eq!(after(&auto, "--permission-mode"), Some("auto"));
        assert_eq!(settings["autoMode"]["allow"], serde_json::json!(["$defaults", super::OWN_BRANCH_FORCE_PUSH, super::OWN_TEMP_CLEANUP, super::REPO_SCRIPTS]));
        let mut approved = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        approved.environment.insert(loka_shared::approvals::ENV.into(), serde_json::json!(["Delete the build cache in ~/Library/Caches/acme"]).to_string());
        let with = args(&approved, &Launch::assume("claude"));
        let settings: serde_json::Value = serde_json::from_str(after(&with, "--settings").unwrap()).unwrap();
        let allow = settings["autoMode"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 5);
        assert!(allow[4].as_str().unwrap().starts_with("Owner Approved:") && allow[4].as_str().unwrap().contains("~/Library/Caches/acme"));
        let full = args(&options(false, Vec::new(), false, Approval::Full, "sonnet"), &Launch::assume("claude"));
        let settings: serde_json::Value = serde_json::from_str(after(&full, "--settings").unwrap()).unwrap();
        assert!(settings.get("autoMode").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn job_writable_settings_are_never_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("settings-trust-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(super::trusted_settings_file(&path));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(!super::trusted_settings_file(&path), "group writable settings could be planted by a job");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), &path).unwrap();
        assert!(!super::trusted_settings_file(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    fn launch_with_version(version: Option<&str>) -> Launch {
        Launch { caps: Arc::new(Caps::parse(version.map(str::to_string), "")), ..Launch::assume("claude") }
    }

    fn on_route(mut opts: CliOptions<'static>, base_url: Option<&str>, cloud: Option<&str>) -> CliOptions<'static> {
        opts.environment.insert("ANTHROPIC_BASE_URL".to_string(), base_url.unwrap_or("https://api.anthropic.com").to_string());
        if let Some(name) = cloud {
            opts.environment.insert(name.to_string(), "1".to_string());
        }
        opts
    }

    fn first_party(opts: CliOptions<'static>) -> CliOptions<'static> {
        on_route(opts, None, None)
    }

    fn built(opts: &CliOptions<'_>, launch: &Launch) -> (Vec<String>, Option<String>) {
        let cmd = command(opts, None, launch).expect("command builds");
        let args = cmd.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let env = cmd.as_std().get_envs().find(|(key, _)| *key == "ENABLE_TOOL_SEARCH").and_then(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()));
        (args, env)
    }

    #[test]
    fn lean_turns_load_world_tools_through_tool_search_on_a_cli_that_has_it() {
        let launch = launch_with_version(Some("2.1.330"));
        let (bare, env) = built(&first_party(options(true, Vec::new(), false, Approval::Full, "sonnet")), &launch);
        assert_eq!(after(&bare, "--tools"), Some("ToolSearch"));
        assert_eq!(env.as_deref(), Some("true"));
        let (reads, env) = built(&first_party(options(true, vec![PathBuf::from("/runner/skills/notes.md")], false, Approval::Full, "opus")), &launch);
        assert_eq!(after(&reads, "--tools"), Some("Read,ToolSearch"));
        assert_eq!(after(&reads, "--allowedTools"), Some("mcp__world"));
        assert_eq!(env.as_deref(), Some("true"));
        let (work, env) = built(&first_party(options(false, Vec::new(), false, Approval::Auto, "sonnet")), &launch);
        assert_eq!(after(&work, "--tools"), None);
        assert_eq!(env.as_deref(), Some("true"));
    }

    #[test]
    fn lean_turns_stay_as_they_were_without_tool_search() {
        for (version, model) in [(Some("2.1.6"), "sonnet"), (None, "sonnet"), (Some("2.1.330"), "glm-5")] {
            let (bare, env) = built(&first_party(options(true, Vec::new(), false, Approval::Full, model)), &launch_with_version(version));
            assert_eq!(after(&bare, "--tools"), Some(""), "{version:?} {model}");
            assert_eq!(env, None, "{version:?} {model}");
        }
        let mut no_servers = first_party(options(true, Vec::new(), false, Approval::Full, "sonnet"));
        no_servers.mcp_config_json = None;
        let (bare, env) = built(&no_servers, &launch_with_version(Some("2.1.330")));
        assert_eq!((after(&bare, "--tools"), env), (Some(""), None));
    }

    #[test]
    fn a_gateway_or_cloud_route_never_gets_tool_search() {
        let launch = launch_with_version(Some("2.1.330"));
        for (base_url, cloud) in [(Some("http://localhost:4000"), None), (Some("https://openrouter.ai/api"), None), (None, Some("CLAUDE_CODE_USE_BEDROCK")), (None, Some("CLAUDE_CODE_USE_VERTEX")), (None, Some("CLAUDE_CODE_USE_FOUNDRY"))] {
            let (bare, env) = built(&on_route(options(true, Vec::new(), false, Approval::Full, "sonnet"), base_url, cloud), &launch);
            assert_eq!((after(&bare, "--tools"), env), (Some(""), None), "{base_url:?} {cloud:?}");
        }
    }

    #[test]
    fn the_launch_and_the_kit_make_the_same_delivery_decision() {
        use crate::world::tool_catalog::{delivery_for, ClaudeFacts, Delivery};
        for version in [None, Some("2.1.6"), Some("2.1.7"), Some("2.1.330")] {
            for model in ["sonnet", "opus", "glm-5"] {
                for (base_url, cloud) in [(None, None), (Some("http://localhost:4000"), None), (None, Some("CLAUDE_CODE_USE_BEDROCK"))] {
                    let opts = on_route(options(true, Vec::new(), false, Approval::Full, model), base_url, cloud);
                    let (_, env) = built(&opts, &launch_with_version(version));
                    let kit = delivery_for("claude", model, &ClaudeFacts { version: version.map(str::to_string), first_party: super::detect::route_is_first_party(&super::detect::host_route_lookup(&opts.environment)) });
                    assert_eq!(env.is_some(), kit == Delivery::Native, "{version:?} {model} {base_url:?} {cloud:?}");
                }
            }
        }
    }

    #[test]
    fn lean_turns_read_only_their_skill_files() {
        let skill = PathBuf::from("/runner/skills/session/atlas-notes.md");
        let args = lean_args(vec![skill, PathBuf::from("relative.md")], false);
        assert_eq!(after(&args, "--tools"), Some("Read"));
        assert_eq!(after(&args, "--permission-mode"), Some("dontAsk"));
        assert!(!args.iter().any(|arg| arg == "--dangerously-skip-permissions"));
        let settings: serde_json::Value = serde_json::from_str(after(&args, "--settings").unwrap()).unwrap();
        assert_eq!(settings["permissions"]["allow"], serde_json::json!(["Read(//runner/skills/session/atlas-notes.md)"]));
        assert_eq!(after(&args, "--allowedTools"), Some("mcp__world"));
        assert_eq!(settings["permissions"]["deny"], serde_json::json!(["Read(//proc/**)", "Read(//tmp/**)"]));

        let bare = lean_args(Vec::new(), false);
        assert_eq!(after(&bare, "--tools"), Some(""));
        assert!(bare.iter().any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(!bare.iter().any(|arg| arg == "--settings") || user_auth_settings().is_some());

        let fast = lean_args(vec![PathBuf::from("/tmp/skills/notes.md")], true);
        assert_eq!(fast.iter().filter(|arg| *arg == "--settings").count(), 1);
        let settings: serde_json::Value = serde_json::from_str(after(&fast, "--settings").unwrap()).unwrap();
        assert_eq!(settings["fastMode"], serde_json::json!(true));
        assert_eq!(settings["permissions"]["deny"], serde_json::json!(["Read(//proc/**)", "Read(//tmp/**)"]));
    }

    #[test]
    fn owner_runs_skip_git_hook_templates_so_init_and_clone_work_beside_the_protected_hooks_dir() {
        let env = |opts: &CliOptions<'_>| {
            let cmd = command(opts, None, &Launch::assume("claude")).expect("command builds");
            cmd.as_std().get_envs().find(|(name, _)| *name == "GIT_TEMPLATE_DIR").map(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()))
        };
        let owner = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        assert_eq!(env(&owner), Some(Some(String::new())));
        let lean = options(true, Vec::new(), false, Approval::Auto, "sonnet");
        assert_eq!(env(&lean), None);
        let dir = std::env::temp_dir().join(format!("claude-templates-{}", uuid::Uuid::new_v4().simple()));
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&dir)
            .env("GIT_TEMPLATE_DIR", "")
            .status()
            .unwrap();
        assert!(init.success());
        assert!(!dir.join(".git/hooks").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn owner_runs_get_git_config_that_avoids_shared_config_writes() {
        let env = |opts: &CliOptions<'_>| {
            let cmd = command(opts, None, &Launch::assume("claude")).expect("command builds");
            cmd.as_std()
                .get_envs()
                .filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned())))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let owner = env(&options(false, Vec::new(), false, Approval::Auto, "sonnet"));
        let count: usize = owner["GIT_CONFIG_COUNT"].parse().unwrap();
        assert!(count >= 1);
        let entries: Vec<(&str, &str)> = (count - 1..count).map(|i| (owner[&format!("GIT_CONFIG_KEY_{i}")].as_str(), owner[&format!("GIT_CONFIG_VALUE_{i}")].as_str())).collect();
        assert_eq!(entries, crate::claude::sandbox::OWNER_GIT_CONFIG);
        assert!(!env(&options(true, Vec::new(), false, Approval::Auto, "sonnet")).contains_key("GIT_CONFIG_COUNT"));
    }

    #[test]
    fn quest_claude_runs_get_their_own_writable_temp_dir() {
        let id = uuid::Uuid::new_v4();
        let tree: &'static Path = Box::leak(Box::new(PathBuf::from(format!("/Users/a/Library/Application Support/dev.ygpt.loka/runner/workspaces/tasks/{id}/world"))));
        let expected = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id));
        let launch = Launch::assume("claude");
        let build = |working_dir: &'static Path| {
            let mut opts = options(false, Vec::new(), false, Approval::Auto, "sonnet");
            opts.working_dir = working_dir;
            command(&opts, None, &launch)
        };
        let cmd = build(tree).expect("owner command builds");
        let env: std::collections::BTreeMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned())))
            .collect();
        assert_eq!(env["TMPDIR"], expected.to_string_lossy());
        assert_eq!(env["CLAUDE_CODE_TMPDIR"], expected.to_string_lossy());
        let args: Vec<String> = cmd.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let settings: serde_json::Value = serde_json::from_str(after(&args, "--settings").unwrap()).unwrap();
        let write: Vec<&str> = settings["sandbox"]["filesystem"]["allowWrite"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
        assert!(write.contains(&expected.to_string_lossy().as_ref()), "{write:?}");
        let sockets: Vec<&str> = settings["sandbox"]["network"]["allowUnixSockets"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
        assert!(sockets.contains(&expected.to_string_lossy().as_ref()), "a quest may bind and reach unix sockets inside its own temp dir: {sockets:?}");
        let _ = std::fs::remove_dir_all(&expected);
        let elsewhere = build(Path::new("/tmp")).expect("command builds");
        let inherited = elsewhere.as_std().get_envs().find(|(name, _)| *name == "CLAUDE_CODE_TMPDIR").and_then(|(_, value)| value?.to_str());
        assert!(!inherited.is_some_and(|dir| dir.contains(crate::cli::QUEST_TEMP_PREFIX)));
    }

    #[test]
    fn folder_sessions_get_their_own_writable_temp_dir_in_claude() {
        let session = uuid::Uuid::new_v4();
        let expected = crate::cli::private_temp_root().join(crate::cli::session_temp_name(session));
        let launch = Launch::assume("claude");
        let build = || {
            let opts = options(false, Vec::new(), false, Approval::Auto, "sonnet");
            crate::cli::SESSION_TEMP.sync_scope(session, || command(&opts, None, &launch)).expect("command builds")
        };
        let env_of = |cmd: &tokio::process::Command| -> std::collections::BTreeMap<String, String> {
            cmd.as_std().get_envs().filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned()))).collect()
        };
        let cmd = build();
        let env = env_of(&cmd);
        for name in ["TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"] {
            assert_eq!(env[name], expected.to_string_lossy(), "{name}");
        }
        let args: Vec<String> = cmd.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let settings: serde_json::Value = serde_json::from_str(after(&args, "--settings").unwrap()).unwrap();
        let write: Vec<&str> = settings["sandbox"]["filesystem"]["allowWrite"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
        assert!(write.contains(&expected.to_string_lossy().as_ref()), "{write:?}");
        let _ = std::fs::remove_dir_all(&expected);
    }

    fn claude_env(provider_keys: &[(&str, &str)], environment: &[(&str, &str)], plan_signed_in: bool) -> std::collections::BTreeMap<String, String> {
        let mut opts = options(false, Vec::new(), false, Approval::Full, "sonnet");
        opts.provider_keys = provider_keys.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
        opts.environment = std::iter::once(("ANTHROPIC_BASE_URL", "https://api.anthropic.com"))
            .chain(environment.iter().copied())
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let launch = Launch { plan_signed_in, ..Launch::assume("claude") };
        let mut cmd = super::run_command(&opts, &launch).expect("command builds");
        cmd.env("CLAUDE_CONFIG_DIR", "/nonexistent/loka-test-claude-config");
        let source = super::user_settings(&opts, &cmd);
        super::detect::SignIn::settle(&mut cmd, source, &launch);
        cmd.as_std().get_envs().filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned()))).collect()
    }

    #[test]
    fn the_claude_plan_sign_in_wins_over_an_api_key_from_keys() {
        const KEY: (&str, &str) = ("ANTHROPIC_API_KEY", "sk-ant-api03-from-keys");
        const TOKEN: (&str, &str) = ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-plan");
        const GATEWAY: (&str, &str) = ("ANTHROPIC_AUTH_TOKEN", "gateway-token");

        let signed_in_only = claude_env(&[], &[], true);
        assert!(!signed_in_only.contains_key("ANTHROPIC_API_KEY"));
        let token_only = claude_env(&[TOKEN], &[], false);
        assert!(token_only.contains_key("CLAUDE_CODE_OAUTH_TOKEN"), "a plan token is passed on");

        let key_only = claude_env(&[KEY], &[], false);
        assert!(key_only.contains_key("ANTHROPIC_API_KEY"), "with no plan sign-in the API key is the fallback");

        let key_and_login = claude_env(&[KEY], &[GATEWAY], true);
        assert!(!key_and_login.contains_key("ANTHROPIC_API_KEY"), "a stored plan login wins over the key");
        assert!(!key_and_login.contains_key("ANTHROPIC_AUTH_TOKEN"), "and over a bearer token");
        let key_and_token = claude_env(&[KEY, TOKEN], &[], false);
        assert!(!key_and_token.contains_key("ANTHROPIC_API_KEY"), "a plan token wins over the key");
        assert!(key_and_token.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        let granted_key_and_token = claude_env(&[TOKEN], &[KEY], false);
        assert!(!granted_key_and_token.contains_key("ANTHROPIC_API_KEY"), "a key granted to the agent loses to the plan too");

        let neither = claude_env(&[], &[], false);
        assert!(!neither.contains_key("ANTHROPIC_API_KEY") && !neither.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));

        let gateway = claude_env(&[TOKEN], &[GATEWAY, ("ANTHROPIC_BASE_URL", "https://llm-gateway.example.com")], true);
        assert!(gateway.contains_key("ANTHROPIC_AUTH_TOKEN"), "a gateway keeps its token, so the plan login is never sent to it");
    }

    #[test]
    fn a_shell_api_key_in_the_claude_env_gives_way_to_the_plan_sign_in() {
        let shell_env = |plan_signed_in: bool, token: bool| {
            let mut cmd = tokio::process::Command::new("claude");
            cmd.env("PATH", "/usr/bin").env("ANTHROPIC_API_KEY", "sk-ant-api03-from-the-shell");
            if token {
                cmd.env("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-plan");
            }
            super::detect::SignIn::settle(&mut cmd, super::detect::UserSettings::ReadByCli, &Launch { plan_signed_in, ..Launch::assume("claude") });
            cmd.as_std().get_envs().filter(|(_, value)| value.is_some()).map(|(name, _)| name.to_string_lossy().into_owned()).collect::<Vec<_>>()
        };
        assert!(shell_env(false, false).contains(&"ANTHROPIC_API_KEY".to_string()), "key only: the shell key is used");
        assert!(!shell_env(true, false).contains(&"ANTHROPIC_API_KEY".to_string()), "stored plan login and a shell key: the plan wins");
        let both = shell_env(false, true);
        assert!(!both.contains(&"ANTHROPIC_API_KEY".to_string()) && both.contains(&"CLAUDE_CODE_OAUTH_TOKEN".to_string()), "plan token and a shell key: the plan wins");
        assert!(shell_env(true, false).contains(&"PATH".to_string()), "nothing else in the shell env changes");
    }

    struct SettledRun {
        env: std::collections::BTreeMap<String, String>,
        settings: serde_json::Value,
    }

    fn settled_run(lean: bool, home: &Path, process: &[(&str, &str)], plan_signed_in: bool) -> SettledRun {
        let mut opts = options(lean, Vec::new(), false, Approval::Full, "sonnet");
        opts.environment = std::iter::once(("ANTHROPIC_BASE_URL", "https://api.anthropic.com"))
            .chain(process.iter().copied())
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let launch = Launch { plan_signed_in, ..Launch::assume("claude") };
        let mut cmd = super::run_command(&opts, &launch).expect("command builds");
        cmd.env_remove("CLAUDE_CONFIG_DIR").env("HOME", home);
        let source = super::user_settings(&opts, &cmd);
        let sign_in = super::detect::SignIn::settle(&mut cmd, source, &launch);
        let settings = sign_in.settings(Some(serde_json::json!({}))).unwrap();
        let env = cmd.as_std().get_envs().filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned()))).collect();
        SettledRun { env, settings }
    }

    fn job_home(settings: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("claude-job-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), settings).unwrap();
        home
    }

    #[test]
    fn an_api_key_in_settings_json_never_beats_the_plan_sign_in() {
        let home = job_home(r#"{"apiKeyHelper": "/opt/helper", "env": {"ANTHROPIC_API_KEY": "sk-ant-api03-from-settings", "ANTHROPIC_SMALL_FAST_MODEL": "haiku"}}"#);

        let work = settled_run(false, &home, &[], true);
        assert!(!work.env.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(work.settings["env"]["ANTHROPIC_API_KEY"], serde_json::json!(""), "Claude Code applies settings.json env over the process env, so the key is blanked through --settings");
        assert!(work.settings.get("apiKeyHelper").is_none(), "the CLI reads its own settings.json on work runs");

        let main_turn = settled_run(true, &home, &[], true);
        assert!(!main_turn.env.contains_key("ANTHROPIC_API_KEY"), "lean turns never copy the settings key back after the plan won");
        assert_eq!(main_turn.env.get("ANTHROPIC_SMALL_FAST_MODEL").map(String::as_str), Some("haiku"), "the rest of the settings env still applies");
        assert_eq!(main_turn.settings["apiKeyHelper"], serde_json::json!("/opt/helper"));
        assert!(main_turn.settings.get("env").is_none());

        let no_plan = settled_run(false, &home, &[], false);
        assert!(no_plan.settings.get("env").is_none(), "with no plan sign-in the settings key stays the fallback");
        let no_plan_turn = settled_run(true, &home, &[], false);
        assert_eq!(no_plan_turn.env.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-ant-api03-from-settings"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_gateway_only_in_the_job_users_settings_keeps_its_token() {
        let home = job_home(r#"{"env": {"ANTHROPIC_BASE_URL": "https://llm-gateway.example.com"}}"#);
        let token = [("ANTHROPIC_AUTH_TOKEN", "gateway-token")];
        for lean in [true, false] {
            let run = settled_run(lean, &home, &token, true);
            assert_eq!(run.env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str), Some("gateway-token"), "lean {lean}: the gateway keeps its token");
            assert!(run.settings.get("env").is_none(), "lean {lean}: nothing is blanked for a gateway");
        }
        assert_eq!(
            settled_run(true, &home, &token, true).env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://llm-gateway.example.com"),
            "the gateway Loka applies on lean turns comes from the same settings.json the decision read"
        );
        std::fs::write(home.join(".claude/settings.json"), r#"{"env": {"ANTHROPIC_BASE_URL": "https://api.anthropic.com"}}"#).unwrap();
        assert!(!settled_run(true, &home, &token, true).env.contains_key("ANTHROPIC_AUTH_TOKEN"), "the Anthropic API is no gateway, so the plan wins");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn refused_auto_mode_fails_closed_instead_of_taking_full_access() {
        let work = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        let normal = Launch::assume("claude");
        assert_eq!(after(&args(&work, &normal), "--permission-mode"), Some("auto"));
        assert_eq!(expected_mode(&work, &normal), Some("auto"));

        let refused = Launch { auto_refused: true, ..Launch::assume("claude") };
        let error = command(&work, None, &refused).expect_err("a refused auto mode must not build a command").to_string();
        assert!(error.contains("never switches to full access"), "{error}");
        assert!(!error.contains('\u{2014}'));
        assert_eq!(loka_shared::cli_errors::classify(&error).map(|f| f.kind), Some(loka_shared::cli_errors::FailureKind::Permissions));
        assert_eq!(expected_mode(&work, &refused), None);

        let help = "  -p, --print x\n  --tools <t> x\n  --permission-mode <mode> x (choices: \"acceptEdits\", \"bypassPermissions\", \"default\")";
        let no_auto = Launch { caps: Arc::new(Caps::parse(Some("2.1.0".into()), help)), ..Launch::assume("claude") };
        assert!(command(&work, None, &no_auto).is_err());

        let haiku = options(false, Vec::new(), false, Approval::Auto, "haiku");
        assert!(command(&haiku, None, &normal).is_err());
        assert_eq!(expected_mode(&haiku, &normal), None);

        let full = options(false, Vec::new(), false, Approval::Full, "haiku");
        assert!(args(&full, &refused).iter().any(|arg| arg == "--dangerously-skip-permissions"));
        assert_eq!(expected_mode(&full, &normal), Some("bypassPermissions"));

        let read_only = options(false, Vec::new(), false, Approval::ReadOnly, "sonnet");
        assert!(!args(&read_only, &refused).iter().any(|arg| arg == "--dangerously-skip-permissions"));
        assert_eq!(expected_mode(&read_only, &normal), None);
        assert_eq!(expected_mode(&options(true, Vec::new(), false, Approval::Auto, "sonnet"), &normal), None);
    }

    #[test]
    fn older_clis_get_only_the_flags_they_know() {
        let help = "  -p, --print x\n  --output-format <f> x\n  --system-prompt <p> x\n  --tools <t> x\n  --setting-sources <s> x\n  --strict-mcp-config x\n  --fork-session x\n  --permission-mode <mode> x (choices: \"acceptEdits\", \"bypassPermissions\", \"default\", \"plan\")";
        let old = Launch { caps: Arc::new(Caps::parse(Some("2.0.1".into()), help)), ..Launch::assume("claude") };
        let work = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        assert!(old.caps.outdated().is_none());
        let refused = command(&work, None, &old).expect_err("an old CLI without auto mode never falls back to full access").to_string();
        assert!(refused.contains("never switches to full access"), "{refused}");
        let built = args(&options(false, Vec::new(), false, Approval::Full, "sonnet"), &old);
        assert!(!built.iter().any(|arg| arg == "--effort"));
        assert!(!built.iter().any(|arg| arg == "auto"));
        assert!(built.iter().any(|arg| arg == "--dangerously-skip-permissions"));
        let reads = args(&options(true, vec![PathBuf::from("/tmp/s.md")], false, Approval::Auto, "sonnet"), &old);
        assert_eq!(after(&reads, "--permission-mode"), Some("default"));
        let current = args(&work, &Launch::assume("claude"));
        assert_eq!(after(&current, "--effort"), Some("low"));
    }

    #[test]
    fn a_refused_permission_mode_stops_the_run_with_a_plain_reason() {
        let opts = options(false, Vec::new(), false, Approval::Auto, "refusal-test-model");
        let init = serde_json::json!({"type": "system", "subtype": "init", "session_id": "s", "permissionMode": "default"});
        let mut reader = TurnReader::new(std::time::Instant::now()).expecting(Some("auto"));
        reader.feed(&init, None);
        let error = refusal(&reader, &opts).unwrap().to_string();
        assert!(error.contains("would not start in auto mode") && error.contains("default mode"));
        assert!(error.contains("never switches to full access") && !error.contains("next try"));
        assert!(!error.contains('\u{2014}'));
        assert_eq!(loka_shared::cli_errors::classify(&error).map(|f| f.kind), Some(loka_shared::cli_errors::FailureKind::Permissions));

        let mut full = TurnReader::new(std::time::Instant::now()).expecting(Some("bypassPermissions"));
        full.feed(&init, None);
        assert!(refusal(&full, &opts).unwrap().to_string().contains("disableBypassPermissionsMode"));

        let mut late = TurnReader::new(std::time::Instant::now()).expecting(Some("auto"));
        late.feed(&serde_json::json!({"type": "system", "subtype": "init", "session_id": "s", "permissionMode": "auto"}), None);
        assert!(refusal(&late, &opts).is_none());
        late.feed(&serde_json::json!({"type": "system", "subtype": "status", "status": null, "permissionMode": "default"}), None);
        assert!(refusal(&late, &opts).unwrap().to_string().contains("would not start in auto mode"));

        let mut planned = TurnReader::new(std::time::Instant::now()).expecting(Some("auto"));
        planned.feed(&serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}}), None);
        planned.feed(&serde_json::json!({"type": "system", "subtype": "status", "permissionMode": "default"}), None);
        assert!(refusal(&planned, &opts).is_none());

        let mut fine = TurnReader::new(std::time::Instant::now()).expecting(Some("auto"));
        fine.feed(&serde_json::json!({"type": "system", "subtype": "init", "session_id": "s", "permissionMode": "auto"}), None);
        assert!(refusal(&fine, &opts).is_none());
        let mut lean = TurnReader::new(std::time::Instant::now());
        lean.feed(&init, None);
        assert!(refusal(&lean, &opts).is_none());
    }

    #[test]
    fn lean_turns_keep_the_users_claude_auth_settings_only() {
        let text = r#"{"apiKeyHelper": "~/bin/key.sh", "model": "opus", "hooks": {"Stop": []}, "permissions": {"allow": ["Bash"]},
            "env": {"ANTHROPIC_BASE_URL": "https://gw", "CLAUDE_CODE_USE_BEDROCK": "1", "AWS_PROFILE": "dev", "SOMETHING_ELSE": "x", "ANTHROPIC_MODEL": 3}}"#;
        let picked = auth_settings_from(text).unwrap();
        assert_eq!(picked.get("apiKeyHelper"), Some(&serde_json::json!("~/bin/key.sh")));
        assert!(picked.get("hooks").is_none() && picked.get("permissions").is_none() && picked.get("model").is_none());
        assert_eq!(
            picked["env"],
            serde_json::json!({"ANTHROPIC_BASE_URL": "https://gw", "CLAUDE_CODE_USE_BEDROCK": "1", "AWS_PROFILE": "dev"})
        );
        assert!(auth_settings_from(r#"{"model": "opus"}"#).is_none());
        assert!(auth_settings_from("not json").is_none());
    }

    #[test]
    fn api_errors_in_a_success_result_are_errors() {
        let limited = serde_json::json!({"type": "result", "subtype": "success", "is_error": true, "result": "Claude AI usage limit reached|1761930000"});
        assert!(is_error_result(&limited));
        let ok = serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "result": "done"});
        assert!(!is_error_result(&ok));
        let max_turns = serde_json::json!({"type": "result", "subtype": "error_max_turns", "is_error": true});
        assert!(!is_error_result(&max_turns));
    }

    #[test]
    fn effort_uses_existing_defaults_without_configuration() {
        assert_eq!(select_effort(None, None, true).unwrap(), "medium");
        assert_eq!(select_effort(None, None, false).unwrap(), "high");
    }

    #[test]
    fn effort_uses_configured_default_for_work() {
        assert_eq!(
            select_effort(None, Some("medium"), false).unwrap(),
            "medium"
        );
    }

    #[test]
    fn explicit_effort_overrides_configured_default() {
        assert_eq!(
            select_effort(Some("max"), Some("medium"), false).unwrap(),
            "max"
        );
    }

    #[test]
    fn effort_rejects_invalid_configured_default() {
        let error = select_effort(None, Some("minimal"), false).unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid WORLD_CLAUDE_EFFORT `minimal`"));
    }

    #[tokio::test]
    async fn writes_mcp_temp_file_with_expected_contents() {
        let mcp_json = r#"{"mcpServers":{"example":{"command":"echo","env":{"TOKEN":"test"}}}}"#;
        let path = write_mcp_config_temp_file(mcp_json)
            .await
            .expect("temp mcp config should be written");

        let content = tokio::fs::read_to_string(&path)
            .await
            .expect("temp mcp config should be readable");
        assert_eq!(content, mcp_json);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&path)
                .await
                .expect("temp mcp config metadata should be readable")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        tokio::fs::remove_file(path)
            .await
            .expect("temp mcp config should be removable");
    }
    fn system(subtype: &str, task: &str) -> serde_json::Value {
        serde_json::json!({"type": "system", "subtype": subtype, "task_id": task, "description": "cargo test -p loka-shared"})
    }

    #[test]
    fn a_turn_without_background_tasks_finishes_at_its_result() {
        let mut background = Background::default();
        let now = Instant::now();
        assert!(!background.keep_open(now, Some(Duration::from_secs(1200))));
        assert!(background.note(now + Duration::from_secs(600), Some(Duration::from_secs(1200))).is_none());
        background.track(&system("task_started", "t1"));
        background.track(&system("task_notification", "t1"));
        assert!(!background.keep_open(now, Some(Duration::from_secs(1200))), "tasks that ended inside the turn need no wait");
    }

    #[test]
    fn background_tasks_running_at_the_result_keep_the_run_open_until_they_finish() {
        let cap = Some(Duration::from_secs(1200));
        let mut background = Background::default();
        let start = Instant::now();
        background.track(&system("task_started", "t1"));
        assert!(background.keep_open(start, cap));
        assert!(background.note(start + Duration::from_secs(5), cap).is_none(), "the CLI may pick up its own notification first");
        let note = background.note(start + BACKGROUND_GRACE, cap).expect("asked to wait in the foreground");
        assert!(note.contains("cargo test -p loka-shared") && note.contains("in the foreground") && note.contains("verdict"));
        assert!(background.note(start + BACKGROUND_GRACE * 2, cap).is_none(), "one note per ended turn");
        assert!(background.keep_open(start + Duration::from_secs(60), cap), "the agent ended again while the task still runs");
        background.track(&system("task_notification", "t1"));
        assert_eq!(background.note(start + Duration::from_secs(61), cap).as_deref(), Some(BACKGROUND_DONE_NOTE));
        assert!(!background.keep_open(start + Duration::from_secs(90), cap), "the answer after the note ends the run");
    }

    #[test]
    fn a_turn_the_cli_resumes_by_itself_gets_no_note() {
        let cap = Some(Duration::from_secs(1200));
        let mut background = Background::default();
        let start = Instant::now();
        background.track(&system("task_started", "t1"));
        assert!(background.keep_open(start, cap));
        background.track(&system("task_notification", "t1"));
        background.track(&serde_json::json!({"type": "assistant", "message": {"content": []}}));
        assert!(background.note(start + Duration::from_secs(60), cap).is_none());
        assert!(!background.keep_open(start + Duration::from_secs(61), cap));
    }

    #[test]
    fn waiting_stops_at_the_cap_and_the_next_answer_ends_the_run() {
        let cap = Some(Duration::from_secs(1200));
        let mut background = Background::default();
        let start = Instant::now();
        background.track(&system("task_started", "t1"));
        for round in 0..BACKGROUND_ASKS {
            let ended = start + Duration::from_secs(u64::from(round) * 60);
            assert!(background.keep_open(ended, cap));
            assert!(background.note(ended + BACKGROUND_GRACE, cap).is_some_and(|note| note.starts_with("[background tasks still running]")));
        }
        let ended = start + Duration::from_secs(600);
        assert!(background.keep_open(ended, cap));
        assert!(background.note(ended + BACKGROUND_GRACE, cap).is_none(), "stubborn turns are not asked forever");
        let stop = background.note(start + Duration::from_secs(1200), cap).expect("stop note at the cap");
        assert!(stop.starts_with("[stop waiting]") && stop.contains("20 minutes") && stop.contains("cargo test"));
        assert!(!background.keep_open(start + Duration::from_secs(1300), cap), "after the stop note the run ends");
        let mut off = Background::default();
        off.track(&system("task_started", "t2"));
        assert!(!off.keep_open(start, None), "WORLD_BACKGROUND_WAIT_SECS=0 turns waiting off");
    }

    #[test]
    fn background_notes_count_toward_max_turns_so_a_spent_run_is_never_kept_open() {
        let cap = Some(Duration::from_secs(1200));
        let assistant = |id: &str| serde_json::json!({"type": "assistant", "message": {"id": id, "content": []}});
        let mut background = Background::within(3);
        let start = Instant::now();
        for id in ["m1", "m1", "m2"] {
            background.track(&assistant(id));
        }
        background.track(&system("task_started", "t1"));
        assert!(background.keep_open(start, cap), "two of three turns used, the run waits");
        assert!(background.note(start + BACKGROUND_GRACE, cap).is_some());
        background.track(&assistant("m3"));
        assert!(!background.keep_open(start + Duration::from_secs(60), cap), "the answer to the note used the last turn, so its result ends the run");
        assert!(background.note(start + Duration::from_secs(60) + BACKGROUND_GRACE, cap).is_none(), "a run that used its turns gets no more notes");
        let mut nested = Background::within(3);
        for (id, parent) in [("m1", None), ("s1", Some("toolu_1")), ("m1", None), ("s2", Some("toolu_1")), ("s3", Some("toolu_1")), ("m2", None), ("s4", Some("toolu_1"))] {
            nested.track(&serde_json::json!({"type": "assistant", "parent_tool_use_id": parent, "message": {"id": id, "content": []}}));
        }
        nested.track(&system("task_started", "t3"));
        assert!(nested.keep_open(start, cap), "subagent messages and a repeated id are not turns of the run");
        let mut unlimited = Background::within(0);
        for id in ["a", "b", "c", "d"] {
            unlimited.track(&assistant(id));
        }
        unlimited.track(&system("task_started", "t2"));
        assert!(unlimited.keep_open(start, cap), "no turn limit leaves only the wait cap");
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loka-fake-claude-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_claude(dir: &Path, script: &str) -> tokio::process::Command {
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut cmd = tokio::process::Command::new(path);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        cmd
    }

    const FAKE_START: &str = r#"read first
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1","permissionMode":"auto"}'
printf '%s\n' '{"type":"user","isReplay":true,"message":{"role":"user","content":"go"}}'"#;

    #[tokio::test]
    async fn a_review_that_ends_while_its_tests_run_still_gives_its_verdict() {
        let dir = scratch();
        let script = format!(
            r#"{FAKE_START}
printf '%s\n' '{{"type":"system","subtype":"task_started","task_id":"b1","description":"cargo test"}}'
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"Waiting on the test run.","session_id":"s1","num_turns":3}}'
sleep 1
printf '%s\n' '{{"type":"system","subtype":"task_notification","task_id":"b1","status":"completed"}}'
read note
case "$note" in *"background tasks finished"*) ;; *) echo '{{"type":"result","subtype":"success","is_error":false,"result":"wrong note"}}'; exit 0;; esac
printf '%s\n' '{{"type":"user","isReplay":true,"message":{{"role":"user","content":"note"}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"Tests pass.\n<!-- verdict {{\"verdict\":\"approve\"}} verdict -->","session_id":"s1"}}'
read rest"#
        );
        let opts = options(false, Vec::new(), false, Approval::Auto, "opus");
        let (_idle, steer) = tokio::sync::mpsc::unbounded_channel();
        let run = super::run_steerable(&opts, fake_claude(&dir, &script), steer, Instant::now(), None);
        let (result, _) = tokio::time::timeout(Duration::from_secs(60), run).await.expect("run finished").unwrap();
        assert!(result.output.starts_with("Waiting on the test run."), "{}", result.output);
        assert!(result.output.ends_with("<!-- verdict {\"verdict\":\"approve\"} verdict -->"), "{}", result.output);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_normal_run_and_a_cli_that_exits_mid_wait_both_return_at_once() {
        let dir = scratch();
        let normal = format!(
            r#"{FAKE_START}
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"<!-- report {{\"status\":\"done\"}} report -->","session_id":"s1"}}'
read rest"#
        );
        let opts = options(false, Vec::new(), false, Approval::Auto, "opus");
        let (_idle, steer) = tokio::sync::mpsc::unbounded_channel();
        let run = super::run_steerable(&opts, fake_claude(&dir, &normal), steer, Instant::now(), None);
        let (result, _) = tokio::time::timeout(Duration::from_secs(10), run).await.expect("no wait without background tasks").unwrap();
        assert!(result.output.contains("<!-- report"), "{}", result.output);

        let exits = format!(
            r#"{FAKE_START}
printf '%s\n' '{{"type":"system","subtype":"task_started","task_id":"b1","description":"sleep"}}'
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"Waiting.","session_id":"s1"}}'
exit 0"#
        );
        let (_idle, steer) = tokio::sync::mpsc::unbounded_channel();
        let run = super::run_steerable(&opts, fake_claude(&dir, &exits), steer, Instant::now(), None);
        let (result, _) = tokio::time::timeout(Duration::from_secs(10), run).await.expect("a closed CLI ends the wait").unwrap();
        assert_eq!(result.output, "Waiting.");
        let _ = std::fs::remove_dir_all(dir);
    }
}
