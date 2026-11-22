use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, info};

use crate::cli::{collect_stderr, configure_child_environment, give_to_job_user, job_command, CliOptions, CliResult, StreamEvent};

const AGENT_PREAMBLE: &str = "\
You are Loka, an autonomous coding agent. You work independently without \
human interaction during execution. If you are uncertain, make the best judgment \
call and document your reasoning in a code comment or commit message.\n\n\
Follow existing code patterns and conventions in each repository. \
Read AGENTS.md or CLAUDE.md if they exist for project-specific instructions.\n\n\
Git commit rules:\n\
- Use Conventional Commits: type(scope): description\n\
- Types: feat, fix, refactor, test, chore, perf, ci\n\
- NEVER add Co-Authored-By trailers to commits\n\
- NEVER add Signed-off-by trailers to commits";

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    let config = Some(write_run_config(opts.mcp_config_json.as_deref().unwrap_or("{}")).await?);
    let result = run_inner(&opts, config.as_ref().map(|c| c.file.as_path())).await;
    drop(config);
    result
}

struct RunConfig {
    root: PathBuf,
    file: PathBuf,
}

impl Drop for RunConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn host_servers() -> Vec<String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let Some(dir) = base.map(|base| base.join("opencode")) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for file in ["opencode.json", "opencode.jsonc", "config.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        let text: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) {
            names.extend(config.get("mcp").and_then(|mcp| mcp.as_object()).into_iter().flat_map(|mcp| mcp.keys().cloned()));
        }
    }
    names
}

fn opencode_mcp(mcp_json: &str) -> Result<serde_json::Value> {
    let mut config = equipped_mcp(mcp_json)?;
    if let Some(servers) = config["mcp"].as_object_mut() {
        for name in host_servers() {
            servers.entry(name).or_insert_with(|| serde_json::json!({ "enabled": false }));
        }
    }
    Ok(config)
}

fn equipped_mcp(mcp_json: &str) -> Result<serde_json::Value> {
    let parsed: serde_json::Value =
        serde_json::from_str(mcp_json).context("Invalid MCP configuration")?;
    let mut servers = serde_json::Map::new();
    if let Some(map) = parsed.get("mcpServers").and_then(|v| v.as_object()) {
        for (name, server) in map {
            let mut entry = serde_json::Map::new();
            if let Some(url) = server.get("url").and_then(|v| v.as_str()) {
                entry.insert("type".into(), "remote".into());
                entry.insert("url".into(), url.into());
                if let Some(headers) = server.get("headers").filter(|v| v.is_object()) {
                    entry.insert("headers".into(), headers.clone());
                }
            } else {
                let Some(command) = server.get("command").and_then(|v| v.as_str()) else {
                    continue;
                };
                let mut argv = vec![serde_json::Value::from(command)];
                if let Some(args) = server.get("args").and_then(|v| v.as_array()) {
                    argv.extend(args.iter().cloned());
                }
                entry.insert("type".into(), "local".into());
                entry.insert("command".into(), argv.into());
                if let Some(env) = server.get("env").filter(|v| v.is_object()) {
                    entry.insert("environment".into(), env.clone());
                }
            }
            entry.insert("enabled".into(), true.into());
            servers.insert(name.clone(), entry.into());
        }
    }
    Ok(serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": servers,
    }))
}

