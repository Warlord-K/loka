use std::collections::{BTreeMap, HashSet};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

const REFRESH_EVERY: Duration = Duration::from_secs(30 * 60);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(45);
const BACKENDS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("omp", "omp"),
    ("opencode", "opencode"),
    ("cursor", "cursor-agent"),
    ("grok", "grok"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

fn cache() -> &'static Mutex<Map<String, Value>> {
    static CACHE: OnceLock<Mutex<Map<String, Value>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Map::new()))
}

pub fn snapshot() -> Value {
    Value::Object(cache().lock().unwrap().clone())
}

pub fn spawn_refresh() {
    tokio::spawn(async {
        loop {
            refresh().await;
            tokio::time::sleep(REFRESH_EVERY).await;
        }
    });
}

async fn refresh() {
    let mut found = Map::new();
    for (backend, bin) in BACKENDS {
        if !loka_shared::backends::enabled_here(backend) || !installed(backend, bin) {
            continue;
        }
        match tokio::time::timeout(DISCOVERY_TIMEOUT, discover(backend)).await {
            Ok(Ok(models)) if !models.is_empty() => {
                found.insert(
                    backend.to_string(),
                    Value::Array(models.into_iter().map(|m| json!([m.id, m.label])).collect()),
                );
            }
            Ok(Ok(_)) => debug!(backend, "CLI reported no models"),
            Ok(Err(error)) => warn!(
                backend,
                error = %crate::remote_runner::redact(&format!("{error:#}"), crate::cli::host_secrets()),
                "Model discovery failed"
            ),
            Err(_) => debug!(backend, "Model discovery timed out"),
        }
    }
    let mut current = cache().lock().unwrap();
    for (backend, models) in found {
        if current.get(&backend) != Some(&models) {
            info!(backend = %backend, count = models.as_array().map_or(0, Vec::len), "Discovered CLI models");
        }
        current.insert(backend, models);
    }
}

fn installed(backend: &str, bin: &str) -> bool {
    if backend == "claude" {
        crate::claude::detect::installed()
    } else {
        command_exists(bin)
    }
}

pub fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
}

pub async fn discover(backend: &str) -> Result<Vec<ModelOption>> {
    match backend {
        "claude" => claude().await,
        "codex" => codex().await,
        "omp" => omp().await,
        "opencode" => opencode().await,
        "cursor" => cursor().await,
        "grok" => grok().await,
        other => Err(anyhow!("no model discovery for {other}")),
    }
}

fn command(bin: impl AsRef<std::ffi::OsStr>) -> Result<Command> {
    let mut cmd = crate::cli::job_command(bin);
    crate::cli::configure_child_environment(&mut cmd, &BTreeMap::new())?;
    cmd.kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(cmd)
}

async fn output(mut cmd: Command) -> Result<String> {
    let mut child = cmd.spawn()?;
    let stderr = crate::cli::stderr_tail(child.stderr.take());
    let out = child.wait_with_output().await?;
    anyhow::ensure!(out.status.success(), "exited with {}: {}", out.status, stderr.lock().unwrap().trim());
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn converse(mut cmd: Command, requests: &[Value], done: impl Fn(&Value) -> bool) -> Result<Value> {
    cmd.stdin(Stdio::piped());
    let state = crate::cli::own_cli_state().await;
    let mut child: Child = cmd.spawn()?;
    drop(state);
    let stderr = crate::cli::stderr_tail(child.stderr.take());
    let mut stdin = child.stdin.take().context("stdin")?;
    for request in requests {
        stdin.write_all(format!("{request}\n").as_bytes()).await?;
    }
    stdin.flush().await?;
    let mut lines = BufReader::new(child.stdout.take().context("stdout")?).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if done(&value) {
                drop(stdin);
                let _ = child.kill().await;
                return Ok(value);
            }
        }
    }
    drop(stdin);
    let status = child.wait().await?;
    Err(anyhow!("CLI exited with {status} before reporting models: {}", stderr.lock().unwrap().trim()))
}

