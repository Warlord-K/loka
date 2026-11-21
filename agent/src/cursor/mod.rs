use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, info, warn};

use crate::cli::{
    collect_stderr, configure_child_environment, job_command, session_id_from_init, summarize_tool_input,
    summarize_tool_output, truncate_for_log, workspace_command, CliOptions, CliResult, StreamEvent,
    TOOL_OUTPUT_MAX_CHARS,
};

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

const WORK_MODE: &str = "default";

const PLUGIN_NAME: &str = "loka-world";

pub(crate) mod chats;
mod sandbox;

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    let plugin_dir = if let Some(mcp_json) = opts.mcp_config_json.as_deref() {
        Some(write_mcp_plugin(mcp_json).await?)
    } else {
        None
    };
    let servers = opts
        .mcp_config_json
        .as_deref()
        .map(mcp_server_names)
        .unwrap_or_default();
    let host = host_servers(opts.working_dir);
    let project_permissions = if servers.is_empty() && host.is_empty() && crate::cli::runner_private_paths().is_empty() {
        None
    } else {
        Some(permit_mcp_tools(opts.working_dir, &servers, &host).await?)
    };

    let result = run_inner(
        &opts,
        plugin_dir.as_ref().map(|plugin| plugin.path.as_path()),
    )
    .await;
    drop(project_permissions);
    result
}

pub(crate) async fn acp_setup(opts: &CliOptions<'_>) -> Result<crate::acp::Setup> {
    let mut command = job_command("cursor-agent");
    configure_child_environment(&mut command, &opts.child_environment())?;
    shield_commands(&mut command, opts);
    command.arg("acp");
    let mut keep: Vec<Box<dyn Send>> = Vec::new();
    let host = host_servers(opts.working_dir);
    if !host.is_empty() || !crate::cli::runner_private_paths().is_empty() {
        keep.push(Box::new(permit_mcp_tools(opts.working_dir, &[], &host).await?));
    }
    let sandbox_mode = crate::claude::sandbox::mode();
    if sandbox_mode != crate::claude::sandbox::Mode::Off {
        let temp = sandbox::run_temp().context("Failed to create the Cursor run temp dir")?;
        command.env("TMPDIR", &temp.0);
        if let Some(policy) = sandbox::policy(crate::claude::sandbox::mode(), opts.working_dir, &opts.readable_files, Some(&temp.0)) {
            keep.push(Box::new(install_sandbox_policy(opts.working_dir, &policy).await?));
        }
        keep.push(Box::new(temp));
    }
    if let Some(sid) = opts.session_id.filter(|_| opts.resume) {
        chats::to_acp(sid).await?;
    }
    Ok(crate::acp::Setup {
        command,
        mode: Some(if opts.lean { "ask" } else { "agent" }.into()),
        model: Some(opts.model.to_string()),
        mcp_servers: crate::acp::mcp_servers(opts.mcp_config_json.as_deref()),
        keep,
    })
}

pub(crate) async fn after_acp_turn(session: &str, working_dir: &Path) -> Option<String> {
    if let Err(error) = chats::from_acp(session, working_dir).await {
        warn!(error = %format!("{error:#}"), "Could not copy the warm Cursor chat back");
    }
    chats::root_of(session).await
}

struct TempPlugin {
    root: PathBuf,
    path: PathBuf,
}

impl Drop for TempPlugin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn write_mcp_plugin(mcp_json: &str) -> Result<TempPlugin> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let root = std::env::temp_dir().join(format!("loka-cursor-plugin-{id}"));
    let path = root.join(PLUGIN_NAME);
    let manifest_dir = path.join(".cursor-plugin");
    tokio::fs::create_dir_all(&path).await.with_context(|| {
        format!(
            "Failed to create Cursor plugin directory: {}",
            path.display()
        )
    })?;
    tokio::fs::create_dir(&manifest_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create Cursor plugin manifest directory: {}",
                manifest_dir.display()
            )
        })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&root, &path, &manifest_dir] {
            tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .await
                .with_context(|| {
                    format!(
                        "Failed to set Cursor plugin directory permissions: {}",
                        dir.display()
                    )
                })?;
        }
    }

    let result = async {
        write_private_file(
            &manifest_dir.join("plugin.json"),
            format!(r#"{{"name":"world-run-{id}","mcpServers":".mcp.json"}}"#).as_bytes(),
        )
        .await?;
        write_private_file(&path.join(".mcp.json"), mcp_json.as_bytes()).await
    }
    .await;

    if let Err(error) = result {
        let _ = tokio::fs::remove_dir_all(&root).await;
        return Err(error);
    }

    Ok(TempPlugin { root, path })
}

