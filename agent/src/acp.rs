use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{info, warn};

use crate::cli::{stderr_tail, CliOptions, CliResult, StreamEvent, Timing, Usage};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

static UNOFFERED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Debug)]
pub struct UnofferedModel(pub String);

impl std::fmt::Display for UnofferedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The ACP server does not offer model {}", self.0)
    }
}

impl std::error::Error for UnofferedModel {}

pub fn offers(model: &str) -> bool {
    !UNOFFERED.lock().unwrap().iter().any(|m| m == model)
}

pub fn is_unoffered(error: &anyhow::Error) -> bool {
    error.chain().any(|e| e.is::<UnofferedModel>())
}

fn unoffered(model: &str) -> anyhow::Error {
    let mut seen = UNOFFERED.lock().unwrap();
    if !seen.iter().any(|m| m == model) {
        warn!(model, "ACP server does not offer this model, its turns run without a warm session");
        seen.push(model.to_string());
    }
    anyhow::Error::new(UnofferedModel(model.to_string()))
}

pub struct Setup {
    pub command: Command,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub mcp_servers: Value,
    pub keep: Vec<Box<dyn Send>>,
}

pub struct AcpProc {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    session: String,
    cost_seen: f64,
    lean: bool,
    _keep: Vec<Box<dyn Send>>,
}

#[derive(Default)]
struct TurnState {
    texts: Vec<(String, String)>,
    last_message: Option<String>,
    context: u64,
    cost: Option<f64>,
    tools: u32,
    first_output_ms: Option<u64>,
    last_output_ms: Option<u64>,
}

impl TurnState {
    fn output(&self) -> String {
        let Some((last, _)) = self.texts.last() else {
            return String::new();
        };
        self.texts.iter().filter(|(id, _)| id == last).map(|(_, t)| t.as_str()).collect::<String>()
    }
}

pub fn mcp_servers(mcp_json: Option<&str>) -> Value {
    let servers = mcp_json
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    let pairs = |value: Option<&Value>| -> Vec<Value> {
        value
            .and_then(Value::as_object)
            .map(|map| map.iter().map(|(name, value)| json!({ "name": name, "value": value.as_str().unwrap_or_default() })).collect())
            .unwrap_or_default()
    };
    let list: Vec<Value> = servers
        .iter()
        .filter_map(|(name, server)| {
            if let Some(url) = server.get("url").and_then(Value::as_str) {
                let kind = if server.get("type").and_then(Value::as_str) == Some("sse") { "sse" } else { "http" };
                return Some(json!({ "type": kind, "name": name, "url": url, "headers": pairs(server.get("headers")) }));
            }
            let command = server.get("command").and_then(Value::as_str)?;
            Some(json!({
                "name": name,
                "command": command,
                "args": server.get("args").cloned().unwrap_or_else(|| json!([])),
                "env": pairs(server.get("env")),
            }))
        })
        .collect();
    Value::Array(list)
}

fn offered_model(opened: &Value, wanted: &str) -> Option<String> {
    let listed = opened
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("modelId").and_then(Value::as_str))
        .chain(
            opened
                .get("configOptions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|o| o.get("id").and_then(Value::as_str) == Some("model"))
                .flat_map(|o| o.get("options").and_then(Value::as_array).into_iter().flatten())
                .filter_map(|o| o.get("value").and_then(Value::as_str)),
        )
        .collect::<Vec<_>>();
    if listed.is_empty() {
        return Some(wanted.to_string());
    }
    let base = |id: &str| id.split('[').next().unwrap_or(id).to_string();
    listed
        .iter()
        .find(|id| **id == wanted)
        .or_else(|| (!wanted.contains('[')).then(|| listed.iter().find(|id| base(id) == wanted)).flatten())
        .map(|id| id.to_string())
}

