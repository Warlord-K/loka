use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::cli::{
    announce_spawn, configure_child_environment, give_to_job_user, job_command, job_user, stderr_tail,
    summarize_tool_output,
    CliOptions, CliResult, StreamEvent, Usage,
};

const AGENT_PREAMBLE: &str = "\
You are Loka, an autonomous coding agent. You work independently without \
human interaction during execution. If you are uncertain, make the best judgment \
call and document your reasoning in a commit message.\n\n\
Follow existing code patterns and conventions in each repository. \
Read AGENTS.md or CLAUDE.md if they exist for project-specific instructions.\n\n\
Git commit rules:\n\
- Use Conventional Commits: type(scope): description\n\
- Types: feat, fix, refactor, test, chore, perf, ci\n\
- NEVER add Co-Authored-By trailers to commits\n\
- NEVER add Signed-off-by trailers to commits";

const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const COPIED_CONFIG: &[&str] = &["models.yml", "models.yaml", "config.yml"];
const MODEL_CONFIG: &[&str] = &["models.yml", "models.yaml"];

pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("WORLD_OMP_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(root) = std::env::var_os("WORLD_RUNNER_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(root).join("omp");
    }
    home().join(".omp/world")
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/agent".to_string()))
}

pub fn config_source() -> PathBuf {
    std::env::var_os("OMP_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".omp/agent"))
}

pub(crate) struct RunDir(PathBuf, Option<crate::sandbox_wrap::RunTemp>);

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .with_context(|| format!("Failed to create {}", path.display()))?;
    file.write_all(contents).await?;
    file.flush().await?;
    give_to_job_user(path)?;
    Ok(())
}

async fn read_config(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    if job_user().is_some() {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut contents = Vec::new();
    options.open(path).await?.read_to_end(&mut contents).await?;
    Ok(contents)
}

impl RunDir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

async fn prepare_run_dir(opts: &CliOptions<'_>) -> Result<RunDir> {
    let dir = config_dir(opts.omp_models.as_deref()).await?;
    let mcp = run_mcp_json(opts.mcp_config_json.as_deref(), &foreign_mcp_names(opts.working_dir))?;
    if let Some(mcp) = mcp {
        write_private(&dir.0.join("mcp.json"), mcp.as_bytes()).await?;
    }
    give_to_job_user(&dir.0)?;
    Ok(dir)
}

pub(crate) async fn config_dir(custom: Option<&str>) -> Result<RunDir> {
    let dir = RunDir(crate::cli::temp_dir().join(format!("loka-omp-{}", uuid::Uuid::new_v4())), None);
    let mut builder = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        builder.mode(0o700);
    }
    builder
        .create(&dir.0)
        .await
        .with_context(|| format!("Failed to create omp run dir {}", dir.0.display()))?;
    let source = config_source();
    for name in COPIED_CONFIG {
        if custom.is_some() && MODEL_CONFIG.contains(name) {
            continue;
        }
        if let Ok(contents) = read_config(&source.join(name)).await {
            write_private(&dir.0.join(name), &contents).await?;
        }
    }
    if let Some(custom) = custom {
        let mut base = None;
        for name in MODEL_CONFIG {
            if let Ok(contents) = read_config(&source.join(name)).await {
                base = Some(String::from_utf8_lossy(&contents).into_owned());
                break;
            }
        }
        let merged = loka_shared::omp_config::merge(base.as_deref(), custom)
            .context("Failed to merge the agent's omp model config")?;
        write_private(&dir.0.join("models.yml"), merged.as_bytes()).await?;
    }
    Ok(dir)
}

fn json_server_names(path: &Path, pointers: &[&str]) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let text: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for pointer in pointers {
        names.extend(value.pointer(pointer).and_then(Value::as_object).into_iter().flat_map(|servers| servers.keys().cloned()));
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_object) {
        for project in projects.values() {
            names.extend(project.get("mcpServers").and_then(Value::as_object).into_iter().flat_map(|servers| servers.keys().cloned()));
        }
    }
    names
}

pub(crate) fn foreign_mcp_names(cwd: &Path) -> std::collections::BTreeSet<String> {
    let home = home();
    let mut names = std::collections::BTreeSet::new();
    let servers = ["/mcpServers"];
    let json_files = [
        home.join(".claude.json"),
        home.join(".claude/settings.json"),
        home.join(".cursor/mcp.json"),
        home.join(".gemini/settings.json"),
        home.join(".codeium/windsurf/mcp_config.json"),
        home.join(".omp/agent/mcp.json"),
        cwd.join(".mcp.json"),
        cwd.join("mcp.json"),
        cwd.join(".cursor/mcp.json"),
        cwd.join(".omp/mcp.json"),
        cwd.join(".gemini/settings.json"),
        cwd.join(".claude/settings.json"),
    ];
    for file in json_files {
        names.extend(json_server_names(&file, &servers));
    }
    let vscode = [
        home.join("Library/Application Support/Code/User/mcp.json"),
        home.join(".config/Code/User/mcp.json"),
        home.join("Library/Application Support/Code/User/settings.json"),
        home.join(".config/Code/User/settings.json"),
        cwd.join(".vscode/mcp.json"),
    ];
    for file in vscode {
        names.extend(json_server_names(&file, &["/servers", "/mcp/servers"]));
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    for file in ["opencode.json", "opencode.jsonc", "config.json"] {
        names.extend(json_server_names(&xdg.join("opencode").join(file), &["/mcp"]));
    }
    names.extend(crate::codex::host_mcp_names(cwd));
    names
}

fn run_mcp_json(equipped: Option<&str>, foreign: &std::collections::BTreeSet<String>) -> Result<Option<String>> {
    let mut config = match equipped {
        Some(text) => serde_json::from_str::<Value>(text).context("Invalid MCP configuration")?,
        None if foreign.is_empty() => return Ok(None),
        None => serde_json::json!({ "mcpServers": {} }),
    };
    let ours: std::collections::BTreeSet<String> =
        config.get("mcpServers").and_then(Value::as_object).map(|servers| servers.keys().cloned().collect()).unwrap_or_default();
    let disabled: Vec<&String> = foreign.iter().filter(|name| !ours.contains(*name)).collect();
    if !disabled.is_empty() {
        config["disabledServers"] = serde_json::json!(disabled);
    }
    Ok(Some(config.to_string()))
}

async fn shield_models(cmd: &mut Command, run_dir: &Path, model: &str) -> Result<()> {
    let Some(proxy) = crate::key_proxy::get() else {
        return Ok(());
    };
    let Some((provider, _)) = model.split_once('/') else {
        return Ok(());
    };
    let path = run_dir.join("models.yml");
    let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let mut config: serde_yaml::Value = if text.trim().is_empty() {
        serde_yaml::Value::Mapping(Default::default())
    } else {
        serde_yaml::from_str(&text).context("Failed to read the omp model config")?
    };
    if !shield_provider(proxy, cmd, &mut config, provider) {
        return Ok(());
    }
    let _ = tokio::fs::remove_file(&path).await;
    write_private(&path, serde_yaml::to_string(&config)?.as_bytes()).await?;
    give_to_job_user(&path)?;
    Ok(())
}

fn env_names_in(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        let tail = rest[at + 1..].trim_start_matches('{');
        let name: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            names.push(name);
        }
        rest = &rest[at + 1..];
    }
    names
}

