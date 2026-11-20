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
    summarize_tool_input, summarize_tool_output, truncate_for_log, CliOptions, CliResult, StreamEvent,
    TOOL_OUTPUT_MAX_CHARS,
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
            let next = if sandbox::mode_for(false) == sandbox::Mode::Off {
                "full access with no sandbox"
            } else {
                "full access inside the sandbox"
            };
            anyhow::anyhow!(
                "Claude Code would not start in auto mode (it fell back to {actual} mode, where it cannot edit files or run commands). Auto mode is not available for this Claude account, model or device settings, so the next try uses {next}."
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

fn user_settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|home| PathBuf::from(home).join(".claude")))?;
    Some(dir.join("settings.json"))
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

fn user_settings_trusted() -> bool {
    crate::cli::job_user().is_none() || user_settings_path().is_none_or(|path| !path.exists() || trusted_settings_file(&path))
}

pub(crate) fn user_auth_settings() -> Option<serde_json::Map<String, serde_json::Value>> {
    let path = user_settings_path()?;
    if !trusted_settings_file(&path) {
        return None;
    }
    auth_settings_from(&std::fs::read_to_string(path).ok()?)
}

fn without_attribution(settings: Option<serde_json::Value>) -> serde_json::Value {
    let mut settings = settings.unwrap_or_else(|| serde_json::json!({}));
    settings["includeCoAuthoredBy"] = serde_json::json!(false);
    settings["attribution"] = serde_json::json!({ "commit": "", "pr": "" });
    settings
}

fn with_auth(cmd: &mut Command, settings: Option<serde_json::Value>) -> Option<serde_json::Value> {
    let Some(mut auth) = user_auth_settings() else {
        return settings;
    };
    if let Some(serde_json::Value::Object(env)) = auth.remove("env") {
        for (name, value) in env {
            if let Some(value) = value.as_str() {
                cmd.env(name, value);
            }
        }
    }
    if auth.is_empty() {
        return settings;
    }
    let mut settings = settings.unwrap_or_else(|| serde_json::json!({}));
    for (key, value) in auth {
        settings[key] = value;
    }
    Some(settings)
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
    ReadOnly,
    Bypass,
}

fn permission(opts: &CliOptions<'_>, launch: &Launch) -> Permission {
    match (opts.lean, opts.approval) {
        (true, _) if !opts.readable_files.is_empty() => Permission::LeanReads,
        (false, crate::cli::Approval::Auto) if detect::wants_auto(opts) && !launch.auto_refused && launch.caps.mode("auto") => Permission::Auto,
        (false, crate::cli::Approval::ReadOnly) => Permission::ReadOnly,
        _ => Permission::Bypass,
    }
}