fn permission_answer(request: &Value, lean: bool) -> Value {
    let kind = request.pointer("/toolCall/kind").and_then(Value::as_str).unwrap_or("other");
    let allowed = !lean || matches!(kind, "other" | "read" | "search" | "think" | "fetch");
    let options = request.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
    let wanted = if allowed { ["allow_once", "allow_always"] } else { ["reject_once", "reject_always"] };
    let option = wanted
        .iter()
        .find_map(|w| options.iter().find(|o| o.get("kind").and_then(Value::as_str) == Some(w)))
        .and_then(|o| o.get("optionId").cloned());
    match option {
        Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

impl AcpProc {
    pub async fn spawn(setup: Setup, opts: &CliOptions<'_>) -> Result<Self> {
        let Setup { mut command, mode, model, mcp_servers, keep } = setup;
        command
            .current_dir(opts.working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().context("Failed to start the ACP server")?;
        crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);
        let stdin = child.stdin.take().context("ACP stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("ACP stdout missing")?).lines();
        let stderr = stderr_tail(child.stderr.take(), opts.redactor());
        let mut proc = Self { child, stdin, stdout, stderr, next_id: 0, session: String::new(), cost_seen: 0.0, lean: opts.lean, _keep: keep };
        proc.request("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} }), &mut TurnState::default(), None).await?;
        let cwd = opts.working_dir.display().to_string();
        let opened = match opts.session_id.filter(|_| opts.resume) {
            Some(sid) => {
                let mut loaded = proc.request("session/load", json!({ "sessionId": sid, "cwd": cwd, "mcpServers": mcp_servers }), &mut TurnState::default(), None).await?;
                if !loaded.is_object() {
                    loaded = json!({});
                }
                loaded["sessionId"] = json!(sid);
                loaded
            }
            None => proc.request("session/new", json!({ "cwd": cwd, "mcpServers": mcp_servers }), &mut TurnState::default(), None).await?,
        };
        proc.session = opened.get("sessionId").and_then(Value::as_str).context("ACP server returned no session id")?.to_string();
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::SessionStarted { session_id: proc.session.clone() });
        }
        if let Some(mode) = mode {
            proc.configure("mode", &mode).await?;
        }
        if let Some(model) = model {
            let Some(offered) = offered_model(&opened, &model) else {
                return Err(unoffered(&model));
            };
            if let Err(e) = proc.configure("model", &offered).await {
                warn!(error = %format!("{e:#}"), "ACP server refused the model");
                return Err(unoffered(&model));
            }
        }
        info!(session = %proc.session, "Started warm ACP session");
        Ok(proc)
    }

    async fn configure(&mut self, option: &str, value: &str) -> Result<()> {
        let session = self.session.clone();
        let set = self
            .request("session/set_config_option", json!({ "sessionId": session, "configId": option, "value": value }), &mut TurnState::default(), None)
            .await;
        if set.is_ok() {
            return Ok(());
        }
        let (method, key) = if option == "mode" { ("session/set_mode", "modeId") } else { ("session/set_model", "modelId") };
        self.request(method, json!({ "sessionId": session, key: value }), &mut TurnState::default(), None)
            .await
            .with_context(|| format!("The ACP server refused {option} {value}"))
            .map(|_| ())
    }

    pub fn session(&self) -> &str {
        &self.session
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

    async fn send(&mut self, message: Value) -> Result<()> {
        self.stdin.write_all(format!("{message}\n").as_bytes()).await.context("ACP stdin closed")?;
        self.stdin.flush().await.context("ACP stdin closed")
    }

    async fn request(&mut self, method: &str, params: Value, turn: &mut TurnState, events: Option<(&crate::cli::EventSender, Instant)>) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let deadline = Instant::now() + if method == "session/prompt" { Duration::from_secs(3600) } else { REQUEST_TIMEOUT };
        loop {
            let line = tokio::time::timeout_at(deadline.into(), self.stdout.next_line())
                .await
                .map_err(|_| anyhow!("ACP {method} timed out"))??
                .ok_or_else(|| anyhow!("ACP server exited during {method}"))?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let has_method = message.get("method").and_then(Value::as_str);
            match (message.get("id"), has_method) {
                (Some(reply), None) if reply.as_u64() == Some(id) => {
                    if let Some(error) = message.get("error") {
                        let text = error.pointer("/data/message").or_else(|| error.pointer("/data/details")).or_else(|| error.get("message")).and_then(Value::as_str).unwrap_or("unknown error");
                        anyhow::bail!("ACP {method} failed: {text}");
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                (Some(request), Some(asked)) => {
                    let request = request.clone();
                    let reply = if asked == "session/request_permission" {
                        json!({ "jsonrpc": "2.0", "id": request, "result": permission_answer(message.get("params").unwrap_or(&Value::Null), self.lean) })
                    } else {
                        json!({ "jsonrpc": "2.0", "id": request, "error": { "code": -32601, "message": "not supported by this client" } })
                    };
                    self.send(reply).await?;
                }
                (None, Some("session/update")) => {
                    if let Some(update) = message.pointer("/params/update") {
                        observe(update, turn, events);
                    }
                }
                _ => {}
            }
        }
    }

    pub async fn ask(&mut self, opts: &CliOptions<'_>, started: Instant) -> Result<(CliResult, Timing)> {
        let mut prompt = vec![json!({ "type": "text", "text": opts.prompt })];
        for path in &opts.image_paths {
            let bytes = tokio::fs::read(path).await.with_context(|| format!("Failed to read image {}", path.display()))?;
            let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                _ => "image/png",
            };
            use base64::Engine;
            prompt.push(json!({ "type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
        }
        let mut turn = TurnState::default();
        let session = self.session.clone();
        let events = opts.event_tx.as_ref().map(|tx| (tx, started));
        let result = self.request("session/prompt", json!({ "sessionId": session, "prompt": prompt }), &mut turn, events).await?;
        let stop = result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn");
        let output = turn.output();
        if stop == "refusal" && output.is_empty() {
            anyhow::bail!("the model refused to answer");
        }
        let n = |key: &str| result.pointer(&format!("/usage/{key}")).and_then(Value::as_u64).unwrap_or(0);
        let mut usage = Usage {
            input_tokens: n("inputTokens") + n("cachedWriteTokens"),
            cached_input_tokens: n("cachedReadTokens"),
            output_tokens: n("outputTokens") + n("thoughtTokens"),
            context_tokens: turn.context,
        };
        if usage.input_tokens + usage.cached_input_tokens + usage.output_tokens == 0 {
            usage = estimated_usage(opts.system_prompt.unwrap_or_default(), opts.prompt, &output, turn.context);
        }
        let cost_usd = match turn.cost {
            Some(total) => {
                let spent = if total >= self.cost_seen { total - self.cost_seen } else { total };
                self.cost_seen = total;
                spent
            }
            None => 0.0,
        };
        let timing = Timing {
            first_output_ms: turn.first_output_ms,
            last_output_ms: turn.last_output_ms,
            result_ms: Some(started.elapsed().as_millis() as u64),
            tool_calls: turn.tools,
            ..Timing::default()
        };
        Ok((
            CliResult {
                output,
                session_id: Some(self.session.clone()),
                cost_usd,
                exit_code: 0,
                usage,
                checkpoint: turn.last_message,
            },
            timing,
        ))
    }

    pub async fn shutdown(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_err() {
            if let Err(error) = self.child.kill().await {
                warn!(%error, "Could not stop the warm ACP server");
            }
        }
    }
}

fn observe(update: &Value, turn: &mut TurnState, events: Option<(&crate::cli::EventSender, Instant)>) {
    let text = |p: &str| update.pointer(p).and_then(Value::as_str).unwrap_or_default().to_string();
    match update.get("sessionUpdate").and_then(Value::as_str).unwrap_or_default() {
        "agent_message_chunk" => {
            let chunk = text("/content/text");
            if chunk.is_empty() {
                return;
            }
            let message = text("/messageId");
            if !message.is_empty() {
                turn.last_message = Some(message.clone());
            }
            if let Some((_, started)) = events {
                let at = started.elapsed().as_millis() as u64;
                turn.first_output_ms.get_or_insert(at);
                turn.last_output_ms = Some(at);
            }
            match turn.texts.last_mut() {
                Some((id, body)) if *id == message => body.push_str(&chunk),
                _ => turn.texts.push((message, chunk)),
            }
            if let Some((tx, _)) = events {
                let _ = tx.send(StreamEvent::AssistantText(turn.output().trim().to_string()));
            }
        }
        "tool_call" => {
            turn.tools += 1;
            if let Some((tx, _)) = events {
                let tool = text("/title");
                let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
                let summary = crate::cli::summarize_tool_input(&tool, &input);
                let _ = tx.send(StreamEvent::ToolUse { tool, input_summary: summary });
            }
        }
        "tool_call_update" => {
            if let Some((tx, _)) = events {
                let Some(output) = update.get("rawOutput").or_else(|| update.get("content")) else {
                    return;
                };
                let output = crate::cli::summarize_tool_output(output).trim().to_string();
                if !output.is_empty() {
                    let _ = tx.send(StreamEvent::ToolResult { tool: text("/title"), output });
                }
            }
        }
        "usage_update" => {
            if let Some(used) = update.get("used").and_then(Value::as_u64) {
                turn.context = used;
            }
            if let Some(amount) = update.pointer("/cost/amount").and_then(Value::as_f64) {
                turn.cost = Some(amount);
            }
        }
        _ => {}
    }
}

fn estimated_usage(system: &str, prompt: &str, output: &str, context: u64) -> Usage {
    let tokens = |text: &str| (text.chars().count() as u64).div_ceil(4);
    let fresh = tokens(system) + tokens(prompt);
    Usage {
        input_tokens: fresh,
        cached_input_tokens: context.saturating_sub(fresh),
        output_tokens: tokens(output),
        context_tokens: context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_warm_session_the_acp_server_cannot_load_is_a_lost_session() {
        let id = "5d1e0c3a-aaaa-4bbb-8ccc-dddddddddddd";
        for said in [format!("Session \\\"{id}\\\" not found"), format!("Session {id} not found")] {
            let script = format!(
                "read line\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}'\nread line\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{{\"code\":-32602,\"message\":\"{said}\"}}}}'\nsleep 1"
            );
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(script);
            let setup = Setup { command, mode: None, model: None, mcp_servers: json!([]), keep: Vec::new() };
            let dir = std::env::temp_dir();
            let opts = crate::omp::tests::options(&dir, "go", "", Some(id), false);
            let Err(error) = AcpProc::spawn(setup, &opts).await else {
                panic!("a session the server cannot load never starts");
            };
            let text = loka_shared::cli_errors::chain(&error);
            assert!(text.starts_with("ACP session/load failed: Session "), "{text}");
            assert!(loka_shared::cli_errors::is_session_missing(&text), "{text}");
            assert_eq!(loka_shared::cli_errors::lost_session_id(&text).as_deref(), Some(id), "{text}");
        }
    }

    #[test]
    fn estimates_usage_when_the_server_reports_none() {
        let usage = estimated_usage("abcd", "abcdefgh", "ok", 0);
        assert_eq!((usage.input_tokens, usage.cached_input_tokens, usage.output_tokens), (3, 0, 1));
        let usage = estimated_usage("abcd", "abcd", "", 100);
        assert_eq!((usage.input_tokens, usage.cached_input_tokens), (2, 98));
    }

    #[test]
    fn converts_mcp_servers_to_acp() {
        let list = mcp_servers(Some(r#"{"mcpServers":{"world":{"command":"/bin/k","args":["mcp"],"env":{"A":"1"}},"docs":{"type":"http","url":"https://d","headers":{"H":"v"}}}}"#));
        let list = list.as_array().unwrap();
        let world = list.iter().find(|s| s["name"] == "world").unwrap();
        assert_eq!(world["command"], "/bin/k");
        assert_eq!(world["env"], json!([{ "name": "A", "value": "1" }]));
        let docs = list.iter().find(|s| s["name"] == "docs").unwrap();
        assert_eq!(docs["type"], "http");
        assert_eq!(docs["headers"], json!([{ "name": "H", "value": "v" }]));
        assert_eq!(mcp_servers(None), json!([]));
    }

    #[test]
    fn lean_sessions_only_allow_tools_that_cannot_change_anything() {
        let options = json!([{ "optionId": "a", "kind": "allow_once" }, { "optionId": "r", "kind": "reject_once" }]);
        let ask = |kind: &str, lean: bool| permission_answer(&json!({ "toolCall": { "kind": kind }, "options": options }), lean)["outcome"]["optionId"].clone();
        assert_eq!(ask("other", true), "a");
        assert_eq!(ask("execute", true), "r");
        assert_eq!(ask("edit", true), "r");
        assert_eq!(ask("execute", false), "a");
    }

    #[test]
    fn picks_the_offered_variant_of_a_model() {
        let opened = json!({ "models": { "availableModels": [{ "modelId": "composer-2.5[fast=true]" }, { "modelId": "gpt-5.2" }] } });
        assert_eq!(offered_model(&opened, "composer-2.5").as_deref(), Some("composer-2.5[fast=true]"));
        assert_eq!(offered_model(&opened, "gpt-5.2").as_deref(), Some("gpt-5.2"));
        assert_eq!(offered_model(&json!({}), "x").as_deref(), Some("x"));
        assert_eq!(offered_model(&opened, "claude-opus-5-5-high"), None);
        assert_eq!(offered_model(&opened, "composer-2.5[fast=false]"), None);
    }

    #[test]
    fn remembers_models_the_server_does_not_offer() {
        let error = unoffered("claude-opus-5-5-high-test");
        assert!(is_unoffered(&error.context("warm turn")));
        assert!(!offers("claude-opus-5-5-high-test"));
        assert!(offers("composer-2.5"));
    }

    #[test]
    fn keeps_the_last_message_text() {
        let mut turn = TurnState::default();
        for (id, text) in [("m1", "thinking "), ("m2", "po"), ("m2", "ng")] {
            observe(&json!({ "sessionUpdate": "agent_message_chunk", "messageId": id, "content": { "type": "text", "text": text } }), &mut turn, None);
        }
        observe(&json!({ "sessionUpdate": "usage_update", "used": 900, "cost": { "amount": 0.5 } }), &mut turn, None);
        assert_eq!(turn.output(), "pong");
        assert_eq!(turn.last_message.as_deref(), Some("m2"));
        assert_eq!((turn.context, turn.cost), (900, Some(0.5)));
    }
}

#[cfg(test)]
mod redaction_tests {
    use crate::cli::straddle::{around, assert_whole_then_hidden, drain, pads};
    use serde_json::json;
    use std::time::Instant;

    #[test]
    fn the_acp_parser_sends_whole_text_so_a_secret_across_any_cut_is_hidden() {
        for pad in pads() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut turn = super::TurnState::default();
            let events = Some((&tx, Instant::now()));
            super::observe(&json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"text": around(pad)}}), &mut turn, events);
            super::observe(&json!({"sessionUpdate": "tool_call", "title": "Bash", "rawInput": {"command": around(pad)}}), &mut turn, events);
            super::observe(&json!({"sessionUpdate": "tool_call_update", "title": "Bash", "rawOutput": around(pad)}), &mut turn, events);
            drop(tx);
            assert_whole_then_hidden(drain(rx), 3, "acp", pad);
        }
    }
}
