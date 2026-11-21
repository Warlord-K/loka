use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{info, warn};

use super::full_prompt;
use crate::cli::{configure_child_environment, job_command, own_cli_state, stderr_tail, Approval, CliOptions, CliResult, StreamEvent, Timing, Usage};

pub struct AppServer {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    secrets: Vec<String>,
    next_id: u64,
    thread_id: Option<String>,
}

fn probe_options<'a>(opts: &CliOptions<'a>) -> CliOptions<'a> {
    CliOptions {
        working_dir: opts.working_dir,
        prompt: "",
        system_prompt: None,
        allowed_tools: None,
        max_turns: 1,
        model: opts.model,
        mcp_config_json: None,
        session_id: None,
        resume: false,
        event_tx: None,
        image_paths: Vec::new(),
        fork: false,
        lean: true,
        effort: None,
        fast: false,
        environment: opts.environment.clone(),
        mcp_has_secrets: false,
        sensitive: opts.sensitive,
        rewind_to: None,
        approval: opts.approval,
        steer: Default::default(),
        omp_models: None,
        provider_keys: opts.provider_keys.clone(),
        readable_files: opts.readable_files.clone(),
    }
}

pub async fn rewind(opts: &CliOptions<'_>, thread_id: &str, drop_head: &str) -> Result<Option<String>> {
    let mut server = AppServer::spawn(&probe_options(opts)).await?;
    let result = server.rewind(thread_id, drop_head).await;
    server.shutdown().await;
    result
}

pub async fn compact(opts: &CliOptions<'_>, thread_id: &str) -> Result<CliResult> {
    let probe = probe_options(opts);
    let mut server = AppServer::spawn(&probe).await?;
    let result = tokio::time::timeout(Duration::from_secs(600), server.compact(&probe, thread_id)).await;
    let tail = server.stderr_tail();
    server.shutdown().await;
    match result {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) if tail.is_empty() || opts.sensitive || !(opts.environment.is_empty() && opts.provider_keys.is_empty()) => Err(error),
        Ok(Err(error)) => Err(error.context(tail)),
        Err(_) => bail!("Codex compaction timed out"),
    }
}

fn turn_texts(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("text", Value::String(text)) => {
                        out.push_str(text);
                        out.push('\n');
                    }
                    _ => turn_texts(child, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| turn_texts(item, out)),
        _ => {}
    }
}

impl AppServer {
    async fn rewind(&mut self, thread_id: &str, drop_head: &str) -> Result<Option<String>> {
        let head = drop_head.trim();
        let head = head.chars().take(200).collect::<String>();
        let mut turns: Vec<(String, String)> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let response = self
                .request(
                    "thread/turns/list",
                    json!({ "threadId": thread_id, "sortDirection": "asc", "itemsView": "full", "limit": 100, "cursor": cursor }),
                )
                .await?;
            for turn in response.get("data").and_then(Value::as_array).into_iter().flatten() {
                let Some(id) = turn.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let mut text = String::new();
                turn_texts(turn.get("items").unwrap_or(&Value::Null), &mut text);
                turns.push((id.to_string(), text));
            }
            cursor = response.get("nextCursor").and_then(Value::as_str).map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        let index = turns
            .iter()
            .rposition(|(_, text)| text.contains(&head))
            .ok_or_else(|| anyhow!("could not find the rewound turn in Codex thread {thread_id}"))?;
        if index == 0 {
            return Ok(None);
        }
        let response = self
            .request(
                "thread/fork",
                json!({ "threadId": thread_id, "lastTurnId": turns[index - 1].0, "excludeTurns": true }),
            )
            .await?;
        response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(|id| Some(id.to_string()))
            .ok_or_else(|| anyhow!("codex app-server returned no forked thread id"))
    }