fn env_name_like(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && text.contains('_')
}

fn shield_secrets(proxy: &crate::key_proxy::KeyProxy, cmd: &mut Command, block: &mut serde_yaml::Mapping, origins: &[String]) {
    if let Some(key) = block.get_mut("apiKey") {
        if let Some(text) = key.as_str().map(str::to_string) {
            if let Some(command) = text.strip_prefix('!') {
                for name in env_names_in(command) {
                    proxy.shield_var(cmd, &name, origins);
                }
            } else if env_name_like(&text) {
                proxy.shield_var(cmd, &text, origins);
            } else if text.len() >= 4 {
                *key = serde_yaml::Value::String(proxy.place("apiKey", "", &text, origins));
            }
        }
    }
    if let Some(headers) = block.get_mut("headers").and_then(serde_yaml::Value::as_mapping_mut) {
        for (_, value) in headers.iter_mut() {
            let Some(text) = value.as_str().map(str::to_string) else { continue };
            if let Some(command) = text.strip_prefix('!') {
                for name in env_names_in(command) {
                    proxy.shield_var(cmd, &name, origins);
                }
            } else if text.len() >= 8 {
                *value = serde_yaml::Value::String(proxy.place("header", "", &text, origins));
            }
        }
    }
}

fn shield_provider(proxy: &crate::key_proxy::KeyProxy, cmd: &mut Command, config: &mut serde_yaml::Value, provider: &str) -> bool {
    use serde_yaml::{Mapping, Value};
    let Some(root) = config.as_mapping_mut() else {
        return false;
    };
    let providers = root.entry(Value::from("providers")).or_insert_with(|| Value::Mapping(Mapping::new()));
    let Some(providers) = providers.as_mapping_mut() else {
        return false;
    };
    let block = providers.entry(Value::from(provider)).or_insert_with(|| Value::Mapping(Mapping::new()));
    let Some(block) = block.as_mapping_mut() else {
        return false;
    };
    let mut urls: Vec<String> = block.get("baseUrl").and_then(Value::as_str).map(String::from).into_iter().collect();
    if let Some(models) = block.get("models").and_then(Value::as_sequence) {
        urls.extend(models.iter().filter_map(|model| model.get("baseUrl").and_then(Value::as_str)).map(String::from));
    }
    if urls.is_empty() {
        let Some(spec) = crate::key_proxy::spec(provider) else {
            return false;
        };
        let shielded = proxy.shield(cmd, &[spec.id]);
        let Some(base) = shielded.bases.get(spec.id) else {
            return false;
        };
        let origins: Vec<String> = crate::key_proxy::origin(base).into_iter().collect();
        let upstream: Vec<String> = proxy.upstream_origin(base).into_iter().collect();
        shield_secrets(proxy, cmd, block, if upstream.is_empty() { &origins } else { &upstream });
        let base = if spec.id == "google" { format!("{base}/v1beta") } else { base.clone() };
        block.insert(Value::from("baseUrl"), Value::String(base));
        return true;
    }
    let origins: Vec<String> = urls.iter().filter_map(|url| crate::key_proxy::origin(url)).collect();
    if origins.is_empty() {
        return false;
    }
    shield_secrets(proxy, cmd, block, &origins);
    if let Some(url) = block.get_mut("baseUrl") {
        if let Some(text) = url.as_str().filter(|text| crate::key_proxy::origin(text).is_some()).map(str::to_string) {
            *url = Value::String(proxy.route(&text));
        }
    }
    if let Some(models) = block.get_mut("models").and_then(Value::as_sequence_mut) {
        for model in models.iter_mut() {
            if let Some(url) = model.get_mut("baseUrl") {
                if let Some(text) = url.as_str().filter(|text| crate::key_proxy::origin(text).is_some()).map(str::to_string) {
                    *url = Value::String(proxy.route(&text));
                }
            }
        }
    }
    true
}

fn resolve_model(model: &str, models_yml: Option<&str>) -> String {
    if model.contains('/') {
        return model.to_string();
    }
    let Some(config) = models_yml.and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(text).ok()) else {
        return model.to_string();
    };
    let Some(providers) = config.get("providers").and_then(serde_yaml::Value::as_mapping) else {
        return model.to_string();
    };
    let candidates: Vec<(String, String)> = providers
        .iter()
        .filter_map(|(name, provider)| Some((name.as_str()?, provider.get("models")?.as_sequence()?)))
        .flat_map(|(name, models)| {
            models
                .iter()
                .filter_map(|m| m.get("id").and_then(serde_yaml::Value::as_str))
                .map(move |id| (name.to_string(), id.to_string()))
        })
        .collect();
    let wanted = model.to_ascii_lowercase();
    candidates
        .iter()
        .find(|(_, id)| id.to_ascii_lowercase() == wanted)
        .or_else(|| candidates.iter().find(|(_, id)| id.to_ascii_lowercase().starts_with(&wanted)))
        .map(|(provider, id)| format!("{provider}/{id}"))
        .unwrap_or_else(|| model.to_string())
}

fn omp_tools(allowed: &str) -> String {
    allowed
        .split(',')
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(|tool| match tool {
            "Read" => "read",
            "Glob" => "glob",
            "Grep" => "grep",
            "Bash" => "bash",
            "Edit" | "MultiEdit" => "edit",
            "Write" => "write",
            "WebSearch" => "web_search",
            other => other,
        })
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(",")
}

fn summarize_input(tool: &str, args: &Value) -> String {
    let field = |name: &str| args.get(name).and_then(Value::as_str);
    let text = match tool {
        "bash" => field("command"),
        "read" | "write" | "edit" | "ast_edit" => field("path").or_else(|| field("file_path")),
        "grep" | "glob" | "find" | "ast_grep" => field("pattern").or_else(|| field("query")),
        "task" => field("description"),
        _ => None,
    };
    text.unwrap_or_default().to_string()
}