struct TempProjectPermissions {
    path: PathBuf,
    original: Option<(Vec<u8>, std::fs::Permissions)>,
    created_dir: bool,
}

impl Drop for TempProjectPermissions {
    fn drop(&mut self) {
        let result = if let Some((contents, permissions)) = &self.original {
            restore_file(&self.path, contents, permissions.clone())
        } else {
            std::fs::remove_file(&self.path)
        };
        if let Err(error) = result {
            warn!(path = %self.path.display(), %error, "Failed to restore Cursor project permissions");
        }
        if self.created_dir {
            let _ = std::fs::remove_dir(self.path.parent().expect("permission file has parent"));
        }
    }
}

fn mcp_server_names(mcp_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(mcp_json)
        .ok()
        .and_then(|value| value.get("mcpServers")?.as_object().cloned())
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

fn mcp_permission(server: &str) -> String {
    format!("Mcp(plugin-{PLUGIN_NAME}-{server}:*)")
}

async fn hide_from_git(working_dir: &Path) {
    hide_from_git_as(working_dir, "cli.json").await
}

async fn hide_from_git_as(working_dir: &Path, name: &str) {
    let output = workspace_command("git", working_dir)
        .await
        .args(["rev-parse", "--show-prefix", "--git-path", "info/exclude"])
        .output()
        .await;
    let Ok(output) = output else { return };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let (Some(prefix), Some(exclude)) = (lines.next(), lines.next()) else {
        return;
    };
    let exclude = working_dir.join(exclude);
    let pattern = format!("/{prefix}.cursor/{name}");
    let existing = tokio::fs::read_to_string(&exclude).await.unwrap_or_default();
    if existing.lines().any(|line| line == pattern) {
        return;
    }
    if let Some(parent) = exclude.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    let contents = format!("{existing}{separator}{pattern}\n");
    if let Err(error) = tokio::fs::write(&exclude, contents).await {
        warn!(path = %exclude.display(), %error, "Failed to hide Cursor permissions from git");
    }
}

fn host_servers(working_dir: &Path) -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let files = home.map(|home| home.join(".cursor").join("mcp.json")).into_iter().chain([working_dir.join(".cursor").join("mcp.json")]);
    let mut names: Vec<String> = files
        .filter_map(|file| std::fs::read(file).ok())
        .flat_map(|bytes| mcp_server_names(&String::from_utf8_lossy(&bytes)))
        .collect();
    names.sort();
    names.dedup();
    names
}

async fn permit_mcp_tools(
    working_dir: &Path,
    servers: &[String],
    host: &[String],
) -> Result<TempProjectPermissions> {
    let cursor_dir = working_dir.join(".cursor");
    let created_dir = match tokio::fs::create_dir(&cursor_dir).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            return Err(error).context("Failed to create Cursor project config directory")
        }
    };
    let path = cursor_dir.join("cli.json");
    let original = match tokio::fs::read(&path).await {
        Ok(contents) => Some((contents, tokio::fs::metadata(&path).await?.permissions())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("Failed to read Cursor project permissions"),
    };
    let mut config = match &original {
        Some((contents, _)) => serde_json::from_slice(contents)
            .context("Failed to parse Cursor project permissions")?,
        None => serde_json::json!({}),
    };
    let config = config
        .as_object_mut()
        .context("Cursor project permissions must be a JSON object")?;
    let permissions = config
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Cursor project permissions.permissions must be a JSON object")?;
    let deny = permissions
        .entry("deny")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .context("Cursor project permissions.permissions.deny must be an array")?;
    for name in host {
        let rule = serde_json::Value::String(format!("Mcp({name}:*)"));
        if !deny.contains(&rule) {
            deny.push(rule);
        }
    }
    for path in crate::cli::runner_private_paths() {
        for rule in [format!("Read({})", path.display()), format!("Read({}/**)", path.display())] {
            let rule = serde_json::Value::String(rule);
            if !deny.contains(&rule) {
                deny.push(rule);
            }
        }
    }
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .context("Cursor project permissions.permissions.allow must be an array")?;
    for server in servers {
        let permission = serde_json::Value::String(mcp_permission(server));
        if !allow.contains(&permission) {
            allow.push(permission);
        }
    }
    write_atomic_private_file(&path, &serde_json::to_vec(&config)?).await?;
    if original.is_none() {
        hide_from_git(working_dir).await;
    }
    Ok(TempProjectPermissions {
        path,
        original,
        created_dir,
    })
}