    async fn compact(&mut self, opts: &CliOptions<'_>, thread_id: &str) -> Result<CliResult> {
        let params = json!({
            "threadId": thread_id,
            "model": opts.model,
            "cwd": opts.working_dir,
            "sandbox": sandbox(opts),
            "approvalPolicy": "never",
            "excludeTurns": true,
        });
        self.request("thread/resume", params).await?;
        let started = Instant::now();
        let compact_request = self.start_request("thread/compact/start", json!({ "threadId": thread_id })).await?;
        let mut usage = Usage::default();
        let mut last_error: Option<String> = None;
        loop {
            let message = self.read().await?;
            if message.get("method").is_some() && message.get("id").is_some() {
                self.answer_server_request(&message).await?;
                continue;
            }
            if message.get("id").and_then(Value::as_u64) == Some(compact_request) {
                if let Some(error) = message.get("error") {
                    bail!("codex thread/compact/start failed: {error}");
                }
                continue;
            }
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if params.get("threadId").and_then(Value::as_str).is_some_and(|id| id != thread_id) {
                continue;
            }
            match message.get("method").and_then(Value::as_str).unwrap_or("") {
                "thread/tokenUsage/updated" => {
                    if let Some(total) = thread_usage(&params, thread_id) {
                        usage = total;
                    }
                }
                "error" => {
                    last_error = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .or_else(|| Some(params.to_string()));
                }
                "turn/completed" => {
                    let status = params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("completed");
                    if status != "completed" {
                        let error = params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .or(last_error)
                            .unwrap_or_else(|| format!("turn {status}"));
                        bail!("Codex compaction {status}: {error}");
                    }
                    info!(thread = thread_id, ms = started.elapsed().as_millis() as u64, "Compacted Codex thread");
                    usage.context_tokens = 0;
                    return Ok(CliResult {
                        output: "compacted".to_string(),
                        session_id: Some(thread_id.to_string()),
                        cost_usd: 0.0,
                        exit_code: 0,
                        usage,
                        checkpoint: None,
                    });
                }
                _ => {}
            }
        }
    }

