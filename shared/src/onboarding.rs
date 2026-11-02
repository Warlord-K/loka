use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::world::WorldStore;

pub const SKIP: &str = "Skip setup";
pub const KEEP_PERSONA: &str = "Keep my persona";
pub const STARTER_YES: &str = "Set up a starter team";
pub const STARTER_NO: &str = "Skip";
pub const GOAL_LATER: &str = "Not sure yet";
pub const GOAL_MAX: usize = 2000;
pub const FINISHED_KIND: &str = "onboarding.finished";
pub const CLIS: &[&str] = &["claude", "codex", "omp", "cursor", "opencode"];
const MODEL_CHOICES: usize = 5;
pub const NAME_MAX: usize = 40;
pub const PERSONA_MAX: usize = 2000;

pub const PERSONAS: &[(&str, &str)] = &[
    (
        "Calm senior engineer",
        "A calm, precise senior engineer. Says little, checks facts first, flags risks early and keeps the big picture of the codebase. Helps with planning work, reviewing changes and keeping quests moving.",
    ),
    (
        "Upbeat project manager",
        "An upbeat, organised project manager. Breaks fuzzy goals into clear quests, hands them to the right agent, follows up without nagging and sends short, plain status updates.",
    ),
    (
        "Curious researcher",
        "A curious researcher who digs into questions, compares options with evidence and explains findings simply. Helps with investigations, write-ups and deciding between approaches.",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Tools,
    Cli,
    Model,
    Name,
    Persona,
    Scan,
    Starter,
    Goal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Onboarding {
    pub step: Step,
    pub agent_id: Uuid,
    #[serde(default)]
    pub cli: Option<String>,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub goal: Option<String>,
}

pub fn is_skip(answer: &str) -> bool {
    let a = answer.trim().trim_end_matches(['.', '!']).to_lowercase();
    a == SKIP.to_lowercase() || a == "skip" || a == "skip it" || a == "later" || a == "not now"
}

pub fn wants_starter(answer: &str) -> Option<bool> {
    let a = answer.trim().trim_end_matches(['.', '!']).to_lowercase();
    if a == STARTER_YES.to_lowercase() || ["yes", "yes please", "sure", "ok", "okay", "do it", "set it up", "go ahead"].contains(&a.as_str()) {
        return Some(true);
    }
    if is_skip(&a) || ["no", "no thanks", "nope", "later"].contains(&a.as_str()) {
        return Some(false);
    }
    let said = words(&a);
    if said.iter().any(|w| matches!(w.as_str(), "no" | "not" | "skip" | "nope" | "don't" | "dont")) {
        return Some(false);
    }
    said.iter().any(|w| w == "team" || w == "starter").then_some(true)
}

pub fn pick_goal(answer: &str) -> Option<String> {
    let text = answer.trim();
    let a = text.trim_end_matches(['.', '!']).to_lowercase();
    let unsure = a == GOAL_LATER.to_lowercase()
        || ["not sure", "no idea", "dunno", "idk", "nothing yet", "nothing", "none", "later", "not now", "no", "nope"].contains(&a.as_str())
        || is_skip(&a);
    if unsure || words(text).len() < 2 {
        return None;
    }
    Some(text.chars().take(GOAL_MAX).collect())
}

pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '.' | '/' | '_' | ':' | '[' | ']')))
        .map(|w| w.trim_matches('.').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

pub const CHECK_AGAIN: &str = "Check again";
pub const CONTINUE: &str = "Continue";
const CODING_CLIS: &[&str] = &["claude", "codex", "cursor", "omp", "opencode"];

struct Tool {
    key: &'static str,
    name: &'static str,
    install: &'static str,
    sign_in: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool { key: "claude", name: "Claude Code", install: "curl -fsSL https://claude.ai/install.sh | bash", sign_in: "claude auth login" },
    Tool { key: "codex", name: "Codex", install: "npm install -g @openai/codex", sign_in: "codex login" },
    Tool { key: "cursor", name: "Cursor", install: "curl https://cursor.com/install -fsS | bash", sign_in: "cursor-agent login" },
    Tool { key: "omp", name: "oh-my-pi", install: "bun install -g @oh-my-pi/pi-coding-agent", sign_in: "add your providers to ~/.omp/agent/models.yml" },
    Tool { key: "opencode", name: "OpenCode", install: "npm install -g opencode-ai", sign_in: "set OPENROUTER_API_KEY" },
    Tool { key: "gh", name: "GitHub CLI", install: "brew install gh", sign_in: "gh auth login" },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Ready,
    SignedOut,
    Missing,
}

pub fn tool_state(tools: &Value, key: &str) -> Option<ToolState> {
    let tool = tools.get(key)?;
    if tool.get("installed").and_then(Value::as_bool) != Some(true) {
        return Some(ToolState::Missing);
    }
    Some(if tool.get("signed_in").and_then(Value::as_bool) == Some(false) { ToolState::SignedOut } else { ToolState::Ready })
}

pub fn ready_clis(tools: &Value) -> Vec<String> {
    CODING_CLIS.iter().filter(|cli| tool_state(tools, cli) == Some(ToolState::Ready)).map(|cli| cli.to_string()).collect()
}

pub struct ToolProblem {
    pub name: &'static str,
    pub missing: bool,
    pub install: &'static str,
    pub sign_in: &'static str,
}

pub fn tool_problems(tools: &Value) -> Vec<ToolProblem> {
    let coding_ready = !ready_clis(tools).is_empty();
    let mut out: Vec<ToolProblem> = TOOLS
        .iter()
        .filter(|tool| matches!(tool.key, "claude" | "codex" | "cursor" | "gh"))
        .filter_map(|tool| match tool_state(tools, tool.key) {
            Some(ToolState::SignedOut) => Some(ToolProblem { name: tool.name, missing: false, install: tool.install, sign_in: tool.sign_in }),
            Some(ToolState::Missing) | None if tool.key == "gh" || (!coding_ready && tool.key == "claude") => {
                Some(ToolProblem { name: tool.name, missing: true, install: tool.install, sign_in: tool.sign_in })
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|p| p.missing);
    out
}

pub fn ready_tool_names(tools: &Value) -> Vec<&'static str> {
    TOOLS.iter().filter(|tool| tool_state(tools, tool.key) == Some(ToolState::Ready)).map(|tool| tool.name).collect()
}

pub fn tools_ready(tools: &Value) -> bool {
    !ready_clis(tools).is_empty() && tool_state(tools, "gh") == Some(ToolState::Ready)
}

pub fn tools_report(tools: &Value) -> String {
    let mut lines = vec!["Before we start, here is what I found on this Mac:".to_string()];
    for tool in TOOLS {
        let shown = matches!(tool.key, "claude" | "codex" | "cursor" | "gh") || tools.get(tool.key).is_some();
        if !shown {
            continue;
        }
        let line = match tool_state(tools, tool.key).unwrap_or(ToolState::Missing) {
            ToolState::Ready => format!("- **{}**: ready", tool.name),
            ToolState::SignedOut => format!("- **{}**: installed, not signed in. Run `{}`", tool.name, tool.sign_in),
            ToolState::Missing => format!("- **{}**: not installed. Install it with `{}`, then `{}`", tool.name, tool.install, tool.sign_in),
        };
        lines.push(line);
    }
    let mut notes = Vec::new();
    if ready_clis(tools).is_empty() {
        notes.push("I need at least one of Claude Code, Codex or Cursor installed and signed in before I can think.");
    }
    if tool_state(tools, "gh") != Some(ToolState::Ready) {
        notes.push("The GitHub CLI lets your agents push branches and open pull requests.");
    }
    notes.push("Run the commands in Terminal (press T here to open one), then tap Check again. I look again every half minute.");
    format!("{}\n\n{}", lines.join("\n"), notes.join(" "))
}

pub fn tools_choices(tools: &Value) -> Vec<String> {
    if ready_clis(tools).is_empty() {
        vec![CHECK_AGAIN.to_string()]
    } else {
        vec![CHECK_AGAIN.to_string(), CONTINUE.to_string()]
    }
}

pub fn wants_check_again(answer: &str) -> bool {
    let a = answer.trim().trim_end_matches(['.', '!']).to_lowercase();
    a == CHECK_AGAIN.to_lowercase() || a == "check" || a == "again" || a == "done" || a == "recheck"
}

pub fn pick_cli(answer: &str, available: &[String]) -> Option<String> {
    let said = words(answer);
    fn alias(w: &str) -> &str {
        match w {
            "claude-code" | "anthropic" => "claude",
            "openai" | "gpt" => "codex",
            other => other,
        }
    }
    said.iter().map(|w| alias(w)).find_map(|w| available.iter().find(|c| c.as_str() == w).cloned())
}

pub fn pick_model(answer: &str, default: &str, models: &[String]) -> Option<String> {
    let text = answer.trim().trim_end_matches(" (default)").trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    if lower == "default" || lower.contains("the default") || lower == "whatever" || lower == "you pick" {
        return Some(default.to_string()).filter(|d| !d.is_empty());
    }
    if let Some(known) = models.iter().find(|m| m.eq_ignore_ascii_case(text)) {
        return Some(known.clone());
    }
    let said = words(text);
    if let Some(known) = models.iter().find(|m| said.iter().any(|w| w == &m.to_lowercase())) {
        return Some(known.clone());
    }
    if !text.contains(char::is_whitespace) && !text.starts_with('-') && text.chars().count() <= 200 {
        return Some(text.to_string());
    }
    None
}

pub fn model_choices(default: &str, models: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !default.is_empty() {
        out.push(format!("{default} (default)"));
    }
    for m in models {
        if out.len() >= MODEL_CHOICES {
            break;
        }
        if m != default && !out.contains(m) {
            out.push(m.clone());
        }
    }
    out
}

pub fn handles_for(name: &str) -> Vec<String> {
    let mut base = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base: String = base.trim_matches('-').chars().take(40).collect();
    let base = base.trim_matches('-').to_string();
    if base.is_empty() {
        return Vec::new();
    }
    std::iter::once(base.clone()).chain((2..=9).map(|n| format!("{base}-{n}"))).collect()
}

pub fn pick_name(answer: &str, current: &str) -> Option<String> {
    let text = answer.trim().trim_matches(['"', '\'', '.', '!']).trim();
    let lower = text.to_lowercase();
    if lower.starts_with("keep") || lower == current.to_lowercase() {
        return Some(current.to_string());
    }
    let stripped = ["call you ", "name you ", "you are "]
        .iter()
        .find_map(|p| lower.find(p).and_then(|at| text.get(at + p.len()..)))
        .map(str::trim)
        .unwrap_or(text);
    let name = stripped.trim_matches(['"', '\'', '.', '!']).trim();
    let ok = !name.is_empty()
        && name.chars().count() <= NAME_MAX
        && name.split_whitespace().count() <= 4
        && name.chars().any(char::is_alphanumeric);
    ok.then(|| name.to_string())
}

pub enum PersonaChoice {
    Keep,
    Set(String),
}

pub fn pick_persona(answer: &str) -> Option<PersonaChoice> {
    let text = answer.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    if lower.starts_with("keep") || lower == "no" || lower == "nope" || lower == "none" {
        return Some(PersonaChoice::Keep);
    }
    if let Some((_, persona)) = PERSONAS.iter().find(|(label, _)| label.eq_ignore_ascii_case(text)) {
        return Some(PersonaChoice::Set((*persona).to_string()));
    }
    Some(PersonaChoice::Set(text.chars().take(PERSONA_MAX).collect()))
}

impl WorldStore {
    pub async fn onboarding_pending(&self, user_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT onboarded_at IS NULL FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(self.pool())
            .await?
            .unwrap_or(false))
    }

    pub async fn onboarding(&self, user_id: Uuid) -> Result<Option<Onboarding>> {
        let raw = sqlx::query_scalar::<_, Option<Value>>("SELECT onboarding FROM users WHERE id = $1 AND onboarded_at IS NULL")
            .bind(user_id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        Ok(raw.and_then(|v| serde_json::from_value(v).ok()))
    }

    pub async fn onboarding_for(&self, user_id: Uuid, agent_id: Uuid) -> Result<Option<Onboarding>> {
        Ok(self.onboarding(user_id).await?.filter(|o| o.agent_id == agent_id))
    }

    pub async fn begin_onboarding(&self, user_id: Uuid, state: &Onboarding) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE users SET onboarding = $2 WHERE id = $1 AND onboarded_at IS NULL AND onboarding IS NULL",
        )
        .bind(user_id)
        .bind(serde_json::to_value(state)?)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() == 1)
    }

    pub async fn set_onboarding(&self, user_id: Uuid, state: &Onboarding) -> Result<()> {
        sqlx::query("UPDATE users SET onboarding = $2 WHERE id = $1 AND onboarded_at IS NULL")
            .bind(user_id)
            .bind(serde_json::to_value(state)?)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn finish_onboarding(&self, user_id: Uuid) -> Result<bool> {
        let done = sqlx::query("UPDATE users SET onboarded_at = NOW() WHERE id = $1 AND onboarded_at IS NULL")
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_name_gets_a_matching_handle() {
        assert_eq!(handles_for("Nova")[..3], ["nova", "nova-2", "nova-3"]);
        assert_eq!(handles_for("Captain Hook!")[0], "captain-hook");
        assert_eq!(handles_for("  R2 D2 ")[0], "r2-d2");
        assert!(handles_for("ñ").is_empty() || handles_for("ñ")[0].chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert!(handles_for("🙂").is_empty());
        assert_eq!(handles_for("Nova").len(), 9);
    }

    #[test]
    fn tools_report_lists_what_to_install_and_who_is_ready() {
        let tools = serde_json::json!({
            "claude": { "installed": true, "signed_in": true },
            "codex": { "installed": false, "signed_in": false },
            "cursor": { "installed": true, "signed_in": false },
            "gh": { "installed": false, "signed_in": false },
        });
        assert_eq!(ready_clis(&tools), vec!["claude".to_string()]);
        assert!(!tools_ready(&tools));
        let report = tools_report(&tools);
        assert!(report.contains("**Claude Code**: ready"));
        assert!(report.contains("npm install -g @openai/codex"));
        assert!(report.contains("`cursor-agent login`"));
        assert!(report.contains("brew install gh"));
        assert!(!report.contains("oh-my-pi"));
        assert!(!report.contains('\u{2014}'));
        assert_eq!(tools_choices(&tools), vec![CHECK_AGAIN.to_string(), CONTINUE.to_string()]);
        let none = serde_json::json!({ "claude": { "installed": false }, "gh": { "installed": true, "signed_in": true } });
        assert!(ready_clis(&none).is_empty());
        assert_eq!(tools_choices(&none), vec![CHECK_AGAIN.to_string()]);
        assert!(tools_report(&none).contains("at least one of"));
        let unknown = serde_json::json!({ "claude": { "installed": true, "signed_in": null }, "gh": { "installed": true, "signed_in": true } });
        assert!(tools_ready(&unknown));
        assert!(wants_check_again("check again"));
        assert!(!wants_check_again("continue"));
    }

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn onboarding_answers() {
        let clis = list(&["claude", "codex", "omp"]);
        assert_eq!(pick_cli("codex", &clis).as_deref(), Some("codex"));
        assert_eq!(pick_cli("I'd like Claude please", &clis).as_deref(), Some("claude"));
        assert_eq!(pick_cli("cursor", &clis), None);

        let models = list(&["sonnet", "opus", "haiku"]);
        assert_eq!(pick_model("sonnet (default)", "sonnet", &models).as_deref(), Some("sonnet"));
        assert_eq!(pick_model("let's go with opus", "sonnet", &models).as_deref(), Some("opus"));
        assert_eq!(pick_model("claude-fable-5-1", "sonnet", &models).as_deref(), Some("claude-fable-5-1"));
        assert_eq!(pick_model("--dangerous", "sonnet", &models), None);
        assert_eq!(pick_model("no idea honestly", "sonnet", &models), None);
        assert_eq!(model_choices("sonnet", &models), list(&["sonnet (default)", "opus", "haiku"]));

        assert_eq!(pick_name("Keep Priya", "Priya").as_deref(), Some("Priya"));
        assert_eq!(pick_name("I'll call you Ada", "Priya").as_deref(), Some("Ada"));
        assert_eq!(pick_name("Juno", "Priya").as_deref(), Some("Juno"));
        assert_eq!(pick_name("this is a much longer sentence than any name", "Priya"), None);

        assert!(matches!(pick_persona("Keep my persona"), Some(PersonaChoice::Keep)));
        assert!(matches!(pick_persona("Curious researcher"), Some(PersonaChoice::Set(p)) if p.starts_with("A curious")));
        assert!(is_skip("Skip setup") && is_skip("skip.") && !is_skip("claude"));
        assert_eq!(wants_starter(STARTER_YES), Some(true));
        assert_eq!(wants_starter("yes please"), Some(true));
        assert_eq!(wants_starter(STARTER_NO), Some(false));
        assert_eq!(wants_starter("not now"), Some(false));
        assert_eq!(wants_starter("no team for me"), Some(false));
        assert_eq!(wants_starter("what is that?"), None);

        assert_eq!(pick_goal(GOAL_LATER), None);
        assert_eq!(pick_goal("not sure."), None);
        assert_eq!(pick_goal("Skip setup"), None);
        assert_eq!(pick_goal("hello"), None);
        assert_eq!(
            pick_goal("I want to contribute to https://github.com/astral-sh/uv").as_deref(),
            Some("I want to contribute to https://github.com/astral-sh/uv")
        );
        assert_eq!(pick_goal(&"word ".repeat(1000)).map(|g| g.chars().count()), Some(GOAL_MAX));
    }
}
