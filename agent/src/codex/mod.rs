use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{debug, info};

pub mod app_server;

use crate::claude::sandbox;
use crate::cli::{
    collect_stderr, configure_child_environment, job_command, own_cli_state, session_id_from_init, summarize_tool_output,
    truncate_for_log, valid_environment_name, CliOptions, CliResult, StreamEvent,
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

pub fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|dir| !dir.is_empty()).map(|home| std::path::PathBuf::from(home).join(".codex")))
}

fn chatgpt_login(auth: &str) -> bool {
    let Ok(auth) = serde_json::from_str::<serde_json::Value>(auth) else {
        return false;
    };
    if let Some(mode) = auth.get("auth_mode").and_then(|mode| mode.as_str()) {
        return mode.to_ascii_lowercase().starts_with("chatgpt");
    }
    let has_key = auth.get("OPENAI_API_KEY").and_then(|key| key.as_str()).is_some_and(|key| !key.trim().is_empty());
    !has_key && auth.get("tokens").is_some_and(|tokens| !tokens.is_null())
}

pub(crate) fn exec_uses_chatgpt_login(cmd: &tokio::process::Command) -> bool {
    let codex_key = cmd.as_std().get_envs().any(|(name, value)| name == "CODEX_API_KEY" && value.is_some_and(|value| !value.is_empty()));
    uses_chatgpt_login(cmd, codex_key)
}

pub(crate) fn uses_chatgpt_login(cmd: &tokio::process::Command, api_key_login: bool) -> bool {
    if api_key_login {
        return false;
    }
    let env: std::collections::HashMap<String, String> = cmd
        .as_std()
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_str()?.to_string(), value?.to_str()?.to_string())))
        .collect();
    let dir = env
        .get("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| env.get("HOME").filter(|dir| !dir.is_empty()).map(|home| std::path::Path::new(home).join(".codex")))
        .or_else(home);
    let Some(dir) = dir else { return false };
    match std::fs::read_to_string(dir.join("auth.json")) {
        Ok(auth) => chatgpt_login(&auth),
        Err(_) => std::fs::read_to_string(dir.join("config.toml")).is_ok_and(|config| keyring_login(&config)),
    }
}

fn keyring_login(config: &str) -> bool {
    config
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.strip_prefix("cli_auth_credentials_store")?.trim_start().strip_prefix('='))
        .any(|value| matches!(value.trim().trim_matches(|c| c == '"' || c == '\''), "keyring" | "auto"))
}

pub fn rollout_thread_id(name: &str) -> Option<&str> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    (id.len() == 36 && id.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() }))
        .then_some(id)
}

pub fn find_rollout(sessions: &std::path::Path, thread_id: &str) -> Option<std::path::PathBuf> {
    let mut dirs = vec![sessions.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<std::path::PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                children.push(entry.path());
            } else if kind.is_file() && entry.file_name().to_str().and_then(rollout_thread_id) == Some(thread_id) {
                return Some(entry.path());
            }
        }
        children.sort();
        dirs.extend(children);
    }
    None
}

pub(crate) fn context_of(usage: &serde_json::Value) -> u64 {
    let n = |keys: [&str; 2]| keys.iter().find_map(|key| usage.get(*key).and_then(|v| v.as_u64()));
    n(["totalTokens", "total_tokens"])
        .unwrap_or_else(|| n(["inputTokens", "input_tokens"]).unwrap_or(0) + n(["outputTokens", "output_tokens"]).unwrap_or(0))
}

fn rollout_context_tokens(thread_id: &str) -> Option<u64> {
    let rollout = find_rollout(&home()?.join("sessions"), thread_id)?;
    let text = std::fs::read_to_string(rollout).ok()?;
    text.lines().rev().find_map(|line| {
        if !line.contains("\"token_count\"") {
            return None;
        }
        let event: serde_json::Value = serde_json::from_str(line).ok()?;
        event.pointer("/payload/info/last_token_usage").map(context_of)
    })
}

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    if crate::cli::compaction_run("codex", &opts) {
        let thread = opts.session_id.unwrap_or_default();
        return app_server::compact(&opts, thread).await;
    }
    if !opts.lean && opts.rewind_to.is_none() {
        if let Some(steer) = opts.steer.take() {
            return run_steerable(&opts, steer).await;
        }
    }
    let (Some(head), Some(sid), true) = (opts.rewind_to, opts.session_id, opts.resume) else {
        return run_exec(opts).await;
    };
    let rewound = app_server::rewind(&opts, sid, head).await?;
    info!(from = sid, to = rewound.as_deref().unwrap_or("fresh"), "Rewound Codex thread");
    run_exec(CliOptions {
        session_id: rewound.as_deref(),
        resume: rewound.is_some(),
        fork: false,
        rewind_to: None,
        approval: Default::default(),
        ..opts
    })
    .await
}

async fn run_steerable(opts: &CliOptions<'_>, steer: crate::cli::SteerReceiver) -> Result<CliResult> {
    let started = std::time::Instant::now();
    info!(dir = %opts.working_dir.display(), model = opts.model, fork = opts.fork, "Invoking Codex app-server with steering");
    let mut server = crate::codex::app_server::AppServer::spawn(opts).await?;
    let outcome = server.work(opts, started, steer).await;
    let tail = server.stderr_tail();
    let uid = crate::cli::JobUid::current();
    tokio::spawn(async move {
        server.shutdown().await;
        drop(uid);
    });
    match outcome {
        Ok((result, _)) => Ok(result),
        Err(error) if tail.is_empty() || opts.sensitive || !(opts.environment.is_empty() && opts.provider_keys.is_empty()) => Err(error),
        Err(error) => Err(error.context(tail)),
    }
}