fn message_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[derive(Default)]
struct Totals {
    usage: Usage,
    cost_usd: f64,
}

impl Totals {
    fn add(&mut self, usage: &Value) {
        let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        self.usage.input_tokens += n("input") + n("cacheWrite");
        self.usage.cached_input_tokens += n("cacheRead");
        self.usage.output_tokens += n("output");
        self.usage.context_tokens = n("input") + n("cacheRead") + n("cacheWrite") + n("output");
        self.cost_usd += usage.pointer("/cost/total").and_then(Value::as_f64).unwrap_or(0.0);
    }
}

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    if crate::cli::compaction_run("omp", &opts) {
        let sid = opts.session_id.unwrap_or_default();
        return compact(&opts, sid).await;
    }
    if !opts.lean && opts.rewind_to.is_none() && opts.image_paths.is_empty() {
        if let Some(steer) = opts.steer.take() {
            return run_steerable(&opts, steer).await;
        }
    }
    let (Some(head), Some(sid), true) = (opts.rewind_to, opts.session_id, opts.resume) else {
        return run_print(opts).await;
    };
    let rewound = rewind(&opts, sid, head).await?;
    info!(from = sid, to = %rewound, "Rewound omp session");
    run_print(CliOptions {
        session_id: Some(&rewound),
        resume: true,
        fork: false,
        rewind_to: None,
        approval: Default::default(),
        ..opts
    })
    .await
}

struct RpcProbe {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    _run_dir: RunDir,
}

impl RpcProbe {
    async fn open(opts: &CliOptions<'_>, sid: &str) -> Result<Self> {
        let sessions = state_dir().join("sessions");
        let run_dir = prepare_run_dir(opts).await?;
        let mut cmd = job_command("omp");
        cmd.kill_on_drop(true);
        cmd.current_dir(opts.working_dir);
        configure_child_environment(&mut cmd, &opts.child_environment())?;
        opts.sign_commits(&mut cmd);
        cmd.env_remove("OMP_PROFILE").env_remove("PI_PROFILE");
        cmd.env("PI_CODING_AGENT_DIR", &run_dir.0);
        cmd.args(["--mode", "rpc", "--no-title", "--no-tools", "--no-skills", "--no-rules", "--no-extensions", "--no-lsp"]);
        cmd.arg("--session-dir").arg(&sessions);
        if !opts.model.is_empty() {
            let models_yml = tokio::fs::read_to_string(run_dir.0.join("models.yml")).await.ok();
            let model = resolve_model(opts.model, models_yml.as_deref());
            shield_models(&mut cmd, &run_dir.0, &model).await?;
            cmd.arg("--model").arg(model);
        }
        cmd.arg("--resume").arg(sid);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("Failed to spawn omp in rpc mode")?;
        announce_spawn(opts.event_tx.as_ref(), &child);
        let stderr = stderr_tail(child.stderr.take(), opts.redactor());
        let stdin = child.stdin.take().context("omp stdin missing")?;
        let lines = BufReader::new(child.stdout.take().context("omp stdout missing")?).lines();
        Ok(Self { child, stdin, lines, stderr, _run_dir: run_dir })
    }

    async fn ask(&mut self, id: &'static str, mut frame: Value) -> Result<Value> {
        frame["id"] = Value::from(id);
        self.stdin.write_all(format!("{frame}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        while let Some(line) = self.lines.next_line().await? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame.get("type").and_then(Value::as_str) != Some("response") || frame.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }
            if frame.get("success").and_then(Value::as_bool) != Some(true) {
                anyhow::bail!("omp {id} failed: {}", frame.get("error").cloned().unwrap_or_default());
            }
            return Ok(frame.get("data").cloned().unwrap_or_default());
        }
        anyhow::bail!("omp closed before answering {id}")
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().trim().to_string()
    }

    async fn close(mut self) {
        let _ = self.child.kill().await;
    }
}

fn stats_usage(stats: &Value) -> (Usage, f64) {
    let tokens = stats.get("tokens").cloned().unwrap_or_default();
    let n = |k: &str| tokens.get(k).and_then(Value::as_u64).unwrap_or(0);
    let usage = Usage {
        input_tokens: n("input") + n("cacheWrite"),
        cached_input_tokens: n("cacheRead"),
        output_tokens: n("output"),
        context_tokens: stats.pointer("/contextUsage/tokens").and_then(Value::as_u64).unwrap_or(0),
    };
    (usage, stats.get("cost").and_then(Value::as_f64).unwrap_or(0.0))
}

fn usage_since(after: Usage, before: Usage) -> Usage {
    Usage {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        cached_input_tokens: after.cached_input_tokens.saturating_sub(before.cached_input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        context_tokens: after.context_tokens,
    }
}

async fn compact(opts: &CliOptions<'_>, sid: &str) -> Result<CliResult> {
    let started = std::time::Instant::now();
    let mut probe = RpcProbe::open(opts, sid).await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(600), async {
        let (before, cost_before) = stats_usage(&probe.ask("before", serde_json::json!({ "type": "get_session_stats" })).await?);
        let compacted = match probe.ask("compact", serde_json::json!({ "type": "compact" })).await {
            Ok(compacted) => compacted,
            Err(error) if error.to_string().contains("Nothing to compact") => Value::Null,
            Err(error) => return Err(error),
        };
        let (after, cost_after) = stats_usage(&probe.ask("after", serde_json::json!({ "type": "get_session_stats" })).await?);
        let state = probe.ask("state", serde_json::json!({ "type": "get_state" })).await?;
        let session = state.get("sessionId").and_then(Value::as_str).unwrap_or(sid).to_string();
        let tokens_before = compacted.get("tokensBefore").and_then(Value::as_u64).unwrap_or(before.context_tokens);
        info!(
            from = sid,
            to = %session,
            tokens_before,
            tokens_after = after.context_tokens,
            ms = started.elapsed().as_millis() as u64,
            "Compacted omp session"
        );
        Ok::<_, anyhow::Error>(CliResult {
            output: "compacted".to_string(),
            session_id: Some(session),
            cost_usd: (cost_after - cost_before).max(0.0),
            exit_code: 0,
            usage: usage_since(after, before),
            checkpoint: None,
        })
    })
    .await;
    let stderr = probe.stderr();
    probe.close().await;
    match result {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(e)) if stderr.is_empty() || opts.sensitive || opts.omp_models.is_some() || !(opts.environment.is_empty() && opts.provider_keys.is_empty()) => Err(e),
        Ok(Err(e)) => Err(e.context(stderr)),
        Err(_) => anyhow::bail!("omp compaction timed out"),
    }
}