async fn write_atomic_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().simple()));
    if let Err(error) = write_private_file(&temp, contents).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).context("Failed to install Cursor project permissions");
    }
    Ok(())
}

fn restore_file(
    path: &Path,
    contents: &[u8],
    permissions: std::fs::Permissions,
) -> std::io::Result<()> {
    use std::io::Write;

    let temp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::set_permissions(&temp, permissions)?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

async fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .with_context(|| format!("Failed to create Cursor plugin file: {}", path.display()))?;
    file.write_all(contents)
        .await
        .with_context(|| format!("Failed to write Cursor plugin file: {}", path.display()))?;
    file.flush()
        .await
        .with_context(|| format!("Failed to write Cursor plugin file: {}", path.display()))
}

fn resumed_session<'a>(opts: &'a CliOptions<'_>) -> Option<&'a str> {
    opts.resume.then_some(opts.session_id).flatten()
}

async fn session_to_resume(opts: &CliOptions<'_>) -> Option<String> {
    let sid = resumed_session(opts)?;
    if opts.rewind_to.is_none() && !opts.fork {
        return Some(sid.to_string());
    }
    match chats::branch(sid, opts.working_dir, opts.rewind_to).await {
        Ok(branched) => {
            info!(from = sid, to = %branched, rewind = opts.rewind_to.is_some(), "Branched Cursor chat");
            Some(branched)
        }
        Err(error) => {
            warn!(error = %format!("{error:#}"), "Could not branch the Cursor chat, starting a fresh one");
            None
        }
    }
}

fn full_prompt(opts: &CliOptions<'_>, resuming: bool) -> String {
    let mut parts = Vec::new();
    if !resuming {
        parts.push(AGENT_PREAMBLE.to_string());
        if let Some(system) = opts.system_prompt {
            parts.push(system.to_string());
        }
    }
    parts.push(opts.prompt.to_string());
    if !opts.image_paths.is_empty() {
        let listed: Vec<String> = opts.image_paths.iter().map(|path| format!("- {}", path.display())).collect();
        parts.push(format!("Attached images (open each with your file read tool):\n{}", listed.join("\n")));
    }
    parts.join("\n\n")
}

async fn install_sandbox_policy(working_dir: &Path, policy: &serde_json::Value) -> Result<TempProjectPermissions> {
    let cursor_dir = working_dir.join(".cursor");
    let created_dir = match tokio::fs::create_dir(&cursor_dir).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error).context("Failed to create Cursor project config directory"),
    };
    let path = cursor_dir.join("sandbox.json");
    let original = match tokio::fs::read(&path).await {
        Ok(contents) => Some((contents, tokio::fs::metadata(&path).await?.permissions())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("Failed to read the Cursor sandbox policy"),
    };
    write_atomic_private_file(&path, &serde_json::to_vec(policy)?).await?;
    if original.is_none() {
        hide_from_git_as(working_dir, "sandbox.json").await;
    }
    Ok(TempProjectPermissions { path, original, created_dir })
}

fn usage_of(result: &serde_json::Value) -> crate::cli::Usage {
    let n = |key: &str| result.pointer(&format!("/usage/{key}")).and_then(|v| v.as_u64()).unwrap_or(0);
    crate::cli::Usage {
        input_tokens: n("inputTokens") + n("cacheWriteTokens"),
        cached_input_tokens: n("cacheReadTokens"),
        output_tokens: n("outputTokens"),
        context_tokens: 0,
    }
}

fn cursor_tool_call(json: &serde_json::Value) -> Option<(String, &serde_json::Value)> {
    let call = json.get("tool_call")?.as_object()?;
    let (kind, value) = call
        .iter()
        .find(|(key, value)| key.ends_with("ToolCall") && value.is_object())?;
    let args = value.get("args")?;
    let tool = if kind == "mcpToolCall" {
        args.get("toolName")?.as_str()?.to_string()
    } else {
        let name = kind.strip_suffix("ToolCall")?;
        match name {
            "shell" => "Bash".to_string(),
            "read" => "Read".to_string(),
            "grep" => "Grep".to_string(),
            "glob" => "Glob".to_string(),
            "write" => "Write".to_string(),
            "edit" => "Edit".to_string(),
            "task" => "Agent".to_string(),
            _ => {
                let mut chars = name.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())?
            }
        }
    };
    Some((tool, args))
}

