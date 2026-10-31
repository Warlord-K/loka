use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::process::Command;

const SETTLED_EVERY: Duration = Duration::from_secs(15 * 60);
const MISSING_EVERY: Duration = Duration::from_secs(30);
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

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
            let tools = check_all().await;
            let settled = settled(&tools);
            *cache().lock().unwrap() = tools;
            tokio::time::sleep(if settled { SETTLED_EVERY } else { MISSING_EVERY }).await;
        }
    });
}

fn settled(tools: &Map<String, Value>) -> bool {
    tools.values().all(|tool| tool.get("installed").and_then(Value::as_bool) == Some(true) && tool.get("signed_in").and_then(Value::as_bool) != Some(false))
}

async fn check_all() -> Map<String, Value> {
    let mut tools = Map::new();
    tools.insert("claude".into(), claude().await);
    tools.insert("codex".into(), by_exit("codex", &["login", "status"]).await);
    tools.insert("cursor".into(), cursor().await);
    tools.insert("gh".into(), gh().await);
    if crate::model_catalog::command_exists("omp") {
        tools.insert("omp".into(), json!({ "installed": true, "signed_in": omp_configured() }));
    }
    if loka_shared::backends::opencode_enabled() && crate::model_catalog::command_exists("opencode") {
        tools.insert("opencode".into(), json!({ "installed": true, "signed_in": std::env::var_os("OPENROUTER_API_KEY").is_some() }));
    }
    tools
}

fn missing() -> Value {
    json!({ "installed": false, "signed_in": false })
}

fn status(signed_in: Option<bool>) -> Value {
    json!({ "installed": true, "signed_in": signed_in })
}

fn command(program: impl AsRef<std::ffi::OsStr>) -> Option<Command> {
    let mut cmd = crate::cli::job_command(program);
    crate::cli::configure_child_environment(&mut cmd, &BTreeMap::new()).ok()?;
    cmd.kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Some(cmd)
}

async fn run(mut cmd: Command) -> Option<(bool, String)> {
    let out = tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text))
}

async fn claude() -> Value {
    if !crate::claude::detect::installed() {
        return missing();
    }
    if ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"].iter().any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty())) {
        return status(Some(true));
    }
    let Ok(launch) = crate::claude::detect::detected().await else {
        return status(None);
    };
    let Some(mut cmd) = command(&launch.program) else {
        return status(None);
    };
    launch.configure(&mut cmd);
    cmd.args(["auth", "status", "--json"]);
    let Some((_, text)) = run(cmd).await else {
        return status(None);
    };
    let signed_in = serde_json::from_str::<Value>(text.trim()).ok().and_then(|v| v.get("loggedIn").and_then(Value::as_bool));
    status(signed_in)
}

async fn by_exit(bin: &str, args: &[&str]) -> Value {
    if !crate::model_catalog::command_exists(bin) {
        return missing();
    }
    let Some(mut cmd) = command(bin) else {
        return status(None);
    };
    cmd.args(args);
    status(run(cmd).await.map(|(ok, _)| ok))
}

async fn cursor() -> Value {
    if !crate::model_catalog::command_exists("cursor-agent") {
        return missing();
    }
    let Some(mut cmd) = command("cursor-agent") else {
        return status(None);
    };
    cmd.arg("status");
    status(run(cmd).await.map(|(ok, text)| ok && cursor_signed_in(&text)))
}

fn cursor_signed_in(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("logged in") && !lower.contains("not logged in")
}

async fn gh() -> Value {
    if !crate::model_catalog::command_exists("gh") {
        return missing();
    }
    if ["GH_TOKEN", "GITHUB_TOKEN"].iter().any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty())) {
        return status(Some(true));
    }
    by_exit("gh", &["auth", "status", "--hostname", "github.com"]).await
}

fn omp_configured() -> bool {
    let dir = std::env::var_os("OMP_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".omp").join("agent")));
    dir.is_some_and(|dir| dir.join("models.yml").is_file() || dir.join("models.yaml").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_status_text_is_read_both_ways() {
        assert!(cursor_signed_in("✓ Logged in as someone@example.com"));
        assert!(!cursor_signed_in("Not logged in. Run cursor-agent login"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_tools_check() {
        let tools = check_all().await;
        println!("{}", serde_json::to_string_pretty(&tools).unwrap());
        assert!(tools.contains_key("gh"));
    }

    #[test]
    fn settled_only_when_everything_is_ready() {
        let mut tools = Map::new();
        tools.insert("claude".into(), status(Some(true)));
        tools.insert("gh".into(), status(None));
        assert!(settled(&tools));
        tools.insert("codex".into(), missing());
        assert!(!settled(&tools));
        tools.insert("codex".into(), status(Some(false)));
        assert!(!settled(&tools));
    }
}