async fn rewind(opts: &CliOptions<'_>, sid: &str, drop_head: &str) -> Result<String> {
    let mut probe = RpcProbe::open(opts, sid).await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        let messages = probe.ask("messages", serde_json::json!({ "type": "get_branch_messages" })).await?;
        let head: String = drop_head.trim().chars().take(200).collect();
        let entry = messages
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| m.get("text").and_then(Value::as_str).is_some_and(|t| t.contains(&head)))
            .filter_map(|m| m.get("entryId").and_then(Value::as_str))
            .next_back()
            .map(String::from)
            .context("could not find the rewound turn in the omp session")?;
        let branched = probe.ask("branch", serde_json::json!({ "type": "branch", "entryId": entry })).await?;
        if branched.get("cancelled").and_then(Value::as_bool) == Some(true) {
            anyhow::bail!("omp cancelled the branch");
        }
        let state = probe.ask("state", serde_json::json!({ "type": "get_state" })).await?;
        state
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| *id != sid)
            .map(String::from)
            .context("omp did not switch to a branched session")
    })
    .await;
    let stderr = probe.stderr();
    probe.close().await;
    match result {
        Ok(Ok(id)) => Ok(id),
        Ok(Err(e)) => Err(e.context(stderr)),
        Err(_) => anyhow::bail!("omp rewind timed out"),
    }
}

fn sandbox_spec(opts: &CliOptions<'_>, run_dir: &Path, sessions: &Path) -> crate::sandbox_wrap::Spec {
    let omp_home = home().join(".omp");
    let state: Vec<PathBuf> = vec![run_dir.to_path_buf(), sessions.to_path_buf(), omp_home.join("logs"), omp_home.join("run")];
    for dir in &state[2..] {
        let _ = std::fs::create_dir_all(dir);
    }
    crate::sandbox_wrap::Spec {
        working_dir: opts.working_dir.to_path_buf(),
        read_only: opts.approval == crate::cli::Approval::ReadOnly,
        readable: opts.image_paths.clone(),
        state,
    }
}

pub(crate) async fn build_command(opts: &CliOptions<'_>, rpc: bool) -> Result<(Command, RunDir, String)> {
    let sessions = state_dir().join("sessions");
    tokio::fs::create_dir_all(&sessions)
        .await
        .with_context(|| format!("Failed to create omp session dir {}", sessions.display()))?;
    let run_dir = prepare_run_dir(opts).await?;

    let mut cmd = job_command("omp");
    cmd.kill_on_drop(true);
    cmd.current_dir(opts.working_dir);
    configure_child_environment(&mut cmd, &opts.child_environment())?;
    opts.sign_commits(&mut cmd);
    cmd.env_remove("OMP_PROFILE").env_remove("PI_PROFILE");
    cmd.env("PI_CODING_AGENT_DIR", &run_dir.0);
    if rpc {
        cmd.args(["--mode", "rpc", "--auto-approve", "--no-title"]);
    } else {
        cmd.args(["--print", "--mode", "json", "--auto-approve", "--no-title"]);
    }
    cmd.arg("--session-dir").arg(&sessions);
    let model = if opts.model.is_empty() {
        String::new()
    } else {
        let models_yml = tokio::fs::read_to_string(run_dir.0.join("models.yml")).await.ok();
        resolve_model(opts.model, models_yml.as_deref())
    };
    shield_models(&mut cmd, &run_dir.0, &model).await?;
    if !model.is_empty() {
        cmd.arg("--model").arg(&model);
    }
    if let Some(sid) = opts.session_id.filter(|_| opts.resume) {
        cmd.arg(if opts.fork { "--fork" } else { "--resume" }).arg(sid);
    }
    if let Some(level) = opts.effort.filter(|e| THINKING_LEVELS.contains(e)) {
        cmd.arg("--thinking").arg(level);
    }

    let system_path = run_dir.0.join("system.md");
    if opts.lean {
        cmd.args(["--no-tools", "--no-skills", "--no-rules", "--no-extensions", "--no-lsp"]);
        if let Some(system) = opts.system_prompt {
            write_private(&system_path, system.as_bytes()).await?;
            cmd.arg("--system-prompt").arg(&system_path);
        }
    } else {
        let mut system = AGENT_PREAMBLE.to_string();
        if let Some(extra) = opts.system_prompt {
            system.push_str("\n\n");
            system.push_str(extra);
        }
        write_private(&system_path, system.as_bytes()).await?;
        cmd.arg("--append-system-prompt").arg(&system_path);
        if let Some(tools) = opts.allowed_tools.map(omp_tools).filter(|t| !t.is_empty()) {
            cmd.arg("--tools").arg(tools);
        }
    }
    if !rpc {
        for image in &opts.image_paths {
            cmd.arg(format!("@{}", image.display()));
        }
    }
    let mut run_dir = run_dir;
    if !opts.lean {
        let (wrapped, temp) = crate::sandbox_wrap::wrap("omp", cmd, &sandbox_spec(opts, &run_dir.0, &sessions), &opts.environment)?;
        cmd = wrapped;
        run_dir.1 = temp;
    }

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    Ok((cmd, run_dir, model))
}

#[derive(Default)]
struct Reader {
    session_id: Option<String>,
    output: String,
    outputs: Vec<String>,
    totals: Totals,
    error_message: Option<String>,
    tools: Option<crate::cli::OpenTools>,
}