fn cursor_tool_summary(tool: &str, input: &serde_json::Value) -> String {
    let summary = summarize_tool_input(tool, input);
    if !summary.is_empty() {
        return summary;
    }
    for field in ["path", "command", "description", "pattern"] {
        if let Some(value) = input.get(field).and_then(|value| value.as_str()) {
            return truncate_for_log(value, 300);
        }
    }
    String::new()
}

fn shield_commands(cmd: &mut tokio::process::Command, opts: &CliOptions<'_>) {
    if !opts.lean && crate::claude::sandbox::mode() == crate::claude::sandbox::Mode::Off {
        crate::cmd_hub::shield(cmd, &opts.environment, crate::cmd_hub::Hook::Shell);
    }
}

async fn run_inner(opts: &CliOptions<'_>, plugin_dir: Option<&Path>) -> Result<CliResult> {
    let mut cmd = job_command("cursor-agent");
    cmd.current_dir(opts.working_dir);
    cmd.kill_on_drop(true);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    shield_commands(&mut cmd, opts);

    cmd.arg("-p");
    cmd.arg("--output-format").arg("stream-json");

    let mut readable = opts.readable_files.clone();
    readable.extend(opts.image_paths.iter().cloned());
    let sandbox_mode = crate::claude::sandbox::mode();
    let sandboxed = sandbox_mode != crate::claude::sandbox::Mode::Off;
    let run_temp = if sandboxed { Some(sandbox::run_temp().context("Failed to create the Cursor run temp dir")?) } else { None };
    if let Some(temp) = &run_temp {
        cmd.env("TMPDIR", &temp.0);
    }
    let policy = sandbox::policy(sandbox_mode, opts.working_dir, &readable, run_temp.as_ref().map(|t| t.0.as_path()));
    let _policy_file = match &policy {
        Some(policy) => Some(install_sandbox_policy(opts.working_dir, policy).await?),
        None => None,
    };
    if opts.lean {
        cmd.arg("--mode").arg("ask");
    } else if opts.approval == crate::cli::Approval::ReadOnly {
        cmd.arg("--mode").arg("plan");
    } else if policy.is_none() {
        cmd.arg("--force");
    }
    cmd.arg("--sandbox").arg(if policy.is_some() { "enabled" } else { "disabled" });
    cmd.arg("--trust");

    cmd.arg("--model").arg(opts.model);

    let resume = session_to_resume(opts).await;
    if let Some(sid) = &resume {
        if !opts.lean && opts.approval != crate::cli::Approval::ReadOnly {
            if let Err(error) = chats::set_mode(sid, WORK_MODE).await {
                warn!(error = %format!("{error:#}"), "Could not switch the Cursor chat to agent mode");
            }
        }
        cmd.arg("--resume").arg(sid);
    }

    if let Some(path) = plugin_dir {
        cmd.arg("--plugin-dir").arg(path);
        cmd.arg("--approve-mcps");
    }

    let full_prompt = full_prompt(opts, resume.is_some());

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    tracing::debug!(
        dir = %opts.working_dir.display(),
        model = opts.model,
        resuming = resume.is_some(),
        sandboxed = policy.is_some(),
        "Invoking Cursor Agent"
    );

    let mut child = cmd.spawn().context(
        "Failed to spawn cursor-agent CLI — is it installed and logged in? Run: cursor-agent login",
    )?;
    crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(full_prompt.as_bytes()).await?;
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

    let mut result_text = String::new();
    let mut last_assistant_text = String::new();
    let mut session_id: Option<String> = None;
    let mut is_error_response = false;
    let mut usage = crate::cli::Usage::default();
    let mut tool_use_names: HashMap<String, String> = HashMap::new();

    let silent_after = crate::cli::silent_after();
    let mut heard = false;
    loop {
        let next = match silent_after.filter(|_| !heard) {
            Some(limit) => match tokio::time::timeout(limit, lines.next_line()).await {
                Ok(next) => next?,
                Err(_) => {
                    if let Some(pid) = child.id() {
                        crate::cli::kill_process_trees(vec![pid]).await;
                    }
                    let _ = child.kill().await;
                    warn!(secs = limit.as_secs(), model = opts.model, "Cursor Agent printed nothing, stopping it");
                    anyhow::bail!("Cursor Agent printed nothing for {} minutes, so the run was stopped", limit.as_secs() / 60);
                }
            },
            None => lines.next_line().await?,
        };
        let Some(line) = next else { break };
        heard = true;
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        if session_id.is_none() {
            if let Some(sid) = session_id_from_init("cursor", &json) {
                session_id = Some(sid.to_string());
                if let Some(tx) = &opts.event_tx {
                    let _ = tx.send(StreamEvent::SessionStarted {
                        session_id: sid.to_string(),
                    });
                }
            }
        }

        match json.get("type").and_then(|v| v.as_str()) {
            Some("tool_call") => {
                let call_id = json.get("call_id").and_then(|value| value.as_str());
                let subtype = json.get("subtype").and_then(|value| value.as_str());
                if let Some((tool, input)) = cursor_tool_call(&json) {
                    if subtype == Some("started") {
                        if let Some(call_id) = call_id {
                            tool_use_names.insert(call_id.to_string(), tool.clone());
                        }
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx.send(StreamEvent::ToolUse {
                                tool: tool.clone(),
                                input_summary: cursor_tool_summary(&tool, input),
                            });
                        }
                    } else if subtype == Some("completed") {
                        if let Some(tx) = &opts.event_tx {
                            let tool = call_id
                                .and_then(|call_id| tool_use_names.get(call_id))
                                .cloned()
                                .unwrap_or(tool);
                            if let Some(result) = json
                                .get("tool_call")
                                .and_then(|call| call.as_object())
                                .and_then(|call| {
                                    call.values().find(|value| value.get("result").is_some())
                                })
                                .and_then(|value| value.get("result"))
                            {
                                let output = summarize_tool_output(result);
                                let output = truncate_for_log(output.trim(), TOOL_OUTPUT_MAX_CHARS);
                                if !output.is_empty() {
                                    let _ = tx.send(StreamEvent::ToolResult { tool, output });
                                }
                            }
                        }
                    }
                }
            }
            Some("assistant") => {
                if let Some(content) = json.pointer("/message/content").and_then(|v| v.as_array()) {
                    for block in content {
                        match block.get("type").and_then(|v| v.as_str()) {
                            Some("tool_use") => {
                                if let Some(tx) = &opts.event_tx {
                                    let tool = block
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let input = block
                                        .get("input")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    let summary = summarize_tool_input(tool, &input);
                                    if let Some(tool_use_id) =
                                        block.get("id").and_then(|v| v.as_str())
                                    {
                                        tool_use_names
                                            .insert(tool_use_id.to_string(), tool.to_string());
                                    }
                                    let _ = tx.send(StreamEvent::ToolUse {
                                        tool: tool.to_string(),
                                        input_summary: summary,
                                    });
                                }
                            }
                            Some("text") => {
                                let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                if !text.trim().is_empty() {
                                    last_assistant_text = text.to_string();
                                    if let Some(tx) = &opts.event_tx {
                                        let truncated: String =
                                            text.trim().chars().take(300).collect();
                                        let _ = tx.send(StreamEvent::AssistantText(truncated));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("user") => {
                if let Some(tx) = &opts.event_tx {
                    if let Some(content) = json.pointer("/message/content") {
                        let blocks: Vec<&serde_json::Value> =
                            if let Some(array) = content.as_array() {
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
                                .and_then(|tool_use_id| tool_use_names.get(tool_use_id))
                                .cloned()
                                .unwrap_or_else(|| "unknown".to_string());
                            let output = block
                                .get("content")
                                .map(summarize_tool_output)
                                .unwrap_or_default();
                            let output = truncate_for_log(output.trim(), TOOL_OUTPUT_MAX_CHARS);
                            if output.is_empty() {
                                continue;
                            }
                            let _ = tx.send(StreamEvent::ToolResult { tool, output });
                        }
                    }
                }
            }
            Some("result") => {
                result_text = json
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                    session_id = Some(sid.to_string());
                }
                usage = usage_of(&json);
                is_error_response = json
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            _ => {}
        }
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = collect_stderr(stderr_handle).await;

    debug!(
        exit_code,
        stderr_len = stderr.len(),
        "Cursor Agent finished"
    );
    if !stderr.is_empty()
        && (opts.environment.is_empty() && opts.provider_keys.is_empty())
        && !opts.mcp_has_secrets
        && !opts.sensitive
    {
        debug!(stderr = %stderr, "Cursor stderr");
    }

    let output = if !result_text.is_empty() {
        result_text
    } else {
        last_assistant_text
    };

    if is_error_response {
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::Error(output.clone()));
        }
        anyhow::bail!("Cursor Agent returned error: {output}");
    }

    if exit_code != 0 && output.is_empty() {
        anyhow::bail!("Cursor Agent exited with code {exit_code}: {stderr}");
    }

    if exit_code == 0 || exit_code == 143 {
        tracing::debug!(exit_code, session_id = session_id.as_deref().unwrap_or("none"), "Cursor Agent completed");
    } else {
        info!(exit_code, session_id = session_id.as_deref().unwrap_or("none"), "Cursor Agent exited with an error but returned output");
    }

    let checkpoint = match session_id.as_deref() {
        Some(sid) => chats::root_of(sid).await,
        None => None,
    };
    Ok(CliResult {
        output,
        session_id,
        cost_usd: 0.0,
        exit_code,
        usage,
        checkpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        cursor_tool_call, full_prompt, mcp_server_names, permit_mcp_tools, resumed_session,
        write_mcp_plugin,
    };
    use crate::cli::CliOptions;

    fn options<'a>(prompt: &'a str, fork: bool) -> CliOptions<'a> {
        CliOptions {
            working_dir: Path::new("/tmp"),
            prompt,
            system_prompt: Some("persona handoff"),
            allowed_tools: None,
            max_turns: 1,
            model: "test",
            mcp_config_json: None,
            session_id: Some("main-session"),
            resume: true,
            event_tx: None,
            image_paths: Vec::new(),
            fork,
            lean: false,
            effort: None,
            fast: false,
            environment: Default::default(),
            mcp_has_secrets: false,
            sensitive: false,
            rewind_to: None,
            approval: Default::default(),
            steer: Default::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files: Vec::new(),
        }
    }

    use std::path::Path;

    #[test]
    fn a_fork_that_cannot_branch_starts_fresh_with_full_prompt() {
        let opts = options("task brief", true);
        assert_eq!(resumed_session(&opts), Some("main-session"));
        let prompt = full_prompt(&opts, false);
        assert!(prompt.contains("persona handoff"));
        assert!(prompt.contains("task brief"));
    }

    #[test]
    fn followup_resumes_with_turn_only() {
        let opts = options("followup", false);
        assert_eq!(resumed_session(&opts), Some("main-session"));
        assert_eq!(full_prompt(&opts, true), "followup");
    }

    #[test]
    fn reads_usage_from_the_result() {
        let usage = super::usage_of(&serde_json::json!({"usage": {"inputTokens": 100, "outputTokens": 7, "cacheReadTokens": 50, "cacheWriteTokens": 3}}));
        assert_eq!((usage.input_tokens, usage.cached_input_tokens, usage.output_tokens), (103, 50, 7));
    }

    #[tokio::test]
    async fn writes_private_mcp_plugin() {
        let mcp_json = r#"{"mcpServers":{"example":{"command":"echo","env":{"TOKEN":"test"}}}}"#;
        let plugin = write_mcp_plugin(mcp_json)
            .await
            .expect("plugin should be written");
        let path = plugin.path.clone();
        let manifest = tokio::fs::read_to_string(path.join(".cursor-plugin/plugin.json"))
            .await
            .expect("manifest should be readable");
        let config = tokio::fs::read_to_string(path.join(".mcp.json"))
            .await
            .expect("MCP config should be readable");
        assert!(manifest.contains(r#""mcpServers":".mcp.json""#));
        assert_eq!(config, mcp_json);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&plugin.root, &path, &path.join(".cursor-plugin")] {
                assert_eq!(
                    tokio::fs::metadata(dir).await.unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            for file in [
                &path.join(".mcp.json"),
                &path.join(".cursor-plugin/plugin.json"),
            ] {
                assert_eq!(
                    tokio::fs::metadata(file)
                        .await
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }

        drop(plugin);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn grants_every_plugin_server_and_restores_project_config() {
        let root = std::env::temp_dir().join(format!(
            "loka-cursor-permissions-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cursor_dir = root.join(".cursor");
        tokio::fs::create_dir_all(&cursor_dir).await.unwrap();
        let path = cursor_dir.join("cli.json");
        let original = br#"{"permissions":{"allow":["Shell(git)"],"deny":["Shell(rm)"]}}"#;
        tokio::fs::write(&path, original).await.unwrap();

        let servers = ["world".to_string(), "slack".to_string()];
        let permissions = permit_mcp_tools(&root, &servers, &[]).await.unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(
            config.pointer("/permissions/allow/1").unwrap(),
            "Mcp(plugin-loka-world-world:*)"
        );
        assert_eq!(
            config.pointer("/permissions/allow/2").unwrap(),
            "Mcp(plugin-loka-world-slack:*)"
        );
        assert_eq!(config.pointer("/permissions/deny/0").unwrap(), "Shell(rm)");
        drop(permissions);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), original);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn creates_valid_fresh_project_config() {
        let root = std::env::temp_dir().join(format!(
            "loka-cursor-permissions-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir(&root).await.unwrap();

        let permissions = permit_mcp_tools(&root, &["linear".to_string()], &[])
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(root.join(".cursor/cli.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            config.pointer("/permissions/deny").unwrap(),
            &serde_json::json!([])
        );
        assert_eq!(
            config.pointer("/permissions/allow/0").unwrap(),
            "Mcp(plugin-loka-world-linear:*)"
        );
        drop(permissions);
        assert!(!root.join(".cursor").exists());
        tokio::fs::remove_dir(root).await.unwrap();
    }

    #[test]
    fn lists_every_mcp_server() {
        let mut names = mcp_server_names(r#"{"mcpServers":{"world":{},"linear":{}}}"#);
        names.sort();
        assert_eq!(names, ["linear", "world"]);
        assert!(mcp_server_names("not json").is_empty());
    }

    #[test]
    fn parses_cursor_mcp_tool_event() {
        let event = serde_json::json!({
            "tool_call": {
                "mcpToolCall": {
                    "args": {"toolName": "world_agents", "args": {}}
                }
            }
        });
        let (tool, input) = cursor_tool_call(&event).unwrap();
        assert_eq!(tool, "world_agents");
        assert_eq!(input["args"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn hides_fresh_permissions_from_git() {
        let root = std::env::temp_dir().join(format!(
            "loka-cursor-git-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir(&root).await.unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&root)
            .status()
            .unwrap();
        assert!(status.success());

        let permissions = permit_mcp_tools(&root, &["world".to_string()], &[])
            .await
            .unwrap();
        let untracked = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&untracked.stdout), "");
        drop(permissions);
        let again = permit_mcp_tools(&root, &["world".to_string()], &[])
            .await
            .unwrap();
        drop(again);
        let exclude = tokio::fs::read_to_string(root.join(".git/info/exclude"))
            .await
            .unwrap();
        assert_eq!(exclude.matches("/.cursor/cli.json").count(), 1);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    const ECHO_MCP: &str = r#"
import json, sys
word = sys.argv[1]
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg["params"].get("protocolVersion", "2024-11-05"), "capabilities": {"tools": {}}, "serverInfo": {"name": "vault", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "secret_word", "description": "Returns the secret word.", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": word}]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#;

    #[tokio::test]
    #[ignore]
    async fn live_mcp_tool_calls() {
        let model = std::env::var("CURSOR_TEST_MODEL").expect("set CURSOR_TEST_MODEL");
        let root = std::env::temp_dir().join(format!(
            "loka-cursor-live-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir(&root).await.unwrap();
        let script = root.join("vault.py");
        tokio::fs::write(&script, ECHO_MCP).await.unwrap();
        let workdir = root.join("work");
        tokio::fs::create_dir(&workdir).await.unwrap();
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&workdir)
            .status()
            .unwrap();

        for lean in [true, false] {
            let word = format!("pineapple{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            let mcp = serde_json::json!({
                "mcpServers": {
                    "vault": {"command": "python3", "args": [script.to_string_lossy(), word]}
                }
            })
            .to_string();
            let mut opts = options(
                "Call the secret_word tool from the vault MCP server and reply with exactly the word it returns.",
                true,
            );
            opts.working_dir = &workdir;
            opts.model = &model;
            opts.system_prompt = None;
            opts.session_id = None;
            opts.resume = false;
            opts.lean = lean;
            opts.mcp_config_json = Some(mcp);
            let result = super::run(opts).await.unwrap();
            eprintln!("lean={lean}: {}", result.output);
            assert!(result.output.contains(&word), "lean={lean}: {}", result.output);
            assert!(!workdir.join(".cursor").exists());
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