async fn write_run_config(mcp_json: &str) -> Result<RunConfig> {
    let config = serde_json::to_vec(&opencode_mcp(mcp_json)?)?;
    let root = crate::cli::temp_dir().join(format!(
        "loka-opencode-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut dir = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    dir.mode(0o700);
    dir.create(&root)
        .await
        .with_context(|| format!("Failed to create opencode config dir: {}", root.display()))?;
    let file = root.join("opencode.json");
    let run_config = RunConfig { root, file };
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut handle = options
        .open(&run_config.file)
        .await
        .with_context(|| format!("Failed to create opencode config: {}", run_config.file.display()))?;
    handle.write_all(&config).await?;
    handle.flush().await?;
    give_to_job_user(&run_config.file)?;
    give_to_job_user(&run_config.root)?;
    Ok(run_config)
}

const LEAN_AGENT: &str = "loka-lean";
const READ_ONLY_AGENT: &str = "loka-read-only";
const WRITING_TOOLS: [&str; 6] = ["bash", "edit", "write", "patch", "multiedit", "task"];

struct RunAgent {
    name: &'static str,
    root: PathBuf,
    content: String,
}

impl Drop for RunAgent {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn lean_tools(mcp_json: Option<&str>) -> serde_json::Value {
    let mut tools = serde_json::Map::new();
    tools.insert("*".into(), false.into());
    let servers = mcp_json
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| value.get("mcpServers").and_then(|v| v.as_object()).cloned())
        .unwrap_or_default();
    for name in servers.keys() {
        tools.insert(format!("{name}_*"), true.into());
    }
    tools.into()
}

fn read_only_agent() -> serde_json::Value {
    let mut tools = serde_json::Map::new();
    let mut permission = serde_json::Map::new();
    for tool in WRITING_TOOLS {
        tools.insert(tool.into(), false.into());
        permission.insert(tool.into(), "deny".into());
    }
    serde_json::json!({ "mode": "primary", "tools": tools, "permission": permission })
}

async fn run_agent(opts: &CliOptions<'_>) -> Result<Option<RunAgent>> {
    if !opts.lean && opts.approval != crate::cli::Approval::ReadOnly {
        return Ok(None);
    }
    let root = crate::cli::temp_dir().join(format!("loka-opencode-agent-{}", uuid::Uuid::new_v4().simple()));
    let mut dir = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    dir.mode(0o700);
    dir.create(&root).await.with_context(|| format!("Failed to create opencode agent dir: {}", root.display()))?;
    let (name, mut agent) = if opts.lean {
        (LEAN_AGENT, serde_json::json!({ "mode": "primary", "tools": lean_tools(opts.mcp_config_json.as_deref()) }))
    } else {
        (READ_ONLY_AGENT, read_only_agent())
    };
    let mut guard = RunAgent { name, root, content: String::new() };
    if let Some(system) = opts.system_prompt.filter(|s| opts.lean && !s.trim().is_empty()) {
        let file = guard.root.join("system.md");
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut handle = options.open(&file).await.context("Failed to write the opencode system prompt")?;
        handle.write_all(system.as_bytes()).await?;
        handle.flush().await?;
        give_to_job_user(&file)?;
        agent["prompt"] = format!("{{file:{}}}", file.display()).into();
    }
    give_to_job_user(&guard.root)?;
    guard.content = serde_json::json!({ "agent": { name: agent } }).to_string();
    Ok(Some(guard))
}

fn full_prompt(opts: &CliOptions<'_>) -> String {
    if opts.lean || (opts.session_id.is_some() && opts.resume) {
        return opts.prompt.to_string();
    }
    let mut parts = vec![AGENT_PREAMBLE.to_string()];
    if let Some(sys) = opts.system_prompt {
        parts.push(sys.to_string());
    }
    parts.push(opts.prompt.to_string());
    parts.join("\n\n")
}

fn xdg_dir(name: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|home| PathBuf::from(home).join(fallback)))
        .map(|dir| dir.join("opencode"))
}