fn work_effort(explicit: Option<&str>, lean: bool) -> Option<String> {
    if let Some(effort) = explicit {
        return Some(effort.to_string());
    }
    if lean {
        return None;
    }
    match std::env::var("WORLD_CODEX_EFFORT") {
        Ok(value) if value.trim().is_empty() || value == "default" => None,
        Ok(value) => Some(value),
        Err(_) => Some("high".to_string()),
    }
}

const LEAN_OFF_FEATURES: &[&str] = &[
    "multi_agent",
    "image_generation",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "apps",
    "plugins",
    "goals",
    "view_image",
    "skill_search",
    "tool_suggest",
    "sleep_tool",
    "in_app_browser",
    "workspace_dependencies",
    "worktrees",
    "hooks",
    "mentions_v2",
    "realtime_conversation",
];

const SHELL_FEATURES: &[&str] = &["shell_tool", "unified_exec", "apply_patch_freeform"];

pub(crate) fn lean_overrides(opts: &CliOptions<'_>) -> Vec<String> {
    lean_overrides_for(opts, sandbox::mode())
}

fn lean_overrides_for(opts: &CliOptions<'_>, mode: sandbox::Mode) -> Vec<String> {
    if !opts.lean {
        return Vec::new();
    }
    let shell_safe = !opts.readable_files.is_empty() && mode != sandbox::Mode::Off;
    let shell: &[&str] = if shell_safe { &[] } else { SHELL_FEATURES };
    LEAN_OFF_FEATURES.iter().chain(shell).map(|feature| format!("features.{feature}=false")).collect()
}

pub(crate) async fn writable_roots(dir: &Path) -> Vec<PathBuf> {
    let env_dir = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let mut roots: Vec<PathBuf> = crate::cli::git_common_dir(dir).await.into_iter().collect();
    roots.extend(env_dir("WORLD_RUNNER_CODEX_STATE_DIR"));
    if crate::cli::JobUid::current().is_none() {
        roots.extend(owner_cache_roots(env_dir("HOME").as_deref()));
    }
    let homes: Vec<PathBuf> = env_dir("CODEX_HOME").into_iter().collect();
    roots.retain(|root| homes.iter().all(|home| !home.starts_with(root) && !root.starts_with(home)));
    roots
}

fn owner_cache_roots(home: Option<&Path>) -> Vec<PathBuf> {
    home.map(crate::claude::sandbox::writable_in).unwrap_or_default().into_iter().filter(|root| root.is_dir()).collect()
}

pub(crate) fn shell_environment_overrides(granted: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let (include, exclude) = crate::cli::shell_environment_patterns(granted);
    Ok(vec![
        "shell_environment_policy.inherit=\"all\"".to_string(),
        "shell_environment_policy.ignore_default_excludes=true".to_string(),
        format!("shell_environment_policy.exclude={}", serde_json::to_string(&exclude)?),
        format!("shell_environment_policy.include_only={}", serde_json::to_string(&include)?),
    ])
}