impl Reader {
    fn feed(&mut self, json: &Value, events: Option<&crate::cli::EventSender>) {
        let send = |event: StreamEvent| {
            if let Some(tx) = events {
                let _ = tx.send(event);
            }
        };
        match json.get("type").and_then(Value::as_str).unwrap_or("") {
            "session" if self.session_id.is_none() => {
                if let Some(sid) = json.get("id").and_then(Value::as_str) {
                    self.session_id = Some(sid.to_string());
                    send(StreamEvent::SessionStarted { session_id: sid.to_string() });
                }
            }
            "message_end" => {
                let message = &json["message"];
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    return;
                }
                if let Some(usage) = message.get("usage") {
                    self.totals.add(usage);
                    send(StreamEvent::Usage(self.totals.usage));
                }
                if message.get("stopReason").and_then(Value::as_str) == Some("error") {
                    let error = message
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("omp model call failed")
                        .to_string();
                    send(StreamEvent::Error(error.clone()));
                    self.error_message = Some(error);
                    return;
                }
                let text = message_text(message);
                if !text.is_empty() {
                    send(StreamEvent::AssistantText(text.clone()));
                    self.output = text;
                    self.error_message = None;
                }
            }
            "tool_execution_start" => {
                let tool = json.get("toolName").and_then(Value::as_str).unwrap_or("tool");
                if let (Some(tools), Some(id)) = (self.tools.as_mut(), json.get("toolCallId").and_then(Value::as_str)) {
                    tools.start(id);
                }
                send(StreamEvent::ToolUse {
                    tool: tool.to_string(),
                    input_summary: summarize_input(tool, &json["args"]),
                });
            }
            "tool_execution_end" => {
                let tool = json.get("toolName").and_then(Value::as_str).unwrap_or("tool");
                if let (Some(tools), Some(id)) = (self.tools.as_mut(), json.get("toolCallId").and_then(Value::as_str)) {
                    tools.finish(id);
                }
                send(StreamEvent::ToolResult {
                    tool: tool.to_string(),
                    output: summarize_tool_output(&json["result"]),
                });
            }
            _ => {}
        }
    }

    fn settle(&mut self) {
        let text = std::mem::take(&mut self.output);
        if !text.trim().is_empty() {
            self.outputs.push(text);
        }
        if let Some(tools) = self.tools.as_mut() {
            tools.clear();
        }
    }
}

fn frame(kind: &str, id: &str, message: Option<&str>) -> String {
    let mut value = serde_json::json!({ "type": kind, "id": id });
    if let Some(message) = message {
        value["message"] = Value::from(message);
    }
    format!("{value}\n")
}

async fn image_contents(paths: &[PathBuf]) -> Result<Vec<Value>> {
    use base64::Engine;
    let mut images = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read image {}", path.display()))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let mime = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/png",
        };
        images.push(serde_json::json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mimeType": mime,
        }));
    }
    Ok(images)
}

fn prompt_frame(id: &str, message: &str, images: Vec<Value>) -> String {
    let mut value = serde_json::json!({ "type": "prompt", "id": id, "message": message });
    if !images.is_empty() {
        value["images"] = Value::Array(images);
    }
    format!("{value}\n")
}

struct Turn {
    reader: Reader,
    failure: Option<String>,
    ended: bool,
    first_output_ms: Option<u64>,
}

async fn drive<W, R>(
    stdin: &mut W,
    lines: &mut tokio::io::Lines<R>,
    opts: &CliOptions<'_>,
    steer: &mut Option<crate::cli::SteerReceiver>,
    sent: &mut usize,
    started: std::time::Instant,
) -> Result<Turn>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let interrupt_after = crate::cli::steer_interrupt_after();
    let hang_after = crate::cli::tool_hang_after();
    let mut watchdog = tokio::time::interval(std::time::Duration::from_secs(15));
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reader = Reader { tools: Some(crate::cli::OpenTools::new()), ..Default::default() };
    let mut failure: Option<String> = None;
    let mut ended = false;
    let mut first_output_ms: Option<u64> = None;
    let mut open = 1usize;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    ended = true;
                    break;
                };
                let Ok(json) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                match json.get("type").and_then(Value::as_str).unwrap_or("") {
                    "response" if json.get("id").and_then(Value::as_str) == Some("state") => {
                        if let Some(sid) = json.pointer("/data/sessionId").and_then(Value::as_str) {
                            if reader.session_id.is_none() {
                                reader.session_id = Some(sid.to_string());
                                if let Some(tx) = &opts.event_tx {
                                    let _ = tx.send(StreamEvent::SessionStarted { session_id: sid.to_string() });
                                }
                            }
                        }
                    }
                    "response" if json.get("success").and_then(Value::as_bool) == Some(false) => {
                        let error = json.get("error").map(Value::to_string).unwrap_or_default();
                        warn!(%error, "omp rejected a command");
                        if json.get("command").and_then(Value::as_str) == Some("prompt") {
                            failure = Some(error);
                            break;
                        }
                    }
                    "prompt_result" => {
                        if json.get("status").and_then(Value::as_str) == Some("error") {
                            if let Some(message) = json.pointer("/error/message").and_then(Value::as_str) {
                                failure = Some(message.to_string());
                            }
                        }
                        open = open.saturating_sub(1);
                        reader.settle();
                        if open > 0 {
                            continue;
                        }
                        let mut next: Vec<String> = Vec::new();
                        if let Some(rx) = steer.as_mut() {
                            while let Ok(steer) = rx.try_recv() {
                                next.push(steer.text);
                                let _ = steer.ack.send(true);
                            }
                        }
                        if next.is_empty() {
                            break;
                        }
                        stdin.write_all(frame("prompt", &format!("prompt-{sent}"), Some(&next.join("\n\n"))).as_bytes()).await?;
                        stdin.flush().await?;
                        *sent += 1;
                        open += 1;
                    }
                    _ => {
                        reader.feed(&json, opts.event_tx.as_ref());
                        if first_output_ms.is_none() && !reader.output.is_empty() {
                            first_output_ms = Some(started.elapsed().as_millis() as u64);
                        }
                    }
                }
            }
            Some(next) = async {
                match steer.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let stuck = reader.tools.as_ref().is_some_and(|t| t.stuck_longer_than(interrupt_after));
                let interrupt = next.interrupt || stuck;
                let id = format!("prompt-{sent}");
                let written = if open == 0 {
                    open += 1;
                    stdin.write_all(frame("prompt", &id, Some(&next.text)).as_bytes()).await
                } else if interrupt {
                    open += 1;
                    let text = format!("{}\n\n{}", crate::cli::INTERRUPTED_NOTE, next.text);
                    stdin.write_all(frame("abort_and_prompt", &id, Some(&text)).as_bytes()).await
                } else {
                    stdin.write_all(frame("steer", &id, Some(&next.text)).as_bytes()).await
                };
                *sent += 1;
                let _ = stdin.flush().await;
                let _ = next.ack.send(written.is_ok());
                written?;
                info!(interrupt, "Steered running omp session");
            }
            _ = watchdog.tick() => {
                let age = reader.tools.as_ref().and_then(|t| t.oldest());
                if let (Some(limit), Some(age)) = (hang_after, age) {
                    if age >= limit && open > 0 {
                        warn!(secs = age.as_secs(), "omp tool call looks stuck, interrupting");
                        if let Some(tools) = reader.tools.as_mut() {
                            tools.clear();
                        }
                        open += 1;
                        stdin.write_all(frame("abort_and_prompt", &format!("prompt-{sent}"), Some(&crate::cli::hang_notice(age))).as_bytes()).await?;
                        stdin.flush().await?;
                        *sent += 1;
                    }
                }
            }
        }
    }
    if let Some(rx) = steer.as_mut() {
        crate::cli::close_steers(rx);
    }
    reader.settle();
    Ok(Turn { reader, failure, ended, first_output_ms })
}