fn sandbox_spec(opts: &CliOptions<'_>, config: Option<&Path>, agent: Option<&RunAgent>) -> crate::sandbox_wrap::Spec {
    let state: Vec<PathBuf> = [("XDG_DATA_HOME", ".local/share"), ("XDG_CACHE_HOME", ".cache"), ("XDG_STATE_HOME", ".local/state")]
        .into_iter()
        .filter_map(|(name, fallback)| xdg_dir(name, fallback))
        .collect();
    for dir in &state {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut readable: Vec<PathBuf> = config.and_then(Path::parent).map(Path::to_path_buf).into_iter().collect();
    readable.extend(agent.map(|agent| agent.root.clone()));
    readable.extend(opts.image_paths.iter().cloned());
    crate::sandbox_wrap::Spec {
        working_dir: opts.working_dir.to_path_buf(),
        read_only: opts.approval == crate::cli::Approval::ReadOnly,
        readable,
        state,
    }
}

fn provider_base(id: &str, base: &str) -> String {
    match id {
        crate::key_proxy::ANTHROPIC => format!("{base}/v1"),
        "google" => format!("{base}/v1beta"),
        _ => base.to_string(),
    }
}

fn base_command(opts: &CliOptions<'_>, config: Option<&Path>, agent: Option<&RunAgent>) -> Result<tokio::process::Command> {
    let mut cmd = job_command("opencode");
    cmd.current_dir(opts.working_dir);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    opts.sign_commits(&mut cmd);
    if let Some(config) = config {
        cmd.env("OPENCODE_CONFIG", config);
    }
    cmd.env("OPENCODE_DISABLE_AUTOUPDATE", "1");
    let mut content = agent.and_then(|agent| serde_json::from_str::<serde_json::Value>(&agent.content).ok()).unwrap_or_else(|| serde_json::json!({}));
    let provider = opts.model.split('/').next().unwrap_or_default();
    for (id, base) in crate::key_proxy::shield(&mut cmd, &[provider]).bases {
        content["provider"][id]["options"]["baseURL"] = serde_json::Value::String(provider_base(id, &base));
    }
    if content.as_object().is_some_and(|object| !object.is_empty()) {
        cmd.env("OPENCODE_CONFIG_CONTENT", content.to_string());
    }
    if agent.is_some() {
        cmd.env("OPENCODE_DISABLE_PROJECT_CONFIG", "1");
    }
    if !opts.lean && opts.approval != crate::cli::Approval::ReadOnly && crate::claude::sandbox::mode() == crate::claude::sandbox::Mode::Off {
        crate::cmd_hub::shield(&mut cmd, &opts.environment, crate::cmd_hub::Hook::Shell);
    }
    if opts.lean {
        for flag in ["OPENCODE_DISABLE_CLAUDE_CODE", "OPENCODE_DISABLE_EXTERNAL_SKILLS"] {
            cmd.env(flag, "1");
        }
    }
    cmd.current_dir(opts.working_dir);
    cmd.kill_on_drop(true);
    Ok(cmd)
}

fn random_suffix(len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for byte in uuid::Uuid::new_v4().as_bytes() {
            if out.len() < len {
                out.push(ALPHABET[*byte as usize % ALPHABET.len()] as char);
            }
        }
    }
    out
}

fn fresh_id(old: &str, ids: &mut std::collections::HashMap<String, String>) -> String {
    ids.entry(old.to_string())
        .or_insert_with(|| {
            let keep = old.len().min(16);
            format!("{}{}", &old[..keep], random_suffix(old.len().saturating_sub(keep).max(10)))
        })
        .clone()
}

async fn project_of(dir: &Path) -> (String, String) {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let git = |args: &'static [&'static str]| {
        let mut cmd = job_command("git");
        cmd.arg("-C").arg(&canonical).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        async move { cmd.output().await.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) }
    };
    let roots = git(&["rev-list", "--max-parents=0", "--all"]).await.unwrap_or_default();
    let mut roots: Vec<&str> = roots.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    roots.sort_unstable();
    let top = git(&["rev-parse", "--show-toplevel"]).await;
    match (roots.first(), top) {
        (Some(root), Some(top)) => {
            let path = canonical.strip_prefix(&top).map(|p| p.display().to_string()).unwrap_or_default();
            (root.to_string(), path)
        }
        _ => ("global".into(), canonical.display().to_string().trim_start_matches('/').to_string()),
    }
}