async fn run_exec(opts: CliOptions<'_>) -> Result<CliResult> {
    let mut cmd = job_command("codex");
    cmd.current_dir(opts.working_dir);
    cmd.kill_on_drop(true);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    let chatgpt_login = exec_uses_chatgpt_login(&cmd);
    let shielded = crate::key_proxy::shield_codex(&mut cmd, chatgpt_login);

    let resuming = opts.session_id.is_some();
    cmd.arg("exec");
    for value in shielded.openai_base().map(crate::key_proxy::codex_overrides).unwrap_or_default() {
        cmd.arg("-c").arg(value);
    }
    for value in shell_environment_overrides(&opts.environment)?.into_iter().chain(shielded.codex_shell_overrides()) {
        cmd.arg("-c").arg(value);
    }
    for value in lean_overrides(&opts) {
        cmd.arg("-c").arg(value);
    }
    cmd.arg("--skip-git-repo-check");
    match app_server::sandbox_of(&opts) {
        "read-only" => {
            cmd.arg("--sandbox").arg("read-only");
        }
        "workspace-write" => {
            cmd.arg("--approve-for-me");
            cmd.arg("-c").arg("sandbox_workspace_write.network_access=true");
            let roots = writable_roots(opts.working_dir).await;
            if !roots.is_empty() {
                cmd.arg("-c").arg(format!("sandbox_workspace_write.writable_roots={}", serde_json::to_string(&roots)?));
            }
        }
        _ => {
            cmd.arg("--dangerously-bypass-approvals-and-sandbox");
        }
    }
    cmd.arg("--json");
    cmd.arg("--model").arg(opts.model);
    if let Some(effort) = work_effort(opts.effort, opts.lean) {
        cmd.arg("-c").arg(format!("model_reasoning_effort=\"{effort}\""));
    }
    if opts.fast {
        cmd.arg("-c").arg("service_tier=\"fast\"");
    }
    mcp_args(&mut cmd, &opts)?;
    let images: Vec<String> = opts.image_paths.iter().map(|p| format!("--image={}", p.display())).collect();
    if let Some(sid) = opts.session_id {
        cmd.arg(if opts.fork { "fork" } else { "resume" }).args(&images).arg(sid).arg("-");
    } else {
        cmd.args(&images).arg("-");
    }

    let full_prompt = full_prompt(&opts);

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(
        dir = %opts.working_dir.display(),
        model = opts.model,
        resuming,
        "Invoking Codex CLI"
    );

    let state = own_cli_state().await;
    let mut child = cmd.spawn().context(
        "Failed to spawn codex CLI — is it installed? Run: npm install -g @openai/codex",
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
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), lines.get_mut().fill_buf()).await;
    drop(state);

    let mut last_agent_message = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_cached_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut session_id: Option<String> = None;
    let mut completed = false;

    while let Some(line) = lines.next_line().await? {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        let event_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if session_id.is_none() {
            if let Some(sid) = session_id_from_init("codex", &json) {
                session_id = Some(sid.to_string());
                if let Some(tx) = &opts.event_tx {
                    let _ = tx.send(StreamEvent::SessionStarted {
                        session_id: sid.to_string(),
                    });
                }
            }
        }

        match event_type {
            "item.started" | "item.completed" => {
                if let Some(item) = json.get("item") {
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    if let Some(tx) = &opts.event_tx {
                        match item_type {
                            "command_execution" if event_type == "item.started" => {
                                let command =
                                    item.get("command").and_then(|v| v.as_str()).unwrap_or("");
                                let summary = loka_shared::text::clip_with_ellipsis(command, 120);
                                let _ = tx.send(StreamEvent::ToolUse {
                                    tool: "Bash".to_string(),
                                    input_summary: summary,
                                });
                            }
                            "command_execution" if event_type == "item.completed" => {
                                let output = extract_codex_item_output(item);
                                if !output.is_empty() {
                                    let _ = tx.send(StreamEvent::ToolResult {
                                        tool: "Bash".to_string(),
                                        output,
                                    });
                                }
                            }
                            "file_change" if event_type == "item.completed" => {
                                if let Some(changes) =
                                    item.get("changes").and_then(|v| v.as_array())
                                {
                                    for change in changes {
                                        let path = change
                                            .get("path")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown");
                                        let kind = change
                                            .get("kind")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("update");
                                        let tool = match kind {
                                            "add" => "Write",
                                            "delete" => "Delete",
                                            _ => "Edit",
                                        };
                                        let _ = tx.send(StreamEvent::ToolUse {
                                            tool: tool.to_string(),
                                            input_summary: shorten_path(path),
                                        });
                                    }
                                }
                            }
                            "mcp_tool_call" if event_type == "item.started" => {
                                let server =
                                    item.get("server").and_then(|v| v.as_str()).unwrap_or("mcp");
                                let tool = item
                                    .get("tool")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let _ = tx.send(StreamEvent::ToolUse {
                                    tool: format!("{server}/{tool}"),
                                    input_summary: String::new(),
                                });
                            }
                            "mcp_tool_call" if event_type == "item.completed" => {
                                let output = extract_codex_item_output(item);
                                if !output.is_empty() {
                                    let server = item
                                        .get("server")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("mcp");
                                    let tool = item
                                        .get("tool")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let _ = tx.send(StreamEvent::ToolResult {
                                        tool: format!("{server}/{tool}"),
                                        output,
                                    });
                                }
                            }
                            "agent_message" if event_type == "item.completed" => {
                                let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                if !text.trim().is_empty() {
                                    last_agent_message = text.to_string();
                                    let truncated: String = text.trim().chars().take(300).collect();
                                    let _ = tx.send(StreamEvent::AssistantText(truncated));
                                }
                            }
                            _ => {}
                        }
                    }

                    if item_type == "agent_message" && event_type == "item.completed" {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            if !text.trim().is_empty() {
                                last_agent_message = text.to_string();
                            }
                        }
                    }
                }
            }
            "turn.completed" => {
                if let Some(usage) = json.get("usage") {
                    total_input_tokens += usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    total_cached_tokens += usage
                        .get("cached_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    total_output_tokens += usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                }
                completed = true;
                break;
            }
            "turn.failed" => {
                let error_msg = json
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                if let Some(tx) = &opts.event_tx {
                    let _ = tx.send(StreamEvent::Error(error_msg.to_string()));
                }
            }
            _ => {}
        }
    }

    let context_tokens = match session_id.clone() {
        Some(thread) if completed => tokio::task::spawn_blocking(move || rollout_context_tokens(&thread)).await.ok().flatten().unwrap_or(0),
        _ => 0,
    };
    let usage = crate::cli::Usage {
        input_tokens: total_input_tokens.saturating_sub(total_cached_tokens),
        cached_input_tokens: total_cached_tokens,
        output_tokens: total_output_tokens,
        context_tokens,
    };

    if completed && !last_agent_message.is_empty() {
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
            input_tokens = total_input_tokens,
            output_tokens = total_output_tokens,
            "Codex CLI completed"
        );
        return Ok(CliResult {
            output: last_agent_message,
            session_id,
            cost_usd: 0.0,
            exit_code: 0,
            usage,
            checkpoint: None,
        });
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = collect_stderr(stderr_handle).await;

    debug!(exit_code, stderr_len = stderr.len(), "Codex CLI finished");

    if !stderr.is_empty()
        && (opts.environment.is_empty() && opts.provider_keys.is_empty())
        && !opts.mcp_has_secrets
        && !opts.sensitive
    {
        debug!(stderr = %stderr, "Codex stderr");
    }

    if exit_code != 0 && last_agent_message.is_empty() {
        anyhow::bail!("Codex CLI exited with code {exit_code}: {stderr}");
    }

    info!(
        exit_code,
        input_tokens = total_input_tokens,
        output_tokens = total_output_tokens,
        "Codex CLI completed"
    );

    Ok(CliResult {
        output: last_agent_message,
        session_id,
        cost_usd: 0.0,
        exit_code,
        usage,
        checkpoint: None,
    })
}