async fn claude() -> Result<Vec<ModelOption>> {
    let launch = crate::claude::detect::detected().await?;
    let mut cmd = command(&launch.program)?;
    launch.configure(&mut cmd);
    if let Some(auth) = crate::claude::user_auth_settings() {
        cmd.arg("--settings").arg(Value::Object(auth).to_string());
    }
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--tools",
        "",
    ]);
    let request = json!({"type": "control_request", "request_id": "models", "request": {"subtype": "initialize"}});
    let response = converse(cmd, &[request], |v| {
        v.get("type").and_then(Value::as_str) == Some("control_response")
    })
    .await?;
    Ok(parse_claude(&response))
}

fn parse_claude(response: &Value) -> Vec<ModelOption> {
    response
        .pointer("/response/response/models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("value")?.as_str()?;
            (id != "default").then(|| ModelOption {
                id: id.to_string(),
                label: m.get("displayName").and_then(Value::as_str).unwrap_or(id).to_string(),
            })
        })
        .collect()
}

async fn codex() -> Result<Vec<ModelOption>> {
    let mut cmd = command("codex")?;
    cmd.arg("app-server");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "loka", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "model/list", "params": {}}),
    ];
    let response = converse(cmd, &requests, |v| v.get("id").and_then(Value::as_i64) == Some(2)).await?;
    Ok(parse_codex(&response))
}

fn parse_codex(response: &Value) -> Vec<ModelOption> {
    response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|m| {
            let id = m.get("model").or_else(|| m.get("id"))?.as_str()?;
            Some(ModelOption {
                id: id.to_string(),
                label: m.get("displayName").and_then(Value::as_str).unwrap_or(id).to_string(),
            })
        })
        .collect()
}

async fn omp() -> Result<Vec<ModelOption>> {
    let config = crate::omp::config_source();
    let run_dir = crate::omp::config_dir(None).await?;
    crate::cli::give_to_job_user(run_dir.path())?;
    let mut cmd = command("omp")?;
    cmd.env_remove("OMP_PROFILE").env_remove("PI_PROFILE");
    cmd.env("PI_CODING_AGENT_DIR", run_dir.path());
    cmd.args(["models", "--json"]);
    let listing: Value = serde_json::from_str(&output(cmd).await?)?;
    let models_yml = tokio::fs::read_to_string(config.join("models.yml")).await.ok();
    Ok(parse_omp(&listing, models_yml.as_deref()))
}