fn rewrite_export(mut export: serde_json::Value, directory: &Path, project: &str, path: &str, keep_through: Option<&str>) -> Result<(String, serde_json::Value)> {
    let mut ids = std::collections::HashMap::new();
    let messages = export.get_mut("messages").and_then(|m| m.as_array_mut()).context("opencode export has no messages")?;
    if let Some(after) = keep_through {
        let at = messages
            .iter()
            .position(|m| m.pointer("/info/id").and_then(|v| v.as_str()) == Some(after))
            .context("could not find the rewound turn in the opencode session")?;
        messages.truncate(at + 1);
    }
    let old_session = export.pointer("/info/id").and_then(|v| v.as_str()).context("opencode export has no session id")?.to_string();
    let session = fresh_id(&old_session, &mut ids);
    let info = export.get_mut("info").and_then(|v| v.as_object_mut()).context("opencode export has no session info")?;
    info.insert("id".into(), session.clone().into());
    info.insert("directory".into(), directory.display().to_string().into());
    info.insert("projectID".into(), project.into());
    info.insert("path".into(), path.into());
    for key in ["parentID", "share", "revert"] {
        info.remove(key);
    }
    for message in export["messages"].as_array_mut().into_iter().flatten() {
        let old = message.pointer("/info/id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let id = fresh_id(&old, &mut ids);
        if let Some(info) = message.get_mut("info").and_then(|v| v.as_object_mut()) {
            info.insert("id".into(), id.clone().into());
            info.insert("sessionID".into(), session.clone().into());
            if let Some(parent) = info.get("parentID").and_then(|v| v.as_str()).map(String::from) {
                info.insert("parentID".into(), fresh_id(&parent, &mut ids).into());
            }
        }
        for part in message.get_mut("parts").and_then(|v| v.as_array_mut()).into_iter().flatten() {
            if let Some(part) = part.as_object_mut() {
                if let Some(old) = part.get("id").and_then(|v| v.as_str()).map(String::from) {
                    part.insert("id".into(), fresh_id(&old, &mut ids).into());
                }
                part.insert("sessionID".into(), session.clone().into());
                part.insert("messageID".into(), id.clone().into());
            }
        }
    }
    Ok((session, export))
}

async fn branch(opts: &CliOptions<'_>, config: Option<&Path>, sid: &str, keep_through: Option<&str>) -> Result<String> {
    let mut export = base_command(opts, config, None)?;
    export.arg("export").arg(sid).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let out = export.output().await.context("Failed to run opencode export")?;
    anyhow::ensure!(out.status.success(), "opencode export failed for session {sid}");
    let text = String::from_utf8_lossy(&out.stdout);
    let json = text.find('{').map(|at| &text[at..]).context("opencode export printed no session")?;
    let exported: serde_json::Value = serde_json::from_str(json).context("opencode export is not JSON")?;
    let directory = std::fs::canonicalize(opts.working_dir).unwrap_or_else(|_| opts.working_dir.to_path_buf());
    let (project, path) = project_of(&directory).await;
    let (session, rewritten) = rewrite_export(exported, &directory, &project, &path, keep_through)?;
    let root = crate::cli::temp_dir().join(format!("loka-opencode-branch-{}", uuid::Uuid::new_v4().simple()));
    let mut dir = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    dir.mode(0o700);
    dir.create(&root).await?;
    let guard = RunConfig { file: root.join("session.json"), root };
    tokio::fs::write(&guard.file, serde_json::to_vec(&rewritten)?).await?;
    give_to_job_user(&guard.root)?;
    give_to_job_user(&guard.file)?;
    let mut import = base_command(opts, config, None)?;
    import.arg("import").arg(&guard.file).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let status = import.status().await.context("Failed to run opencode import")?;
    anyhow::ensure!(status.success(), "opencode import failed");
    Ok(session)
}

#[derive(Default)]
struct Turn {
    session_id: Option<String>,
    texts: Vec<(String, String)>,
    last_message: Option<String>,
    usage: crate::cli::Usage,
    cost_usd: f64,
    error: Option<String>,
    tools: u32,
}

impl Turn {
    fn output(&self) -> String {
        let Some((last, _)) = self.texts.last() else {
            return String::new();
        };
        self.texts.iter().filter(|(message, _)| message == last).map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n\n")
    }

    fn feed(&mut self, json: &serde_json::Value, events: Option<&crate::cli::EventSender>) {
        if self.session_id.is_none() {
            if let Some(sid) = json.get("sessionID").and_then(|v| v.as_str()) {
                self.session_id = Some(sid.to_string());
                if let Some(tx) = events {
                    let _ = tx.send(StreamEvent::SessionStarted { session_id: sid.to_string() });
                }
            }
        }
        let part = json.get("part").unwrap_or(&serde_json::Value::Null);
        let text = |v: &serde_json::Value, p: &str| v.pointer(p).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match json.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                let body = text(part, "/text");
                if body.trim().is_empty() {
                    return;
                }
                if let Some(tx) = events {
                    let _ = tx.send(StreamEvent::AssistantText(body.trim().chars().take(300).collect()));
                }
                self.texts.push((text(part, "/messageID"), body));
            }
            "tool_use" => {
                self.tools += 1;
                let Some(tx) = events else {
                    return;
                };
                let tool = text(part, "/tool");
                let input = part.pointer("/state/input").cloned().unwrap_or(serde_json::Value::Null);
                let _ = tx.send(StreamEvent::ToolUse { tool: tool.clone(), input_summary: crate::cli::summarize_tool_input(&tool, &input) });
                let output = part.pointer("/state/output").or_else(|| part.pointer("/state/error")).map(crate::cli::summarize_tool_output).unwrap_or_default();
                let output = crate::cli::truncate_for_log(output.trim(), crate::cli::TOOL_OUTPUT_MAX_CHARS);
                if !output.is_empty() {
                    let _ = tx.send(StreamEvent::ToolResult { tool, output });
                }
            }
            "step_finish" => {
                let n = |p: &str| part.pointer(p).and_then(|v| v.as_u64()).unwrap_or(0);
                let (input, read, write) = (n("/tokens/input"), n("/tokens/cache/read"), n("/tokens/cache/write"));
                self.usage.input_tokens += input + write;
                self.usage.cached_input_tokens += read;
                self.usage.output_tokens += n("/tokens/output") + n("/tokens/reasoning");
                self.usage.context_tokens = input + read + write;
                self.cost_usd += part.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(message) = part.get("messageID").and_then(|v| v.as_str()) {
                    self.last_message = Some(message.to_string());
                }
                if let Some(tx) = events {
                    let _ = tx.send(StreamEvent::Usage(self.usage));
                }
            }
            "error" => {
                let message = json
                    .pointer("/error/data/message")
                    .or_else(|| json.pointer("/error/message"))
                    .or_else(|| json.pointer("/error/name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                if let Some(tx) = events {
                    let _ = tx.send(StreamEvent::Error(message.clone()));
                }
                self.error = Some(message);
            }
            _ => {}
        }
    }
}