fn turn_result(turn: Turn, session_id: Option<String>, stderr: &str) -> Result<CliResult> {
    let Turn { reader, failure, .. } = turn;
    if reader.outputs.is_empty() {
        if let Some(error) = failure.or(reader.error_message) {
            anyhow::bail!("omp model call failed: {error}");
        }
        anyhow::bail!("omp ended without an answer: {stderr}");
    }
    Ok(CliResult {
        output: reader.outputs.join("\n\n"),
        session_id: reader.session_id.or(session_id),
        cost_usd: reader.totals.cost_usd,
        exit_code: 0,
        usage: reader.totals.usage,
        checkpoint: None,
    })
}

async fn run_steerable(opts: &CliOptions<'_>, steer: crate::cli::SteerReceiver) -> Result<CliResult> {
    let (mut cmd, _run_dir, model) = build_command(opts, true).await?;
    info!(dir = %opts.working_dir.display(), model = %model, fork = opts.fork, "Invoking omp with steering");
    let mut child = cmd
        .spawn()
        .context("Failed to spawn omp CLI. Install it with: bun install -g @oh-my-pi/pi-coding-agent")?;
    announce_spawn(opts.event_tx.as_ref(), &child);
    let stderr = stderr_tail(child.stderr.take(), opts.redactor());
    let mut stdin = child.stdin.take().context("omp stdin missing")?;
    let mut lines = BufReader::new(child.stdout.take().context("omp stdout missing")?).lines();
    stdin.write_all(frame("get_state", "state", None).as_bytes()).await?;
    stdin.write_all(frame("prompt", "prompt-0", Some(opts.prompt)).as_bytes()).await?;
    stdin.flush().await?;
    let mut sent = 1usize;
    let mut steer = Some(steer);
    let turn = drive(&mut stdin, &mut lines, opts, &mut steer, &mut sent, std::time::Instant::now()).await?;
    drop(stdin);
    if tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    let stderr = stderr.lock().unwrap().trim().to_string();
    turn_result(turn, None, &stderr)
}

pub(crate) struct WarmOmp {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    session_id: Option<String>,
    sent: usize,
    _run_dir: RunDir,
}

impl WarmOmp {
    pub(crate) async fn spawn(opts: &CliOptions<'_>) -> Result<Self> {
        let (mut cmd, run_dir, model) = build_command(opts, true).await?;
        info!(dir = %opts.working_dir.display(), model = %model, "Starting warm omp session");
        let mut child = cmd
            .spawn()
            .context("Failed to spawn omp CLI. Install it with: bun install -g @oh-my-pi/pi-coding-agent")?;
        announce_spawn(opts.event_tx.as_ref(), &child);
        let stderr = stderr_tail(child.stderr.take(), opts.redactor());
        let mut stdin = child.stdin.take().context("omp stdin missing")?;
        let mut lines = BufReader::new(child.stdout.take().context("omp stdout missing")?).lines();
        stdin.write_all(frame("get_state", "state", None).as_bytes()).await?;
        stdin.flush().await?;
        let ready = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while let Some(line) = lines.next_line().await? {
                let Ok(json) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if json.get("type").and_then(Value::as_str) == Some("response")
                    && json.get("id").and_then(Value::as_str) == Some("state")
                {
                    return Ok(json.pointer("/data/sessionId").and_then(Value::as_str).map(String::from));
                }
            }
            anyhow::bail!("omp exited before it was ready")
        })
        .await;
        let session_id = match ready {
            Ok(Ok(id)) => id,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                return Err(error.context(stderr.lock().unwrap().trim().to_string()));
            }
            Err(_) => {
                let _ = child.kill().await;
                anyhow::bail!("omp did not get ready in time");
            }
        };
        if let (Some(tx), Some(sid)) = (&opts.event_tx, &session_id) {
            let _ = tx.send(StreamEvent::SessionStarted { session_id: sid.clone() });
        }
        Ok(Self { child, stdin, lines, stderr, session_id, sent: 0, _run_dir: run_dir })
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub(crate) fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub(crate) fn stderr_tail(&self) -> String {
        self.stderr.lock().unwrap().trim().to_string()
    }

    pub(crate) async fn ask(
        &mut self,
        opts: &CliOptions<'_>,
        started: std::time::Instant,
    ) -> Result<(CliResult, crate::cli::Timing)> {
        let images = image_contents(&opts.image_paths).await?;
        let id = format!("prompt-{}", self.sent);
        self.sent += 1;
        self.stdin
            .write_all(prompt_frame(&id, opts.prompt, images).as_bytes())
            .await
            .context("warm omp session stdin closed")?;
        self.stdin.flush().await?;
        let mut steer = opts.steer.take();
        let turn = drive(&mut self.stdin, &mut self.lines, opts, &mut steer, &mut self.sent, started).await?;
        if turn.ended {
            anyhow::bail!("warm omp session exited mid-turn");
        }
        let timing = crate::cli::Timing {
            first_output_ms: turn.first_output_ms,
            result_ms: Some(started.elapsed().as_millis() as u64),
            ..Default::default()
        };
        let result = turn_result(turn, self.session_id.clone(), &self.stderr_tail())?;
        if result.session_id.is_some() {
            self.session_id = result.session_id.clone();
        }
        Ok((result, timing))
    }

    pub(crate) async fn shutdown(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await.is_err() {
            if let Err(e) = self.child.kill().await {
                warn!(error = %e, "Could not kill warm omp session");
            }
        }
    }
}

