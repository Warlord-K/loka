use serde_json::Value;

pub const OPENCODE: &str = "opencode";
pub const ENABLE_OPENCODE_ENV: &str = "WORLD_ENABLE_OPENCODE";
pub const AGENT_CLIS: [&str; 5] = ["claude", "codex", "cursor", OPENCODE, "omp"];
pub const OPENCODE_DISABLED: &str =
    "OpenCode is disabled on this device. Switch the agent to another CLI, or have a device admin set WORLD_ENABLE_OPENCODE=1 and restart the runner.";

pub fn opencode_enabled() -> bool {
    flag_on(std::env::var(ENABLE_OPENCODE_ENV).ok().as_deref())
}

fn flag_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

pub fn cost_per_run(cli: &str) -> bool {
    matches!(cli, "omp" | OPENCODE)
}

pub fn tokens_only(cli: &str) -> bool {
    matches!(cli, "cursor" | "codex")
}

pub fn is_agent_cli(cli: &str) -> bool {
    AGENT_CLIS.contains(&cli)
}

pub fn enabled_here(cli: &str) -> bool {
    cli != OPENCODE || opencode_enabled()
}

pub fn agent_clis_here() -> Vec<&'static str> {
    AGENT_CLIS.into_iter().filter(|c| enabled_here(c)).collect()
}

pub fn cli_list_here() -> String {
    let clis = agent_clis_here();
    match clis.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => clis.join(""),
    }
}

pub fn runner_offers_opencode(capabilities: &Value) -> bool {
    capabilities.get("backends").and_then(|b| b.get(OPENCODE)).and_then(Value::as_bool) == Some(true)
}

pub fn allowed_on(cli: &str, runner_capabilities: Option<&Value>) -> bool {
    if cli != OPENCODE {
        return true;
    }
    match runner_capabilities {
        Some(caps) => runner_offers_opencode(caps),
        None => opencode_enabled(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opencode_flag_parsing() {
        assert!(!flag_on(None));
        assert!(!flag_on(Some("")));
        assert!(!flag_on(Some("0")));
        assert!(flag_on(Some("1")));
        assert!(flag_on(Some(" TRUE ")));
    }

    #[test]
    fn runners_must_advertise_opencode() {
        assert!(allowed_on("claude", Some(&json!({}))));
        assert!(!allowed_on(OPENCODE, Some(&json!({}))));
        assert!(!allowed_on(OPENCODE, Some(&json!({"backends": {"opencode": false}}))));
        assert!(allowed_on(OPENCODE, Some(&json!({"backends": {"opencode": true}}))));
    }
}