pub(crate) async fn acp_setup(opts: &CliOptions<'_>) -> Result<crate::acp::Setup> {
    let config = write_run_config(opts.mcp_config_json.as_deref().unwrap_or("{}")).await?;
    let agent = run_agent(opts).await?;
    let mut command = base_command(opts, Some(&config.file), agent.as_ref())?;
    command.arg("acp");
    let mode = agent.as_ref().map(|agent| agent.name.to_string());
    let mut keep: Vec<Box<dyn Send>> = vec![Box::new(config)];
    if let Some(agent) = agent {
        keep.push(Box::new(agent));
    }
    Ok(crate::acp::Setup { command, mode, model: Some(opts.model.to_string()), mcp_servers: serde_json::json!([]), keep })
}

async fn run_inner(opts: &CliOptions<'_>, config: Option<&Path>) -> Result<CliResult> {
    let branched = match (opts.session_id, opts.resume) {
        (Some(sid), true) if opts.fork || opts.rewind_to.is_some() => {
            let branched = branch(opts, config, sid, opts.rewind_to).await?;
            info!(from = sid, to = %branched, rewind = opts.rewind_to.is_some(), "Branched opencode session");
            Some(branched)
        }
        _ => None,
    };
    let agent = run_agent(opts).await?;
    let mut cmd = base_command(opts, config, agent.as_ref())?;
    cmd.arg("run");
    match &agent {
        Some(agent) => {
            cmd.arg("--agent").arg(agent.name);
        }
        None => {
            cmd.arg("--dangerously-skip-permissions");
        }
    }
    cmd.arg("--format").arg("json");
    cmd.arg("--dir").arg(opts.working_dir);
    cmd.arg("-m").arg(opts.model);
    if let Some(effort) = opts.effort {
        cmd.arg("--variant").arg(effort);
    }
    match (branched.as_deref(), opts.session_id) {
        (Some(sid), _) => {
            cmd.arg("-s").arg(sid);
        }
        (None, Some(sid)) if opts.resume => {
            cmd.arg("-s").arg(sid);
        }
        _ => {}
    }
    for image_path in &opts.image_paths {
        cmd.arg("--file").arg(image_path);
    }

    let _sandbox_temp = if opts.lean {
        None
    } else {
        let (wrapped, temp) = crate::sandbox_wrap::wrap("opencode", cmd, &sandbox_spec(opts, config, agent.as_ref()), &opts.environment)?;
        cmd = wrapped;
        temp
    };

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(dir = %opts.working_dir.display(), model = opts.model, lean = opts.lean, "Invoking opencode CLI");

    let mut child = cmd
        .spawn()
        .context("Failed to spawn opencode CLI. Is it installed? Run: curl -fsSL https://opencode.ai/install | bash")?;
    crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);

    let prompt = full_prompt(opts);
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await?;
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

    let mut lines = BufReader::new(child.stdout.take().expect("stdout piped")).lines();
    let mut turn = Turn::default();
    while let Some(line) = lines.next_line().await? {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            turn.feed(&json, opts.event_tx.as_ref());
        }
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = collect_stderr(stderr_handle).await;
    drop(agent);
    debug!(exit_code, stderr_len = stderr.len(), tools = turn.tools, "opencode CLI finished");
    if !stderr.is_empty() && (opts.environment.is_empty() && opts.provider_keys.is_empty()) && !opts.mcp_has_secrets && !opts.sensitive {
        debug!(stderr = %stderr, "opencode stderr");
    }

    let output = turn.output();
    if let Some(error) = turn.error.as_deref().filter(|_| output.is_empty()) {
        anyhow::bail!("opencode returned error: {error}");
    }
    if exit_code != 0 && output.is_empty() {
        anyhow::bail!("opencode CLI exited with code {exit_code}: {stderr}");
    }
    info!(exit_code, cost_usd = turn.cost_usd, session_id = turn.session_id.as_deref().unwrap_or("none"), "opencode CLI completed");

    Ok(CliResult {
        output,
        session_id: turn.session_id.or(branched),
        cost_usd: turn.cost_usd,
        exit_code,
        usage: turn.usage,
        checkpoint: turn.last_message,
    })
}