fn full_prompt(opts: &CliOptions<'_>) -> String {
    if opts.session_id.is_some() {
        return opts.prompt.to_string();
    }
    let mut parts = if opts.lean {
        Vec::new()
    } else {
        vec![AGENT_PREAMBLE.to_string()]
    };
    if let Some(sys) = opts.system_prompt {
        parts.push(sys.to_string());
    }
    parts.push(opts.prompt.to_string());
    parts.join("\n\n")
}

#[derive(Debug, Deserialize, Serialize)]
struct McpLaunch {
    command: String,
    args: Vec<String>,
    environment: BTreeMap<String, String>,
}

pub fn launch_mcp(encoded: &str) -> Result<()> {
    let spec: McpLaunch = serde_json::from_str(encoded).context("invalid MCP launch specification")?;
    let mut cmd = std::process::Command::new(&spec.command);
    cmd.args(&spec.args);
    cmd.env_remove("WORLD_SECRETS_KEY");
    for name in std::env::vars_os().map(|(name, _)| name) {
        if name.to_string_lossy().starts_with("LOKA_MCP_ENV_")
            || name.to_string_lossy().starts_with("LOKA_MCP_HEADER_")
        {
            cmd.env_remove(name);
        }
    }
    for (name, alias) in spec.environment {
        anyhow::ensure!(
            valid_environment_name(&name) && name != "WORLD_SECRETS_KEY",
            "invalid MCP environment key {name}"
        );
        let value = std::env::var(&alias)
            .with_context(|| format!("missing MCP environment value for {name}"))?;
        cmd.env_remove(&alias);
        cmd.env(name, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = cmd.exec();
        Err(error).with_context(|| format!("failed to start MCP server {}", spec.command))
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .with_context(|| format!("failed to start MCP server {}", spec.command))?;
        anyhow::ensure!(status.success(), "MCP server exited with {status}");
        Ok(())
    }
}

type McpOverrides = (Vec<String>, Vec<(String, String)>);

fn codex_home() -> Option<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".codex")))
}

pub(crate) fn config_server_names(text: &str) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        let path = if let Some(header) = line.strip_prefix('[').filter(|rest| !rest.starts_with('[')) {
            header.split(']').next().unwrap_or_default()
        } else {
            line.split('=').next().unwrap_or_default()
        };
        let Some(rest) = path.trim().strip_prefix("mcp_servers.") else {
            continue;
        };
        let name = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => rest.split('.').next().unwrap_or_default().trim(),
        };
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
    names
}

pub(crate) fn host_mcp_names(working_dir: &std::path::Path) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let files = codex_home().map(|home| home.join("config.toml")).into_iter().chain([working_dir.join(".codex").join("config.toml")]);
    for file in files {
        if let Ok(text) = std::fs::read_to_string(file) {
            names.extend(config_server_names(&text));
        }
    }
    names
}

pub(crate) fn host_isolation_overrides(host: &std::collections::BTreeSet<String>) -> Vec<String> {
    let mut out = vec!["features.plugins=false".to_string(), "features.apps=false".to_string()];
    out.extend(
        host.iter()
            .filter(|name| !name.contains(['.', '"', '\\', ' ']))
            .map(|name| format!("mcp_servers.{name}.enabled=false")),
    );
    out
}

pub(crate) fn mcp_args(cmd: &mut tokio::process::Command, opts: &CliOptions<'_>) -> Result<()> {
    let host = host_mcp_names(opts.working_dir);
    for value in host_isolation_overrides(&host) {
        cmd.arg("-c").arg(value);
    }
    if let Some(mcp) = opts.mcp_config_json.as_deref() {
        cmd.arg("-c").arg("mcp_optional_startup_grace_ms=0");
        let (overrides, mcp_env) = mcp_overrides(mcp, &host)?;
        for value in overrides {
            cmd.arg("-c").arg(value);
        }
        cmd.envs(mcp_env);
    }
    Ok(())
}