async fn run_print(opts: CliOptions<'_>) -> Result<CliResult> {
    let (mut cmd, _run_dir, model) = build_command(&opts, false).await?;

    info!(
        dir = %opts.working_dir.display(),
        model = %model,
        resume = opts.resume && opts.session_id.is_some(),
        fork = opts.fork,
        lean = opts.lean,
        "Invoking omp CLI"
    );

    let mut child = cmd
        .spawn()
        .context("Failed to spawn omp CLI. Install it with: bun install -g @oh-my-pi/pi-coding-agent")?;
    announce_spawn(opts.event_tx.as_ref(), &child);
    let stderr = stderr_tail(child.stderr.take(), opts.redactor());
    {
        let mut stdin = child.stdin.take().expect("stdin piped");
        stdin.write_all(opts.prompt.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let mut lines = BufReader::new(child.stdout.take().expect("stdout piped")).lines();
    let mut reader = Reader::default();

    while let Some(line) = lines.next_line().await? {
        let Ok(json) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        reader.feed(&json, opts.event_tx.as_ref());
    }
    let Reader { session_id, output, totals, error_message, .. } = reader;

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = stderr.lock().unwrap().trim().to_string();
    if !stderr.is_empty() && (opts.environment.is_empty() && opts.provider_keys.is_empty()) && !opts.mcp_has_secrets && !opts.sensitive && opts.omp_models.is_none() {
        debug!(stderr = %stderr, "omp stderr");
    }
    if output.is_empty() {
        if let Some(error) = error_message {
            anyhow::bail!("omp model call failed: {error}");
        }
        if exit_code != 0 {
            anyhow::bail!("omp exited with code {exit_code}: {stderr}");
        }
    }

    info!(
        exit_code,
        session_id = session_id.as_deref().unwrap_or("none"),
        input_tokens = totals.usage.input_tokens,
        output_tokens = totals.usage.output_tokens,
        "omp CLI completed"
    );

    Ok(CliResult {
        output,
        session_id,
        cost_usd: totals.cost_usd,
        exit_code,
        usage: totals.usage,
        checkpoint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{message_text, omp_tools, resolve_model, run, run_mcp_json, Totals};
    use crate::cli::CliOptions;

    #[test]
    fn imported_servers_from_other_tools_are_denied() {
        let foreign = std::collections::BTreeSet::from(["github".to_string(), "personal".to_string()]);
        let config: serde_json::Value = serde_json::from_str(&run_mcp_json(Some(r#"{"mcpServers":{"github":{"command":"x"}}}"#), &foreign).unwrap().unwrap()).unwrap();
        assert_eq!(config["disabledServers"], serde_json::json!(["personal"]));
        assert_eq!(config["mcpServers"]["github"]["command"], "x");
        let bare: serde_json::Value = serde_json::from_str(&run_mcp_json(None, &foreign).unwrap().unwrap()).unwrap();
        assert_eq!(bare["disabledServers"], serde_json::json!(["github", "personal"]));
        assert!(run_mcp_json(None, &Default::default()).unwrap().is_none());
    }

    fn options<'a>(dir: &'a std::path::Path, prompt: &'a str, model: &'a str, session: Option<&'a str>, fork: bool) -> CliOptions<'a> {
        CliOptions {
            working_dir: dir,
            prompt,
            system_prompt: Some("Answer in one short sentence."),
            allowed_tools: None,
            max_turns: 3,
            model,
            mcp_config_json: None,
            session_id: session,
            resume: session.is_some(),
            event_tx: None,
            image_paths: vec![],
            fork,
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
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_resume_and_fork() {
        let model = std::env::var("OMP_TEST_MODEL").expect("OMP_TEST_MODEL");
        let dir = std::env::temp_dir().join(format!("loka-omp-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = run(options(&dir, "Remember the word heliotrope. Reply ok.", &model, None, false)).await.unwrap();
        let sid = first.session_id.clone().expect("session id");
        let second = run(options(&dir, "What word did I ask you to remember?", &model, Some(&sid), false)).await.unwrap();
        assert_eq!(second.session_id.as_deref(), Some(sid.as_str()));
        let fork = run(options(&dir, "Say the word again.", &model, Some(&sid), true)).await.unwrap();
        assert!(fork.session_id.is_some_and(|id| id != sid));
        println!("{}\n{}\n{}", first.output, second.output, fork.output);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore]
    async fn live_omp_sends_its_key_through_the_proxy() {
        let proxy = crate::key_proxy::tests::test_proxy();
        let (upstream, seen) = crate::key_proxy::tests::upstream().await;
        let dir = std::env::temp_dir().join(format!("omp-key-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (provider, model, var, value, base_var) in [
            ("anthropic", "anthropic/claude-sonnet-4-5", "ANTHROPIC_API_KEY", "sk-ant-api03-omp-real", Some("ANTHROPIC_BASE_URL")),
            ("openrouter", "openrouter/anthropic/claude-sonnet-4.5", "OPENROUTER_API_KEY", "sk-or-v1-omp-real", None),
        ] {
            seen.lock().unwrap().clear();
            let mut cmd = tokio::process::Command::new(std::env::var("OMP_BIN").unwrap_or_else(|_| "omp".into()));
            cmd.current_dir(&dir);
            cmd.env("PI_CODING_AGENT_DIR", &dir);
            cmd.env(var, value);
            let mut config = serde_yaml::Value::Mapping(Default::default());
            if let Some(base_var) = base_var {
                cmd.env(base_var, &upstream);
            } else {
                config = serde_yaml::from_str(&format!("providers:\n  {provider}:\n    baseUrl: {upstream}/api/v1\n    apiKey: {var}\n")).unwrap();
            }
            assert!(super::shield_provider(proxy, &mut cmd, &mut config, provider));
            let _ = std::fs::remove_file(dir.join("models.yml"));
            std::fs::write(dir.join("models.yml"), serde_yaml::to_string(&config).unwrap()).unwrap();
            cmd.args(["--print", "--mode", "json", "--no-title", "--no-tools", "--model", model, "hi"]);
            let output = crate::key_proxy::tests::run_cli(cmd).await;
            let hits = seen.lock().unwrap().clone();
            assert!(hits.iter().any(|(_, key)| key.contains(value)), "{provider}: {hits:?} {output}");
            assert!(!hits.iter().any(|(_, key)| key.contains("loka-")), "{provider}: {hits:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_models_provider_goes_through_the_key_proxy() {
        let proxy = crate::key_proxy::tests::test_proxy();
        let mut config: serde_yaml::Value = serde_yaml::from_str(include_str!("../../../shared/omp-models.yml")).unwrap();
        let mut cmd = tokio::process::Command::new("true");
        cmd.env("FAL_KEY", "id:real-fal-secret");
        cmd.env("ANTHROPIC_API_KEY", "sk-ant-api03-real");
        assert!(super::shield_provider(proxy, &mut cmd, &mut config, "fal"));
        let env: std::collections::HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().to_string(), v.unwrap().to_string_lossy().to_string()))
            .collect();
        assert!(env["FAL_KEY"].starts_with("loka-"));
        assert_eq!(env["ANTHROPIC_API_KEY"], "sk-ant-api03-real");
        let text = serde_yaml::to_string(&config).unwrap();
        assert!(!text.contains("https://fal.run"), "{text}");
        assert!(text.contains("http://127.0.0.1:"));

        let mut custom: serde_yaml::Value =
            serde_yaml::from_str("providers:\n  mine:\n    baseUrl: https://llm.example/v1\n    apiKey: literal-secret-value\n    models:\n      - id: m\n").unwrap();
        assert!(super::shield_provider(proxy, &mut cmd, &mut custom, "mine"));
        let text = serde_yaml::to_string(&custom).unwrap();
        assert!(!text.contains("literal-secret-value") && !text.contains("llm.example"), "{text}");

        let mut builtin = serde_yaml::Value::Mapping(Default::default());
        assert!(super::shield_provider(proxy, &mut cmd, &mut builtin, "anthropic"));
        assert!(serde_yaml::to_string(&builtin).unwrap().contains("baseUrl: http://127.0.0.1:"));
        let mut missing = serde_yaml::Value::Mapping(Default::default());
        assert!(!super::shield_provider(proxy, &mut cmd, &mut missing, "openrouter"));
    }

    #[test]
    fn resolves_bare_model_names_against_models_yml() {
        let yml = include_str!("../../../shared/omp-models.yml");
        assert_eq!(resolve_model("kimi", Some(yml)), "fal/kimi-k3");
        assert_eq!(resolve_model("glm-5.3", Some(yml)), "fal/glm-5.3");
        assert_eq!(resolve_model("fal/kimi-k3", Some(yml)), "fal/kimi-k3");
        assert_eq!(resolve_model("opus", Some(yml)), "opus");
        assert_eq!(resolve_model("kimi", None), "kimi");
        let custom = "providers:\n  mine:\n    baseUrl: https://x\n    models:\n      - id: house-7b\n";
        let merged = loka_shared::omp_config::merge(Some(yml), custom).unwrap();
        assert_eq!(resolve_model("house", Some(&merged)), "mine/house-7b");
        assert_eq!(resolve_model("kimi", Some(&merged)), "fal/kimi-k3");
    }

    #[test]
    fn maps_claude_tool_names() {
        assert_eq!(omp_tools("Read,Glob,Grep,Bash"), "read,glob,grep,bash");
        assert_eq!(omp_tools(""), "");
    }

    #[test]
    fn joins_text_blocks_and_skips_thinking() {
        let message = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "hello "},
                {"type": "toolCall", "name": "bash"},
                {"type": "text", "text": "world"}
            ]
        });
        assert_eq!(message_text(&message), "hello world");
    }

    #[test]
    fn sums_usage_across_calls() {
        let mut totals = Totals::default();
        let usage = serde_json::json!({
            "input": 10, "output": 5, "cacheRead": 100, "cacheWrite": 2,
            "cost": {"total": 0.25}
        });
        totals.add(&usage);
        totals.add(&usage);
        assert_eq!(totals.usage.input_tokens, 24);
        assert_eq!(totals.usage.cached_input_tokens, 200);
        assert_eq!(totals.usage.output_tokens, 10);
        assert_eq!(totals.usage.context_tokens, 117);
        assert!((totals.cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn compaction_usage_is_the_stats_difference() {
        let before = serde_json::json!({ "tokens": { "input": 100, "output": 10, "cacheRead": 1000, "cacheWrite": 0 }, "cost": 0.5, "contextUsage": { "tokens": 4000 } });
        let after = serde_json::json!({ "tokens": { "input": 300, "output": 60, "cacheRead": 1000, "cacheWrite": 5 }, "cost": 0.75, "contextUsage": { "tokens": 900 } });
        let (before, cost_before) = super::stats_usage(&before);
        let (after, cost_after) = super::stats_usage(&after);
        let used = super::usage_since(after, before);
        assert_eq!((used.input_tokens, used.cached_input_tokens, used.output_tokens, used.context_tokens), (205, 0, 50, 900));
        assert!((cost_after - cost_before - 0.25).abs() < 1e-9);
    }

    #[tokio::test]
    #[ignore]
    async fn live_omp_compact() {
        let model = std::env::var("OMP_TEST_MODEL").expect("OMP_TEST_MODEL");
        let dir = std::env::temp_dir().join(format!("loka-omp-compact-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = run(options(&dir, "Remember the word heliotrope. Reply ok.", &model, None, false)).await.unwrap();
        let sid = first.session_id.clone().expect("session id");
        let filler = (0..1500).map(|i| format!("Line {i} of a long design note about river barges, tides and cargo manifests.")).collect::<Vec<_>>().join("\n");
        let long = format!("Also remember the word marmalade. Here is a long note to keep in mind:\n{filler}\nReply ok.");
        let second = run(options(&dir, &long, &model, Some(&sid), false)).await.unwrap();
        eprintln!("context before compaction {}", second.usage.context_tokens);
        assert!(second.usage.context_tokens > 0);
        let started = std::time::Instant::now();
        let compacted = run(options(&dir, crate::cli::COMPACT_PROMPT, &model, Some(&sid), false)).await.unwrap();
        eprintln!("compaction took {} ms, context after {}, usage {:?}, cost {}", started.elapsed().as_millis(), compacted.usage.context_tokens, compacted.usage, compacted.cost_usd);
        let session = compacted.session_id.expect("compacted session id");
        let answer = run(options(&dir, "Which two words did I ask you to remember?", &model, Some(&session), false)).await.unwrap();
        let reply = answer.output.to_lowercase();
        assert!(reply.contains("heliotrope") && reply.contains("marmalade"), "{}", answer.output);
        eprintln!("after compaction answered with context {}", answer.usage.context_tokens);
        assert!(compacted.usage.context_tokens < second.usage.context_tokens);
    }
}

#[cfg(test)]
mod redaction_tests {
    use crate::cli::straddle::{around, assert_whole_then_hidden, drain, pads};
    use serde_json::json;

    #[test]
    fn the_omp_parser_sends_whole_text_so_a_secret_across_any_cut_is_hidden() {
        for pad in pads() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut reader = super::Reader::default();
            reader.feed(&json!({"type": "tool_execution_start", "toolName": "bash", "args": {"command": around(pad)}}), Some(&tx));
            reader.feed(&json!({"type": "tool_execution_end", "toolName": "bash", "result": around(pad)}), Some(&tx));
            reader.feed(
                &json!({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "text", "text": around(pad)}]}}),
                Some(&tx),
            );
            drop(tx);
            assert_whole_then_hidden(drain(rx), 3, "omp", pad);
        }
    }
}