    pub async fn spawn(opts: &CliOptions<'_>) -> Result<Self> {
        let mut cmd = job_command("codex");
        cmd.current_dir(opts.working_dir);
        cmd.kill_on_drop(true);
        configure_child_environment(&mut cmd, &opts.child_environment())?;
        let api_key_login = opts.provider_keys.contains_key("OPENAI_API_KEY");
        let chatgpt_login = super::uses_chatgpt_login(&cmd, api_key_login);
        let shielded = crate::key_proxy::shield_codex(&mut cmd, chatgpt_login);
        cmd.arg("app-server");
        let api_key = opts.provider_keys.get("OPENAI_API_KEY").map(|key| {
            shielded
                .placeholders
                .get("OPENAI_API_KEY")
                .cloned()
                .or_else(|| crate::key_proxy::get().and_then(|proxy| proxy.shield_value("OPENAI_API_KEY", key)))
                .unwrap_or_else(|| key.clone())
        });
        if api_key.is_some() {
            cmd.arg("-c").arg("cli_auth_credentials_store=\"ephemeral\"");
        }
        let openai_base = shielded.openai_base().map(String::from).or_else(|| {
            api_key.as_ref().filter(|key| key.contains("loka-")).and_then(|_| crate::key_proxy::get().map(|proxy| proxy.openai_base()))
        });
        for value in openai_base.as_deref().map(crate::key_proxy::codex_overrides).unwrap_or_default() {
            cmd.arg("-c").arg(value);
        }
        for value in super::shell_environment_overrides(&opts.environment)?.into_iter().chain(shielded.codex_shell_overrides()).chain(super::lean_overrides(opts)) {
            cmd.arg("-c").arg(value);
        }
        if let Some(effort) = opts.effort {
            cmd.arg("-c").arg(format!("model_reasoning_effort=\"{effort}\""));
        }
        if opts.fast {
            cmd.arg("-c").arg("service_tier=\"fast\"");
        }
        super::mcp_args(&mut cmd, opts)?;
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        info!(dir = %opts.working_dir.display(), model = opts.model, "Starting warm Codex app-server");
        let _state = own_cli_state().await;
        let mut child = cmd.spawn().context("Failed to spawn codex app-server")?;
        crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("codex stdin missing"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("codex stdout missing"))?).lines();
        let stderr = stderr_tail(child.stderr.take());
        let mut server = Self { child, stdin, stdout, stderr, secrets: opts.secret_values(), next_id: 0, thread_id: None };
        let initialize =
            server.request("initialize", json!({ "clientInfo": { "name": "loka", "version": env!("CARGO_PKG_VERSION") } }));
        tokio::time::timeout(Duration::from_secs(30), initialize).await.context("codex app-server did not initialize")??;
        server.send(&json!({ "jsonrpc": "2.0", "method": "initialized" })).await?;
        if let Some(api_key) = api_key {
            server.request("account/login/start", json!({ "type": "apiKey", "apiKey": api_key })).await?;
        }
        Ok(server)
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().unwrap().trim().to_string()
    }

    pub async fn shutdown(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_err() {
            if let Err(e) = self.child.kill().await {
                warn!(error = %e, "Could not stop warm Codex app-server");
            }
        }
    }

    async fn open_work_thread(&mut self, opts: &CliOptions<'_>) -> Result<String> {
        let mode = sandbox(opts);
        let mut params = json!({ "model": opts.model, "cwd": opts.working_dir, "sandbox": mode, "approvalPolicy": "never" });
        if mode == "workspace-write" {
            params["approvalPolicy"] = json!("on-request");
            params["approvalsReviewer"] = json!("auto_review");
            let roots = super::writable_roots(opts.working_dir).await;
            params["config"] = json!({ "sandbox_workspace_write": { "writable_roots": roots, "network_access": true } });
        }
        let response = match opts.session_id {
            Some(sid) => {
                params["threadId"] = json!(sid);
                params["excludeTurns"] = json!(true);
                let method = if opts.fork { "thread/fork" } else { "thread/resume" };
                self.request(method, params).await?
            }
            None => self.request("thread/start", params).await?,
        };
        let id = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("codex app-server returned no thread id"))?
            .to_string();
        self.thread_id = Some(id.clone());
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::SessionStarted { session_id: id.clone() });
        }
        Ok(id)
    }

    async fn start_turn(&mut self, opts: &CliOptions<'_>, thread_id: &str, input: Vec<Value>) -> Result<u64> {
        let mut params = json!({ "threadId": thread_id, "input": input, "model": opts.model });
        if let Some(effort) = super::work_effort(opts.effort, opts.lean) {
            params["effort"] = json!(effort);
        }
        self.start_request("turn/start", params).await
    }

    pub async fn work(
        &mut self,
        opts: &CliOptions<'_>,
        started: Instant,
        mut steer: crate::cli::SteerReceiver,
    ) -> Result<(CliResult, Timing)> {
        let thread_id = self.open_work_thread(opts).await?;
        let mut input = vec![json!({ "type": "text", "text": full_prompt(opts) })];
        for path in &opts.image_paths {
            input.push(json!({ "type": "localImage", "path": path }));
        }
        let mut turn_request = self.start_turn(opts, &thread_id, input).await?;

        let interrupt_after = crate::cli::steer_interrupt_after();
        let hang_after = crate::cli::tool_hang_after();
        let mut watchdog = tokio::time::interval(Duration::from_secs(15));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut timing = Timing::default();
        let elapsed = |started: Instant| started.elapsed().as_millis() as u64;
        let mut outputs: Vec<String> = Vec::new();
        let mut answer = String::new();
        let mut final_answer: Option<String> = None;
        let mut usage = Usage::default();
        let mut last_error: Option<String> = None;
        let mut turn_id: Option<String> = None;
        let mut tools = crate::cli::OpenTools::new();
        let mut waiting: Vec<String> = Vec::new();
        let mut steering: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        let mut interrupting = false;
        let mut commands: std::collections::HashMap<String, Instant> = std::collections::HashMap::new();
        let server_pid = self.pid();

        loop {
            tokio::select! {
                message = self.read() => {
                    let message = message?;
                    if message.get("method").is_some() && message.get("id").is_some() {
                        self.answer_server_request(&message).await?;
                        continue;
                    }
                    if let Some(id) = message.get("id").and_then(Value::as_u64) {
                        if id == turn_request {
                            if let Some(error) = message.get("error") {
                                bail!("codex turn/start failed: {error}");
                            }
                        } else if let Some(text) = steering.remove(&id) {
                            if message.get("error").is_some() {
                                waiting.push(text);
                            }
                        }
                        continue;
                    }
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    match message.get("method").and_then(Value::as_str).unwrap_or("") {
                        "turn/started" => {
                            timing.init_ms.get_or_insert(elapsed(started));
                            turn_id = params.pointer("/turn/id").and_then(Value::as_str).map(String::from);
                        }
                        "item/started" => {
                            let item = params.get("item").cloned().unwrap_or(Value::Null);
                            if let Some(id) = item.get("id").and_then(Value::as_str) {
                                if matches!(item.get("type").and_then(Value::as_str), Some("commandExecution" | "mcpToolCall")) {
                                    tools.start(id);
                                }
                                if item.get("type").and_then(Value::as_str) == Some("commandExecution") {
                                    commands.insert(id.to_string(), Instant::now());
                                }
                            }
                            emit_item(opts, &item, false);
                        }
                        "item/completed" => {
                            let item = params.get("item").cloned().unwrap_or(Value::Null);
                            if let Some(id) = item.get("id").and_then(Value::as_str) {
                                tools.finish(id);
                                commands.remove(id);
                            }
                            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                                "agentMessage" => {
                                    let now = elapsed(started);
                                    timing.first_output_ms.get_or_insert(now);
                                    timing.last_output_ms = Some(now);
                                    let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                                    if !text.trim().is_empty() {
                                        answer = text.to_string();
                                        if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                                            final_answer = Some(text.to_string());
                                        }
                                    }
                                }
                                "commandExecution" | "mcpToolCall" | "fileChange" | "webSearch" => timing.tool_calls += 1,
                                _ => {}
                            }
                            emit_item(opts, &item, true);
                        }
                        "thread/tokenUsage/updated" => {
                            if let Some(total) = thread_usage(&params, &thread_id) {
                                usage = total;
                                if let Some(tx) = &opts.event_tx {
                                    let _ = tx.send(StreamEvent::Usage(total));
                                }
                            }
                        }
                        "error" => {
                            last_error = params
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .map(String::from)
                                .or_else(|| Some(params.to_string()));
                        }
                        "turn/completed" => {
                            timing.result_ms = Some(elapsed(started));
                            tools.clear();
                            turn_id = None;
                            let status = params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("completed");
                            let stopped_by_us = status == "interrupted" && interrupting;
                            if status != "completed" && !stopped_by_us {
                                let error = params
                                    .pointer("/turn/error/message")
                                    .and_then(Value::as_str)
                                    .map(String::from)
                                    .or(last_error.take())
                                    .unwrap_or_else(|| format!("turn {status}"));
                                if let Some(tx) = &opts.event_tx {
                                    let _ = tx.send(StreamEvent::Error(error.clone()));
                                }
                                crate::cli::close_steers(&mut steer);
                                bail!("Codex turn {status}: {error}");
                            }
                            let text = final_answer.take().unwrap_or_else(|| std::mem::take(&mut answer));
                            answer.clear();
                            if !text.trim().is_empty() {
                                outputs.push(text);
                            }
                            while let Ok(next) = steer.try_recv() {
                                waiting.push(next.text);
                                let _ = next.ack.send(true);
                            }
                            waiting.extend(steering.drain().map(|(_, text)| text));
                            if waiting.is_empty() {
                                break;
                            }
                            let mut text = std::mem::take(&mut waiting).join("\n\n");
                            if interrupting {
                                text = format!("{}\n\n{text}", crate::cli::INTERRUPTED_NOTE);
                            }
                            interrupting = false;
                            turn_request = self.start_turn(opts, &thread_id, vec![json!({ "type": "text", "text": text })]).await?;
                        }
                        _ => {}
                    }
                }
                Some(next) = steer.recv() => {
                    let interrupt = next.interrupt || tools.stuck_longer_than(interrupt_after);
                    match turn_id.clone() {
                        Some(active) if interrupt => {
                            waiting.push(next.text);
                            if !interrupting {
                                interrupting = true;
                                self.start_request("turn/interrupt", json!({ "threadId": thread_id, "turnId": active })).await?;
                                stop_commands(server_pid, &mut commands);
                            }
                        }
                        Some(active) => {
                            let id = self
                                .start_request(
                                    "turn/steer",
                                    json!({ "threadId": thread_id, "expectedTurnId": active, "input": [{ "type": "text", "text": next.text }] }),
                                )
                                .await?;
                            steering.insert(id, next.text);
                        }
                        None => waiting.push(next.text),
                    }
                    let _ = next.ack.send(true);
                    info!(interrupt, "Steered running Codex session");
                }
                _ = watchdog.tick() => {
                    if let (Some(limit), Some(age), Some(active)) = (hang_after, tools.oldest(), turn_id.clone()) {
                        if age >= limit && !interrupting {
                            warn!(secs = age.as_secs(), "Codex tool call looks stuck, interrupting");
                            waiting.push(crate::cli::hang_notice(age));
                            interrupting = true;
                            self.start_request("turn/interrupt", json!({ "threadId": thread_id, "turnId": active })).await?;
                            stop_commands(server_pid, &mut commands);
                        }
                    }
                }
            }
        }
        crate::cli::close_steers(&mut steer);
        timing.model_turns = Some(outputs.len() as u64);
        Ok((
            CliResult {
                output: outputs.join("\n\n"),
                session_id: Some(thread_id),
                cost_usd: 0.0,
                exit_code: 0,
                usage,
                checkpoint: None,
            },
            timing,
        ))
    }

    pub async fn ask(&mut self, opts: &CliOptions<'_>, started: Instant) -> Result<(CliResult, Timing)> {
        let prompt = full_prompt(opts);
        let thread_id = match self.thread_id.clone() {
            Some(id) => id,
            None => {
                let mut params = json!({ "model": opts.model, "cwd": opts.working_dir, "sandbox": sandbox(opts), "approvalPolicy": "never" });
                let response = match opts.session_id {
                    Some(sid) => {
                        params["threadId"] = json!(sid);
                        params["excludeTurns"] = json!(true);
                        self.request("thread/resume", params).await?
                    }
                    None => self.request("thread/start", params).await?,
                };
                let id = response
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("codex app-server returned no thread id"))?
                    .to_string();
                self.thread_id = Some(id.clone());
                id
            }
        };

        let mut input = vec![json!({ "type": "text", "text": prompt })];
        for path in &opts.image_paths {
            input.push(json!({ "type": "localImage", "path": path }));
        }
        let mut params = json!({ "threadId": thread_id, "input": input, "model": opts.model });
        if let Some(effort) = opts.effort {
            params["effort"] = json!(effort);
        }
        let turn_request = self.start_request("turn/start", params).await?;

        let mut timing = Timing::default();
        let elapsed = |started: Instant| started.elapsed().as_millis() as u64;
        let mut answer = String::new();
        let mut final_answer: Option<String> = None;
        let mut usage = Usage::default();
        let mut last_error: Option<String> = None;
        let mut model_turns = 0u64;
        let mut turn_id: Option<String> = None;
        loop {
            let message = self.read().await?;
            if message.get("method").is_some() && message.get("id").is_some() {
                self.answer_server_request(&message).await?;
                continue;
            }
            if message.get("id").and_then(Value::as_u64) == Some(turn_request) {
                if let Some(error) = message.get("error") {
                    bail!("codex turn/start failed: {error}");
                }
                continue;
            }
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match message.get("method").and_then(Value::as_str).unwrap_or("") {
                "turn/started" => {
                    timing.init_ms.get_or_insert(elapsed(started));
                    turn_id = params.pointer("/turn/id").and_then(Value::as_str).map(String::from);
                }
                "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" => {
                    timing.first_output_ms.get_or_insert(elapsed(started));
                }
                "item/completed" => {
                    let item = params.get("item").cloned().unwrap_or(Value::Null);
                    match item.get("type").and_then(Value::as_str).unwrap_or("") {
                        "agentMessage" => {
                            let now = elapsed(started);
                            timing.first_output_ms.get_or_insert(now);
                            timing.last_output_ms = Some(now);
                            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.trim().is_empty() {
                                answer = text.to_string();
                                if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                                    final_answer = Some(text.to_string());
                                }
                                if let Some(tx) = &opts.event_tx {
                                    let _ = tx.send(StreamEvent::AssistantText(text.trim().chars().take(300).collect()));
                                }
                            }
                        }
                        "mcpToolCall" | "commandExecution" | "fileChange" | "webSearch" => {
                            timing.tool_calls += 1;
                            if let Some(tx) = &opts.event_tx {
                                let tool = match (item.get("server").and_then(Value::as_str), item.get("tool").and_then(Value::as_str)) {
                                    (Some(server), Some(tool)) => format!("{server}/{tool}"),
                                    _ => item.get("type").and_then(Value::as_str).unwrap_or("tool").to_string(),
                                };
                                let _ = tx.send(StreamEvent::ToolUse { tool, input_summary: String::new() });
                            }
                        }
                        _ => {}
                    }
                }
                "thread/tokenUsage/updated" => {
                    if turn_id.is_some() && params.get("turnId").and_then(Value::as_str) == turn_id.as_deref() {
                        model_turns += 1;
                    }
                    if let Some(total) = thread_usage(&params, &thread_id) {
                        usage = total;
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx.send(StreamEvent::Usage(total));
                        }
                    }
                }
                "error" => {
                    last_error = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .or_else(|| Some(params.to_string()));
                }
                "turn/completed" => {
                    timing.result_ms = Some(elapsed(started));
                    timing.model_turns = Some(model_turns);
                    let status = params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("completed");
                    if status != "completed" {
                        let error = params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .or(last_error)
                            .unwrap_or_else(|| format!("turn {status}"));
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx.send(StreamEvent::Error(error.clone()));
                        }
                        bail!("Codex turn {status}: {error}");
                    }
                    let output = final_answer.unwrap_or(answer);
                    return Ok((
                        CliResult {
                            output,
                            session_id: Some(thread_id),
                            cost_usd: 0.0,
                            exit_code: 0,
                            usage,
                            checkpoint: None,
                        },
                        timing,
                    ));
                }
                _ => {}
            }
        }
    }

    async fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.context("codex app-server stdin closed")?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        loop {
            let Some(line) = self.stdout.next_line().await? else {
                let status = match tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await {
                    Ok(Ok(status)) => status.to_string(),
                    _ => "unknown status".to_string(),
                };
                let stderr = crate::remote_runner::redact(&self.stderr_tail(), &self.secrets);
                warn!(%status, %stderr, "Codex app-server exited");
                bail!("codex app-server exited with {status}");
            };
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                return Ok(value);
            }
        }
    }

    async fn start_request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        Ok(id)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.start_request(method, params).await?;
        loop {
            let message = self.read().await?;
            if message.get("method").is_some() {
                if message.get("id").is_some() {
                    self.answer_server_request(&message).await?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("codex {method} failed: {}", crate::remote_runner::redact(&error.to_string(), &self.secrets));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn answer_server_request(&mut self, message: &Value) -> Result<()> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => json!({ "decision": "decline" }),
            "execCommandApproval" | "applyPatchApproval" => json!({ "decision": "denied" }),
            "mcpServer/elicitation/request" => json!({ "action": "decline" }),
            "item/permissions/requestApproval" => json!({ "permissions": {} }),
            "item/tool/requestUserInput" => json!({ "answers": {} }),
            _ => {
                warn!(method, "Unsupported codex app-server request");
                return self
                    .send(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} is not supported") } }))
                    .await;
            }
        };
        warn!(method, "Declined codex app-server request");
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result })).await
    }
}