pub(crate) fn expected_mode(opts: &CliOptions<'_>, launch: &Launch) -> Option<&'static str> {
    if opts.lean {
        return None;
    }
    match permission(opts, launch) {
        Permission::Auto => Some("auto"),
        Permission::Bypass => Some("bypassPermissions"),
        Permission::LeanReads | Permission::ReadOnly => None,
    }
}

pub(crate) fn command(opts: &CliOptions<'_>, mcp_config_path: Option<&Path>, launch: &Launch) -> Result<Command> {
    let mut cmd = job_command(&launch.program);
    cmd.current_dir(opts.working_dir);
    cmd.kill_on_drop(true);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    launch.configure(&mut cmd);

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
    }
    cmd.arg("--verbose");
    cmd.arg("--output-format").arg("stream-json");
    cmd.arg("--max-turns").arg(opts.max_turns.to_string());
    cmd.arg("--model").arg(opts.model);
    if let Some(level) = launch.caps.effort(effort) {
        cmd.arg("--effort").arg(level);
    }
    let sandbox_mode = if opts.lean { sandbox::Mode::Off } else { sandbox::mode_for(false) };
    let writable = if sandbox_mode == sandbox::Mode::Off { Vec::new() } else { sandbox::git_write_roots(opts.working_dir) };
    let settings = (!lean_reads).then(|| sandbox::settings(sandbox_mode, &opts.environment, mcp_config_path, &writable, opts.fast)).flatten();
    let settings = if opts.lean { settings } else { Some(without_attribution(settings)) };
    let skip_user_settings = !opts.lean && !user_settings_trusted();
    let settings = if (opts.lean && !lean_reads) || skip_user_settings { with_auth(&mut cmd, settings) } else { settings };
    if let Some(mut settings) = settings {
        sandbox::open_for_owner(&mut settings);
        if mode == Permission::Auto {
            settings["autoMode"] = serde_json::json!({ "allow": owner_allow(&opts.environment) });
        }
        if mode == Permission::Bypass && opts.approval == crate::cli::Approval::Auto && settings.get("sandbox").is_some() {
            settings["sandbox"]["failIfUnavailable"] = serde_json::json!(true);
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
        cmd.arg("--tools").arg("Read");
        cmd.arg("--setting-sources").arg("");
        let read_settings = Some(lean_read_settings(&opts.readable_files, opts.working_dir, opts.fast));
        let settings = with_auth(&mut cmd, read_settings).unwrap_or_default();
        cmd.arg("--settings").arg(settings.to_string());
        let servers = mcp_server_rules(opts.mcp_config_json.as_deref());
        if !servers.is_empty() {
            cmd.arg("--allowedTools").arg(servers);
        }
    } else if opts.lean {
        cmd.arg("--tools").arg("");
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
                    let output = truncate_for_log(output.trim(), TOOL_OUTPUT_MAX_CHARS);
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

pub(crate) struct SteerState {
    sent: usize,
    replayed: usize,
    interrupted: bool,
    tools: crate::cli::OpenTools,
}

impl SteerState {
    pub(crate) fn new() -> Self {
        Self { sent: 1, replayed: 0, interrupted: false, tools: crate::cli::OpenTools::new() }
    }

    pub(crate) fn oldest_tool(&self) -> Option<std::time::Duration> {
        self.tools.oldest()
    }

    pub(crate) fn stuck_longer_than(&self, limit: std::time::Duration) -> bool {
        self.tools.stuck_longer_than(limit)
    }

    pub(crate) fn track(&mut self, json: &serde_json::Value) {
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
    let stderr = crate::cli::stderr_tail(child.stderr.take());
    let stdout = child.stdout.take().expect("stdout piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut reader = TurnReader::new(started).expecting(expect);
    reader.timing.spawned = true;
    reader.timing.spawn_ms = started.elapsed().as_millis() as u64;

    let interrupt_after = crate::cli::steer_interrupt_after();
    let hang_after = crate::cli::tool_hang_after();
    let mut state = SteerState::new();
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
    use super::{auth_settings_from, command, expected_mode, is_error_result, refusal, select_effort, user_auth_settings, write_mcp_config_temp_file, TurnReader};
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
    fn refused_auto_mode_falls_back_to_full_access_in_the_sandbox() {
        let work = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        let normal = Launch::assume("claude");
        assert_eq!(after(&args(&work, &normal), "--permission-mode"), Some("auto"));
        assert_eq!(expected_mode(&work, &normal), Some("auto"));

        let refused = Launch { auto_refused: true, ..Launch::assume("claude") };
        let fallback = args(&work, &refused);
        assert!(fallback.iter().any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(!fallback.iter().any(|arg| arg == "--permission-mode"));
        assert_eq!(expected_mode(&work, &refused), Some("bypassPermissions"));

        let haiku = options(false, Vec::new(), false, Approval::Auto, "haiku");
        assert_eq!(expected_mode(&haiku, &normal), Some("bypassPermissions"));
        let read_only = options(false, Vec::new(), false, Approval::ReadOnly, "sonnet");
        assert_eq!(expected_mode(&read_only, &normal), None);
        assert_eq!(expected_mode(&options(true, Vec::new(), false, Approval::Auto, "sonnet"), &normal), None);
    }

    #[test]
    fn older_clis_get_only_the_flags_they_know() {
        let help = "  -p, --print x\n  --output-format <f> x\n  --system-prompt <p> x\n  --tools <t> x\n  --setting-sources <s> x\n  --strict-mcp-config x\n  --fork-session x\n  --permission-mode <mode> x (choices: \"acceptEdits\", \"bypassPermissions\", \"default\", \"plan\")";
        let old = Launch { caps: Arc::new(Caps::parse(Some("2.0.1".into()), help)), ..Launch::assume("claude") };
        let work = options(false, Vec::new(), false, Approval::Auto, "sonnet");
        assert!(old.caps.outdated().is_none());
        let built = args(&work, &old);
        assert!(!built.iter().any(|arg| arg == "--effort"));
        assert!(!built.iter().any(|arg| arg == "auto"));
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
}