#[cfg(test)]
mod tests {
    use super::{opencode_mcp, rewrite_export};

    #[tokio::test]
    #[ignore]
    async fn live_opencode_sends_its_key_through_the_proxy() {
        crate::key_proxy::tests::test_proxy();
        let (upstream, seen) = crate::key_proxy::tests::upstream().await;
        let home = std::env::temp_dir().join(format!("opencode-key-proxy-{}", std::process::id()));
        for (model, var, value, base_var, base) in [
            ("anthropic/claude-sonnet-5", "ANTHROPIC_API_KEY", "sk-ant-api03-oc-real", "ANTHROPIC_BASE_URL", upstream.clone()),
            ("openai/gpt-5", "OPENAI_API_KEY", "sk-oc-openai-real", "OPENAI_BASE_URL", format!("{upstream}/v1")),
        ] {
            seen.lock().unwrap().clear();
            let mut cmd = tokio::process::Command::new(std::env::var("OPENCODE_BIN").unwrap_or_else(|_| "opencode".into()));
            for (name, sub) in [("HOME", ""), ("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state")] {
                let dir = home.join(sub);
                std::fs::create_dir_all(&dir).unwrap();
                cmd.env(name, dir);
            }
            cmd.current_dir(&home);
            cmd.env("OPENCODE_DISABLE_AUTOUPDATE", "1");
            cmd.env(var, value);
            cmd.env(base_var, &base);
            let provider = model.split('/').next().unwrap();
            let mut content = serde_json::json!({});
            for (id, route) in crate::key_proxy::shield(&mut cmd, &[provider]).bases {
                content["provider"][id]["options"]["baseURL"] = serde_json::Value::String(super::provider_base(id, &route));
            }
            cmd.env("OPENCODE_CONFIG_CONTENT", content.to_string());
            cmd.args(["run", "-m", model, "hi"]);
            let output = crate::key_proxy::tests::run_cli(cmd).await;
            let hits = seen.lock().unwrap().clone();
            assert!(hits.iter().any(|(_, key)| key.contains(value)), "{model}: {hits:?} {output}");
            assert!(!hits.iter().any(|(_, key)| key.contains("loka-")), "{model}: {hits:?}");
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn branches_keep_order_and_drop_later_turns() {
        let export = json!({
            "info": {"id": "ses_e343463a2ffezKTnCInw4bfYBr", "directory": "/old", "projectID": "global", "parentID": "ses_x"},
            "messages": [
                {"info": {"id": "msg_1cbcb9c6f001EgOGywloKA2zGN", "sessionID": "ses_e343463a2ffezKTnCInw4bfYBr"}, "parts": [{"id": "prt_1cbcb9c72001KoSig322lNkQaQ", "messageID": "msg_1cbcb9c6f001EgOGywloKA2zGN"}]},
                {"info": {"id": "msg_1cbcb9d3e001qACIMs3Q66k1j8", "parentID": "msg_1cbcb9c6f001EgOGywloKA2zGN"}, "parts": []},
                {"info": {"id": "msg_1cbcbb5f1001yIj2yI8Km8JIa0"}, "parts": []}
            ]
        });
        let (session, out) = rewrite_export(export.clone(), std::path::Path::new("/new"), "abc", "", Some("msg_1cbcb9d3e001qACIMs3Q66k1j8")).unwrap();
        assert_ne!(session, "ses_e343463a2ffezKTnCInw4bfYBr");
        assert!(session.starts_with("ses_e343463a2ffe"));
        assert_eq!(out["info"]["directory"], "/new");
        assert_eq!(out["info"]["projectID"], "abc");
        assert!(out["info"].get("parentID").is_none());
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let first = messages[0]["info"]["id"].as_str().unwrap();
        assert!(first.starts_with("msg_1cbcb9c6f001") && first != "msg_1cbcb9c6f001EgOGywloKA2zGN");
        assert_eq!(messages[1]["info"]["parentID"], first);
        assert_eq!(messages[0]["parts"][0]["messageID"], first);
        assert_eq!(messages[0]["parts"][0]["sessionID"], session.as_str());
        assert!(rewrite_export(export, std::path::Path::new("/new"), "abc", "", Some("msg_missing")).is_err());
    }
    use serde_json::json;

    #[test]
    fn translates_claude_mcp_servers() {
        let source = json!({"mcpServers": {
            "world": {"command": "/bin/loka-agent", "args": ["mcp", "runner"], "env": {"WORLD_RUNNER_URL": "https://x"}},
            "docs": {"type": "http", "url": "https://docs", "headers": {"Authorization": "Bearer t"}}
        }});
        let config = opencode_mcp(&source.to_string()).unwrap();
        assert_eq!(config["mcp"]["world"]["type"], "local");
        assert_eq!(config["mcp"]["world"]["command"], json!(["/bin/loka-agent", "mcp", "runner"]));
        assert_eq!(config["mcp"]["world"]["environment"]["WORLD_RUNNER_URL"], "https://x");
        assert_eq!(config["mcp"]["docs"]["type"], "remote");
        assert_eq!(config["mcp"]["docs"]["headers"]["Authorization"], "Bearer t");
    }
}