fn mcp_overrides(mcp_json: &str, host: &std::collections::BTreeSet<String>) -> Result<McpOverrides> {
    let v = serde_json::from_str::<serde_json::Value>(mcp_json)?;
    let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut out = Vec::new();
    let mut process_env = Vec::new();
    for (name, server) in servers {
        let prefix = if host.contains(name) { format!("mcp_servers.{name}_world") } else { format!("mcp_servers.{name}") };
        if let Some(url) = server.get("url") {
            out.push(format!("{prefix}.url={url}"));
        }
        let environment = server.get("env").and_then(|e| e.as_object());
        if let Some(command) = server.get("command") {
            if environment.is_some_and(|env| !env.is_empty()) {
                let command = command
                    .as_str()
                    .ok_or_else(|| anyhow!("MCP server {name} command must be a string"))?;
                let args = server
                    .get("args")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .map(|value| {
                        value
                            .as_str()
                            .map(String::from)
                            .ok_or_else(|| anyhow!("MCP server {name} args must be strings"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut aliases = BTreeMap::new();
                for (key, value) in environment.into_iter().flatten() {
                    anyhow::ensure!(
                        valid_environment_name(key) && key != "WORLD_SECRETS_KEY",
                        "invalid MCP environment key {key}"
                    );
                    let value = value
                        .as_str()
                        .ok_or_else(|| anyhow!("MCP server {name} environment values must be strings"))?;
                    let alias = format!("LOKA_MCP_ENV_{}", process_env.len());
                    process_env.push((alias.clone(), value.to_string()));
                    aliases.insert(key.clone(), alias);
                }
                let spec = serde_json::to_string(&McpLaunch {
                    command: command.to_string(),
                    args,
                    environment: aliases.clone(),
                })?;
                let executable = std::env::current_exe().context("could not locate loka-agent")?;
                out.push(format!("{prefix}.command={}", serde_json::to_string(&executable)?));
                out.push(format!(
                    "{prefix}.args={}",
                    serde_json::to_string(&vec!["mcp-launch", spec.as_str()])?
                ));
                out.push(format!(
                    "{prefix}.env_vars={}",
                    serde_json::to_string(&aliases.into_values().collect::<Vec<_>>())?
                ));
            } else {
                out.push(format!("{prefix}.command={command}"));
                if let Some(args) = server.get("args") {
                    out.push(format!("{prefix}.args={args}"));
                }
            }
        }
        if let Some(headers) = server.get("headers").and_then(|h| h.as_object()) {
            let pairs: Vec<String> = headers
                .iter()
                .filter_map(|(key, value)| {
                    let value = value.as_str()?;
                    let env_name = format!("LOKA_MCP_HEADER_{}", process_env.len());
                    process_env.push((env_name.clone(), value.to_string()));
                    Some(format!(
                        "{}={}",
                        serde_json::to_string(key).unwrap(),
                        serde_json::to_string(&env_name).unwrap()
                    ))
                })
                .collect();
            if !pairs.is_empty() {
                out.push(format!(
                    "{prefix}.env_http_headers={{{}}}",
                    pairs.join(", ")
                ));
            }
        }
        if let Some(mode) = server.get("default_tools_approval_mode") {
            let mode = mode.as_str().ok_or_else(|| {
                anyhow!("MCP server {name} default tools approval mode must be a string")
            })?;
            anyhow::ensure!(
                matches!(mode, "auto" | "prompt" | "writes" | "approve"),
                "invalid MCP server {name} default tools approval mode"
            );
            out.push(format!(
                "{prefix}.default_tools_approval_mode={}",
                serde_json::to_string(mode)?
            ));
        } else if name == "world" {
            out.push(format!("{prefix}.default_tools_approval_mode=\"approve\""));
        }
    }
    Ok((out, process_env))
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(3).collect();
    if parts.len() < 3 {
        return path.to_string();
    }
    let shortened: Vec<&str> = parts.into_iter().rev().collect();
    format!("…/{}", shortened.join("/"))
}

fn extract_codex_item_output(item: &serde_json::Value) -> String {
    let candidates = [
        "output",
        "stdout",
        "stderr",
        "text",
        "result",
        "combined_output",
    ];
    let mut chunks = Vec::new();
    for key in candidates {
        if let Some(value) = item.get(key) {
            let text = summarize_tool_output(value);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
        }
    }
    if chunks.is_empty() {
        if let Some(exit_code) = item.get("exit_code").and_then(|v| v.as_i64()) {
            if exit_code != 0 {
                chunks.push(format!("command failed with exit code {exit_code}"));
            }
        }
    }
    truncate_for_log(&chunks.join("\n"), TOOL_OUTPUT_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::{chatgpt_login, exec_uses_chatgpt_login, mcp_overrides, owner_cache_roots, uses_chatgpt_login, work_effort, McpLaunch};

    #[test]
    fn owner_codex_runs_can_write_existing_package_caches() {
        let home = std::env::temp_dir().join(format!("loka-codex-caches-{}", uuid::Uuid::new_v4().simple()));
        for dir in [".cargo/registry", ".npm", ".ssh"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        let roots = owner_cache_roots(Some(&home));
        let _ = std::fs::remove_dir_all(&home);
        assert!(roots.contains(&home.join(".cargo/registry")));
        assert!(roots.contains(&home.join(".npm")));
        assert!(!roots.contains(&home.join(".cargo/git")));
        assert!(roots.iter().all(|root| !root.starts_with(home.join(".ssh"))));
        assert!(owner_cache_roots(None).is_empty());
    }

    #[test]
    fn reads_the_login_mode_from_auth_json() {
        let cases = [
            (r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"x"}}"#, true),
            (r#"{"auth_mode":"chatgptAuthTokens","tokens":{"id_token":"x"}}"#, true),
            (r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-key"}"#, false),
            (r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-key","tokens":{"id_token":"x"}}"#, false),
            (r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"x"}}"#, true),
            (r#"{"OPENAI_API_KEY":"","tokens":{"id_token":"x"}}"#, true),
            (r#"{"OPENAI_API_KEY":"sk-key","tokens":{"id_token":"x"}}"#, false),
            (r#"{"OPENAI_API_KEY":null,"tokens":null}"#, false),
            ("{}", false),
            ("not json", false),
        ];
        for (auth, expected) in cases {
            assert_eq!(chatgpt_login(auth), expected, "{auth}");
        }
    }

    #[test]
    fn api_keys_win_over_a_chatgpt_auth_file_only_where_codex_reads_them() {
        let dir = std::env::temp_dir().join(format!("loka-codex-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), r#"{"auth_mode":"chatgpt","tokens":{"id_token":"x"}}"#).unwrap();
        let command = |codex_key: Option<&str>| {
            let mut cmd = tokio::process::Command::new("true");
            cmd.env_clear().env("CODEX_HOME", &dir).env("OPENAI_API_KEY", "sk-shell-key");
            if let Some(key) = codex_key {
                cmd.env("CODEX_API_KEY", key);
            }
            cmd
        };
        assert!(uses_chatgpt_login(&command(None), false));
        assert!(!uses_chatgpt_login(&command(None), true));
        assert!(uses_chatgpt_login(&command(Some("sk-codex-key")), false));
        assert!(exec_uses_chatgpt_login(&command(None)));
        assert!(exec_uses_chatgpt_login(&command(Some(""))));
        assert!(!exec_uses_chatgpt_login(&command(Some("sk-codex-key"))));
        std::fs::write(dir.join("auth.json"), r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-key"}"#).unwrap();
        assert!(!uses_chatgpt_login(&command(None), false));
        assert!(!exec_uses_chatgpt_login(&command(None)));
        std::fs::remove_file(dir.join("auth.json")).unwrap();
        assert!(!uses_chatgpt_login(&command(None), false));
        for store in ["keyring", "auto"] {
            std::fs::write(dir.join("config.toml"), format!("model = \"gpt-5\"\ncli_auth_credentials_store = \"{store}\"\n\n[mcp_servers.x]\ncommand = \"x\"\n")).unwrap();
            assert!(uses_chatgpt_login(&command(None), false), "{store}");
            assert!(exec_uses_chatgpt_login(&command(None)), "{store}");
            assert!(!uses_chatgpt_login(&command(None), true), "{store}");
            assert!(!exec_uses_chatgpt_login(&command(Some("sk-codex-key"))), "{store}");
        }
        for config in ["cli_auth_credentials_store = \"file\"\n", "# cli_auth_credentials_store = \"keyring\"\n", "[profiles.x]\ncli_auth_credentials_store = \"keyring\"\n"] {
            std::fs::write(dir.join("config.toml"), config).unwrap();
            assert!(!uses_chatgpt_login(&command(None), false), "{config}");
        }
        std::fs::write(dir.join("config.toml"), "cli_auth_credentials_store = \"keyring\"\n").unwrap();
        std::fs::write(dir.join("auth.json"), r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-key"}"#).unwrap();
        assert!(!uses_chatgpt_login(&command(None), false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn work_runs_default_to_high_effort_and_turns_keep_the_cli_default() {
        assert_eq!(work_effort(Some("low"), false).as_deref(), Some("low"));
        assert_eq!(work_effort(None, true), None);
        if std::env::var("WORLD_CODEX_EFFORT").is_err() {
            assert_eq!(work_effort(None, false).as_deref(), Some("high"));
        }
    }

    #[test]
    fn translates_mcp_json_to_config_overrides() {
        let json = r#"{"mcpServers":{"remote":{"url":"https://mcp.example.com/mcp","headers":{"Authorization":"Bearer credential \"quoted\"","X-API-Key":"secret-key"}},"slack":{"command":"/usr/bin/python3","args":["slack_mcp.py"],"default_tools_approval_mode":"approve"},"world":{"command":"/bin/loka","args":["mcp"],"env":{"WORLD_AGENT_ID":"abc"}}}}"#;
        let (overrides, child_env) = mcp_overrides(json, &Default::default()).unwrap();
        assert_eq!(
            overrides
                .iter()
                .filter(|override_value| override_value.contains("default_tools_approval_mode"))
                .count(),
            2
        );
        assert!(overrides
            .iter()
            .any(|value| value == r#"mcp_servers.slack.default_tools_approval_mode="approve""#));
        assert!(overrides
            .iter()
            .any(|value| value == r#"mcp_servers.world.default_tools_approval_mode="approve""#));
        assert!(!overrides
            .iter()
            .any(|value| value.starts_with("mcp_servers.remote.default_tools_approval_mode=")));
        assert!(!overrides.join(" ").contains("credential"));
        assert!(!overrides.join(" ").contains("secret-key"));
        assert!(!overrides.join(" ").contains("\"abc\""));
        assert!(overrides.iter().any(|value| value.contains("mcp-launch")));
        assert!(overrides.iter().any(|value| value == r#"mcp_servers.world.env_vars=["LOKA_MCP_ENV_2"]"#));
        let args = overrides
            .iter()
            .find_map(|value| value.strip_prefix("mcp_servers.world.args="))
            .unwrap();
        let args: Vec<String> = serde_json::from_str(args).unwrap();
        let launch: McpLaunch = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(launch.command, "/bin/loka");
        assert_eq!(launch.args, ["mcp"]);
        assert_eq!(launch.environment["WORLD_AGENT_ID"], "LOKA_MCP_ENV_2");
        assert_eq!(
            child_env,
            vec![
                (
                    "LOKA_MCP_HEADER_0".to_string(),
                    "Bearer credential \"quoted\"".to_string()
                ),
                ("LOKA_MCP_HEADER_1".to_string(), "secret-key".to_string()),
                ("LOKA_MCP_ENV_2".to_string(), "abc".to_string()),
            ]
        );
    }

    #[test]
    fn finds_host_servers_and_switches_them_off() {
        let text = "model = \"x\"\n[mcp_servers.ghar]\nurl = \"https://x\"\n[mcp_servers.ghar.http_headers]\nAuthorization = \"Bearer t\"\n[mcp_servers.\"node.repl\"]\ncommand = \"n\"\nmcp_servers.inline.command = \"y\"\n[[profiles]]\n";
        let names = super::config_server_names(text);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["ghar", "inline", "node.repl"]);
        let off = super::host_isolation_overrides(&super::config_server_names(text));
        assert!(off.contains(&"features.plugins=false".to_string()));
        assert!(off.contains(&"features.apps=false".to_string()));
        assert!(off.contains(&"mcp_servers.ghar.enabled=false".to_string()));
        assert!(!off.iter().any(|value| value.contains("node.repl")));
        let host = std::collections::BTreeSet::from(["github".to_string()]);
        let (overrides, _) = mcp_overrides(r#"{"mcpServers":{"github":{"command":"x"}}}"#, &host).unwrap();
        assert!(overrides.iter().all(|value| value.starts_with("mcp_servers.github_world.")), "{overrides:?}");
    }

    #[test]
    fn keeps_same_mcp_key_values_isolated_per_server() {
        let json = r#"{"mcpServers":{"one":{"command":"one","env":{"TOKEN":"first"}},"two":{"command":"two","env":{"TOKEN":"second"}}}}"#;
        let (overrides, child_env) = mcp_overrides(json, &Default::default()).unwrap();
        assert!(!overrides.join(" ").contains("first"));
        assert!(!overrides.join(" ").contains("second"));
        assert_eq!(child_env[0].1, "first");
        assert_eq!(child_env[1].1, "second");
        let launches = ["one", "two"].map(|name| {
            let prefix = format!("mcp_servers.{name}.args=");
            let args = overrides
                .iter()
                .find_map(|value| value.strip_prefix(&prefix))
                .unwrap();
            let args: Vec<String> = serde_json::from_str(args).unwrap();
            serde_json::from_str::<McpLaunch>(&args[1]).unwrap()
        });
        assert_ne!(launches[0].environment["TOKEN"], launches[1].environment["TOKEN"]);
    }
}

#[cfg(test)]
mod lean_tests {
    use crate::cli::CliOptions;

    fn options(lean: bool, readable: Vec<std::path::PathBuf>) -> CliOptions<'static> {
        CliOptions {
            working_dir: std::path::Path::new("/tmp"),
            prompt: "",
            system_prompt: None,
            allowed_tools: None,
            max_turns: 1,
            model: "m",
            mcp_config_json: None,
            session_id: None,
            resume: false,
            event_tx: None,
            image_paths: Vec::new(),
            fork: false,
            lean,
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
            readable_files: readable,
        }
    }

    #[test]
    fn lean_turns_drop_builtin_tools_but_keep_the_shell_for_skill_files() {
        assert!(super::lean_overrides(&options(false, Vec::new())).is_empty());
        let bare = super::lean_overrides(&options(true, Vec::new()));
        assert!(bare.contains(&"features.multi_agent=false".to_string()));
        assert!(bare.contains(&"features.shell_tool=false".to_string()));
        assert!(!bare.iter().any(|o| o.contains("code_mode_host")), "Codex routes MCP tool calls through the code mode host");
        let skilled = super::lean_overrides(&options(true, vec!["/skills/a/SKILL.md".into()]));
        assert!(skilled.contains(&"features.image_generation=false".to_string()));
        assert!(!skilled.iter().any(|o| o.contains("shell_tool") || o.contains("unified_exec")));
        let unsandboxed = super::lean_overrides_for(&options(true, vec!["/skills/a/SKILL.md".into()]), crate::claude::sandbox::Mode::Off);
        assert!(unsandboxed.contains(&"features.shell_tool=false".to_string()), "no shell when nothing can sandbox it");
        assert!(unsandboxed.contains(&"features.apply_patch_freeform=false".to_string()));
    }

    #[test]
    fn main_turns_stay_read_only_even_with_task_sandboxing_off() {
        use crate::claude::sandbox::Mode;
        use crate::cli::Approval;
        use super::app_server::sandbox_for;
        for approval in [Approval::Auto, Approval::Full, Approval::ReadOnly] {
            for mode in [Mode::Off, Mode::Auto, Mode::Strict] {
                assert_eq!(sandbox_for(true, approval, mode), "read-only");
            }
        }
        assert_eq!(sandbox_for(false, Approval::Auto, Mode::Off), "danger-full-access");
        assert_eq!(sandbox_for(false, Approval::Auto, Mode::Auto), "workspace-write");
        assert_eq!(sandbox_for(false, Approval::ReadOnly, Mode::Off), "read-only");
        assert_eq!(sandbox_for(false, Approval::Full, Mode::Auto), "danger-full-access");
    }
}

#[cfg(test)]
mod live_tests {
    use crate::cli::CliOptions;

    #[tokio::test]
    #[ignore]
    async fn cold_runs_return_without_waiting_for_exit() {
        let model = std::env::var("CODEX_TEST_MODEL").expect("set CODEX_TEST_MODEL");
        let dir = std::env::temp_dir();
        for backend in ["codex", "claude"] {
            let model = if backend == "claude" { "haiku" } else { model.as_str() };
            let started = std::time::Instant::now();
            let result = crate::cli::run(
                backend,
                CliOptions {
                    working_dir: &dir,
                    prompt: "Reply with the single word ok.",
                    system_prompt: None,
                    allowed_tools: None,
                    max_turns: 2,
                    model,
                    mcp_config_json: None,
                    session_id: None,
                    resume: false,
                    event_tx: None,
                    image_paths: Vec::new(),
                    fork: false,
                    lean: true,
                    effort: Some("low"),
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
                },
            )
            .await
            .unwrap();
            assert!(result.output.to_lowercase().contains("ok"), "{}", result.output);
            assert!(result.session_id.is_some());
            eprintln!("{backend} cold run {:?}", started.elapsed());
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_rewind_drops_later_turns() {
        let codex_model = std::env::var("CODEX_TEST_MODEL").expect("set CODEX_TEST_MODEL");
        let dir = std::env::temp_dir();
        for backend in ["claude", "codex"] {
            let model = if backend == "claude" { "haiku" } else { codex_model.as_str() };
            let fresh_sid = (backend == "claude").then(|| uuid::Uuid::new_v4().to_string());
            let turn = |prompt: String, sid: Option<String>, resume: bool, rewind: Option<String>| {
                let dir = dir.clone();
                let model = model.to_string();
                async move {
                    crate::cli::run(
                        backend,
                        CliOptions {
                            working_dir: &dir,
                            prompt: &prompt,
                            system_prompt: None,
                            allowed_tools: None,
                            max_turns: 2,
                            model: &model,
                            mcp_config_json: None,
                            session_id: sid.as_deref(),
                            resume,
                            event_tx: None,
                            image_paths: Vec::new(),
                            fork: false,
                            lean: true,
                            effort: Some("low"),
                            fast: false,
                            environment: Default::default(),
                            mcp_has_secrets: false,
                            sensitive: false,
                            rewind_to: rewind.as_deref(),
                            approval: Default::default(),
                            steer: Default::default(),
                            omp_models: None,
                            provider_keys: Default::default(),
                            readable_files: Vec::new(),
                        },
                    )
                    .await
                    .unwrap()
                }
            };
            let first = turn("Remember the word APPLE. Reply with just ok.".into(), fresh_sid.clone(), false, None).await;
            let sid = first.session_id.clone().unwrap();
            let second_prompt = "Also remember the word BANANA. Reply with just ok.";
            let second = turn(second_prompt.into(), Some(sid.clone()), true, None).await;
            let anchor = if backend == "claude" { first.checkpoint.clone().expect("claude checkpoint") } else { second_prompt.to_string() };
            assert!(second.session_id.is_some());
            let rewound = turn(
                "List every word I asked you to remember, uppercase, comma separated, nothing else.".into(),
                Some(sid.clone()),
                true,
                Some(anchor),
            )
            .await;
            eprintln!("{backend}: {} -> {:?}: {}", sid, rewound.session_id, rewound.output);
            assert!(rewound.output.contains("APPLE"), "{}", rewound.output);
            assert!(!rewound.output.contains("BANANA"), "{}", rewound.output);
            assert_ne!(rewound.session_id.as_deref(), Some(sid.as_str()));
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_codex_compact() {
        let model = std::env::var("CODEX_TEST_MODEL").expect("set CODEX_TEST_MODEL");
        let dir = std::env::temp_dir();
        let options = |prompt: &'static str, sid: Option<String>| CliOptions {
            working_dir: Box::leak(dir.clone().into_boxed_path()),
            prompt,
            system_prompt: None,
            allowed_tools: None,
            max_turns: 2,
            model: Box::leak(model.clone().into_boxed_str()),
            mcp_config_json: None,
            session_id: sid.map(|sid| &*Box::leak(sid.into_boxed_str())),
            resume: true,
            event_tx: None,
            image_paths: Vec::new(),
            fork: false,
            lean: true,
            effort: Some("low"),
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
        };
        let first = crate::cli::run("codex", options("Remember the word heliotrope. Reply with just ok.", None)).await.unwrap();
        let sid = first.session_id.clone().expect("thread id");
        let second = crate::cli::run("codex", options("Also remember the word marmalade. Reply with just ok.", Some(sid.clone()))).await.unwrap();
        eprintln!("exec context before compaction {}", second.usage.context_tokens);
        assert!(second.usage.context_tokens > 0);
        assert!(second.usage.context_tokens < second.usage.input_tokens + second.usage.cached_input_tokens);
        let started = std::time::Instant::now();
        let compacted = crate::cli::run("codex", options(crate::cli::COMPACT_PROMPT, Some(sid.clone()))).await.unwrap();
        eprintln!("compaction took {} ms, usage {:?}", started.elapsed().as_millis(), compacted.usage);
        assert_eq!(compacted.session_id.as_deref(), Some(sid.as_str()));
        let answer = crate::cli::run("codex", options("Which two words did I ask you to remember? Answer with the two words.", Some(sid.clone()))).await.unwrap();
        let reply = answer.output.to_lowercase();
        assert!(reply.contains("heliotrope") && reply.contains("marmalade"), "{}", answer.output);
        eprintln!("after compaction context {}", answer.usage.context_tokens);
        let mut server = crate::codex::app_server::AppServer::spawn(&options("", None)).await.unwrap();
        let (warm, _) = server.ask(&options("Say the two words again.", Some(sid.clone())), std::time::Instant::now()).await.unwrap();
        server.shutdown().await;
        eprintln!("app-server context {}", warm.usage.context_tokens);
        assert!(warm.usage.context_tokens > 0);
    }
}