fn stop_commands(server_pid: Option<u32>, commands: &mut std::collections::HashMap<String, Instant>) {
    let oldest = commands.drain().map(|(_, at)| at.elapsed()).max();
    if let (Some(root), Some(age)) = (server_pid, oldest) {
        tokio::spawn(crate::cli::kill_recent_descendants(
            root,
            age + Duration::from_secs(2),
            &["codex", "node_repl", "cua-repl", "loka-agent mcp"],
        ));
    }
}

fn emit_item(opts: &CliOptions<'_>, item: &Value, completed: bool) {
    let Some(tx) = &opts.event_tx else {
        return;
    };
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
    let mcp_tool = || format!("{}/{}", item.get("server").and_then(Value::as_str).unwrap_or("mcp"), text("tool"));
    match (text("type"), completed) {
        ("commandExecution", false) => {
            let _ = tx.send(StreamEvent::ToolUse {
                tool: "Bash".to_string(),
                input_summary: text("command").chars().take(120).collect(),
            });
        }
        ("commandExecution", true) => {
            let output = text("aggregatedOutput").trim();
            if !output.is_empty() {
                let _ = tx.send(StreamEvent::ToolResult {
                    tool: "Bash".to_string(),
                    output: crate::cli::truncate_for_log(output, crate::cli::TOOL_OUTPUT_MAX_CHARS),
                });
            }
        }
        ("mcpToolCall", false) => {
            let _ = tx.send(StreamEvent::ToolUse { tool: mcp_tool(), input_summary: String::new() });
        }
        ("mcpToolCall", true) => {
            let output = item.get("result").map(crate::cli::summarize_tool_output).unwrap_or_default();
            if !output.trim().is_empty() {
                let _ = tx.send(StreamEvent::ToolResult {
                    tool: mcp_tool(),
                    output: crate::cli::truncate_for_log(output.trim(), crate::cli::TOOL_OUTPUT_MAX_CHARS),
                });
            }
        }
        ("fileChange", true) => {
            for change in item.get("changes").and_then(Value::as_array).into_iter().flatten() {
                let kind = change
                    .get("kind")
                    .and_then(|k| k.as_str().or_else(|| k.get("type").and_then(Value::as_str)))
                    .unwrap_or("update");
                let tool = match kind {
                    "add" => "Write",
                    "delete" => "Delete",
                    _ => "Edit",
                };
                let _ = tx.send(StreamEvent::ToolUse {
                    tool: tool.to_string(),
                    input_summary: change.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                });
            }
        }
        ("agentMessage", true) => {
            let message = text("text").trim();
            if !message.is_empty() {
                let _ = tx.send(StreamEvent::AssistantText(message.chars().take(300).collect()));
            }
        }
        _ => {}
    }
}