fn configured_providers(models_yml: Option<&str>) -> HashSet<String> {
    models_yml
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(text).ok())
        .and_then(|config| config.get("providers").and_then(serde_yaml::Value::as_mapping).cloned())
        .map(|providers| providers.keys().filter_map(|k| k.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn parse_omp(listing: &Value, models_yml: Option<&str>) -> Vec<ModelOption> {
    let configured = configured_providers(models_yml);
    listing
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("kind").and_then(Value::as_str).is_none_or(|k| k == "chat"))
        .filter(|m| {
            configured.is_empty()
                || m.get("provider").and_then(Value::as_str).is_some_and(|p| configured.contains(p))
        })
        .filter_map(|m| {
            let id = m.get("selector")?.as_str()?;
            Some(ModelOption {
                id: id.to_string(),
                label: m.get("name").and_then(Value::as_str).unwrap_or(id).to_string(),
            })
        })
        .collect()
}

async fn opencode() -> Result<Vec<ModelOption>> {
    let mut cmd = command("opencode")?;
    cmd.arg("models");
    Ok(parse_lines(&output(cmd).await?, |line| line.contains('/')))
}

async fn cursor() -> Result<Vec<ModelOption>> {
    let mut cmd = command("cursor-agent")?;
    cmd.arg("models");
    Ok(parse_cursor(&output(cmd).await?))
}

fn parse_cursor(text: &str) -> Vec<ModelOption> {
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(|line| line.trim().split_once(" - "))
        .map(|(id, label)| (id.trim(), label.trim().trim_end_matches("(current)").trim_end_matches("(default)").trim()))
        .filter(|(id, _)| !id.is_empty() && !id.contains(' ') && *id != "auto" && seen.insert(id.to_string()))
        .map(|(id, label)| ModelOption { id: id.to_string(), label: label.to_string() })
        .collect()
}

async fn grok() -> Result<Vec<ModelOption>> {
    let mut cmd = command("grok")?;
    cmd.arg("models");
    let text = output(cmd).await?;
    let listed = text.split_once("Available models:").map(|(_, rest)| rest).unwrap_or_default();
    Ok(parse_lines(listed, |_| true))
}

fn parse_lines(text: &str, keep: impl Fn(&str) -> bool) -> Vec<ModelOption> {
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(|line| line.trim().trim_start_matches("* ").split_whitespace().next())
        .filter(|id| keep(id) && seen.insert(id.to_string()))
        .map(|id| ModelOption { id: id.to_string(), label: id.to_string() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_skips_the_default_alias() {
        let response = json!({"response": {"response": {"models": [
            {"value": "default", "displayName": "Default"},
            {"value": "opus", "displayName": "Opus 5.5"},
            {"value": "claude-fable-5-1", "displayName": "Fable 5.1"}
        ]}}});
        let ids: Vec<_> = parse_claude(&response).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["opus", "claude-fable-5-1"]);
    }

    #[test]
    fn codex_skips_hidden_models() {
        let response = json!({"result": {"data": [
            {"id": "gpt-6.1-sol", "model": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "hidden": false},
            {"id": "internal", "model": "internal", "hidden": true}
        ]}});
        assert_eq!(
            parse_codex(&response),
            [ModelOption { id: "gpt-6.1-sol".into(), label: "GPT-6.1-Sol".into() }]
        );
    }

    #[test]
    fn omp_keeps_only_configured_providers() {
        let listing = json!({"models": [
            {"provider": "amazon-bedrock", "kind": "chat", "selector": "amazon-bedrock/kimi", "name": "Kimi"},
            {"provider": "fal", "kind": "chat", "selector": "fal/kimi-k3", "name": "fal Kimi-K3"},
            {"provider": "fal", "kind": "image", "selector": "fal/flux", "name": "Flux"}
        ]});
        let yml = include_str!("../../shared/omp-models.yml");
        let ids: Vec<_> = parse_omp(&listing, Some(yml)).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["fal/kimi-k3"]);
        assert_eq!(parse_omp(&listing, None).len(), 2);
    }

    #[test]
    fn cursor_listing_keeps_ids_and_labels() {
        let text = "Available models\n\nauto - Auto (default)\ncomposer-2.5 - Composer 2.5 (current)\ngpt-5.2 - GPT-5.2\nTip: use --model\n";
        let models = parse_cursor(text);
        assert_eq!(models, vec![
            ModelOption { id: "composer-2.5".into(), label: "Composer 2.5".into() },
            ModelOption { id: "gpt-5.2".into(), label: "GPT-5.2".into() },
        ]);
    }

    #[test]
    fn grok_and_opencode_text_listings() {
        let grok = "You are not authenticated.\n\nDefault model: grok-build\n\nAvailable models:\n  * grok-build (default)\n  grok-4\n";
        let listed = grok.split_once("Available models:").unwrap().1;
        let ids: Vec<_> = parse_lines(listed, |_| true).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["grok-build", "grok-4"]);
        let opencode = "openrouter/moonshotai/kimi-k2.6\nanthropic/claude-sonnet-5\nnot a model\n";
        assert_eq!(parse_lines(opencode, |l| l.contains('/')).len(), 2);
    }

    #[tokio::test]
    #[ignore]
    async fn live_discovery() {
        for (backend, bin) in BACKENDS {
            if command_exists(bin) {
                println!("{backend}: {:?}", discover(backend).await.map(|m| m.into_iter().map(|m| m.id).collect::<Vec<_>>()));
            }
        }
    }
}