fn sandbox(opts: &CliOptions<'_>) -> &'static str {
    sandbox_of(opts)
}

pub(crate) fn sandbox_of(opts: &CliOptions<'_>) -> &'static str {
    sandbox_for(opts.lean, opts.approval, crate::claude::sandbox::mode())
}

pub(crate) fn sandbox_for(lean: bool, approval: Approval, mode: crate::claude::sandbox::Mode) -> &'static str {
    match (lean, approval) {
        (true, _) | (false, Approval::ReadOnly) => "read-only",
        _ if mode == crate::claude::sandbox::Mode::Off => "danger-full-access",
        (false, Approval::Auto) => "workspace-write",
        (false, Approval::Full) => "danger-full-access",
    }
}

fn thread_usage(params: &Value, thread_id: &str) -> Option<Usage> {
    if params.get("threadId").and_then(Value::as_str).is_some_and(|id| id != thread_id) {
        return None;
    }
    let total = params.pointer("/tokenUsage/total")?;
    let n = |k: &str| total.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input_tokens: n("inputTokens").saturating_sub(n("cachedInputTokens")),
        cached_input_tokens: n("cachedInputTokens"),
        output_tokens: n("outputTokens"),
        context_tokens: params.pointer("/tokenUsage/last").map(super::context_of).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::thread_usage;
    use serde_json::json;

    fn update(thread: &str, input: u64, cached: u64, output: u64) -> serde_json::Value {
        json!({
            "threadId": thread,
            "turnId": "turn",
            "tokenUsage": {
                "total": { "totalTokens": input + output, "inputTokens": input, "cachedInputTokens": cached, "outputTokens": output },
                "last": { "totalTokens": 10, "inputTokens": 9, "cachedInputTokens": 0, "outputTokens": 1 },
            },
        })
    }

    #[test]
    fn thread_usage_reads_cumulative_totals_like_exec() {
        let usage = thread_usage(&update("fork", 65857, 31872, 26), "fork").unwrap();
        assert_eq!(usage.input_tokens, 65857 - 31872);
        assert_eq!(usage.cached_input_tokens, 31872);
        assert_eq!(usage.output_tokens, 26);
        assert_eq!(usage.context_tokens, 10);
    }

    #[test]
    fn context_comes_from_the_last_call_in_either_spelling() {
        assert_eq!(super::super::context_of(&json!({ "totalTokens": 16947, "inputTokens": 16942 })), 16947);
        assert_eq!(super::super::context_of(&json!({ "input_tokens": 16942, "output_tokens": 5 })), 16947);
    }

    fn options<'a>(dir: &'a std::path::Path, model: &'a str, prompt: &'a str, parent: Option<&'a str>) -> crate::cli::CliOptions<'a> {
        crate::cli::CliOptions {
            working_dir: dir,
            prompt,
            system_prompt: None,
            allowed_tools: None,
            max_turns: 5,
            model,
            mcp_config_json: None,
            session_id: parent,
            resume: parent.is_some(),
            event_tx: None,
            image_paths: Vec::new(),
            fork: parent.is_some(),
            lean: false,
            effort: Some("low"),
            fast: false,
            environment: Default::default(),
            mcp_has_secrets: false,
            sensitive: false,
            rewind_to: None,
            approval: Default::default(),
            steer: crate::cli::SteerSlot::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files: Vec::new(),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_task_fork_usage_matches_exec() {
        let Ok(model) = std::env::var("CODEX_TEST_MODEL") else { return };
        let dir = std::env::temp_dir();
        let parent = crate::cli::run("codex", options(&dir, &model, "Reply with the single word ready.", None)).await.unwrap();
        let parent_id = parent.session_id.clone().unwrap();
        let baseline = json!({
            "input_tokens": parent.usage.input_tokens,
            "cached_input_tokens": parent.usage.cached_input_tokens,
            "output_tokens": parent.usage.output_tokens,
        });
        let total = |u: crate::cli::Usage| loka_shared::runners::RunnerUsage {
            input_tokens: u.input_tokens,
            cached_input_tokens: u.cached_input_tokens,
            output_tokens: u.output_tokens,
            context_tokens: 0,
        };
        let prompt = "Run `echo forked` in the shell, then reply with its output.";
        let exec = crate::cli::run("codex", options(&dir, &model, prompt, Some(&parent_id))).await.unwrap();
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut steerable = options(&dir, &model, prompt, Some(&parent_id));
        steerable.steer = crate::cli::SteerSlot::new(receiver);
        let app = crate::cli::run("codex", steerable).await.unwrap();
        let exec_delta = total(exec.usage).since(&baseline);
        let app_delta = total(app.usage).since(&baseline);
        eprintln!("parent {:?}\nexec fork {:?} -> {exec_delta:?}\napp-server fork {:?} -> {app_delta:?}", parent.usage, exec.usage, app.usage);
        assert_ne!(app.session_id.as_deref(), Some(parent_id.as_str()));
        assert!(app.usage.input_tokens + app.usage.cached_input_tokens > parent.usage.input_tokens + parent.usage.cached_input_tokens);
        assert!(app_delta.output_tokens > 0 && app_delta.input_tokens + app_delta.cached_input_tokens > 0);
        let (a, e) = (app_delta.input_tokens + app_delta.cached_input_tokens, exec_delta.input_tokens + exec_delta.cached_input_tokens);
        assert!(a * 2 > e && e * 2 > a, "app-server delta {a} and exec delta {e} should be the same order");
    }

    #[test]
    fn thread_usage_ignores_other_threads() {
        assert!(thread_usage(&update("subagent", 900, 0, 5), "task").is_none());
        let unlabelled = json!({ "tokenUsage": { "total": { "inputTokens": 40, "cachedInputTokens": 10, "outputTokens": 2 } } });
        assert_eq!(thread_usage(&unlabelled, "task").unwrap().input_tokens, 30);
        assert!(thread_usage(&json!({ "threadId": "task" }), "task").is_none());
    }
}
