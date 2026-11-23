use std::{collections::{HashMap, HashSet}, error::Error as StdError, fmt};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::run_causes::Required;

pub const CHANNEL_EVENTS: &str = "world_events";

pub const COMPACT_PROMPT: &str = "/compact";
pub const SESSION_STARTED_LIKE: &str = "Session started: %";

pub fn compact_limit(limit: i64, floor: Option<i64>) -> i64 {
    floor.map_or(limit, |floor| limit.max(floor + limit / 4))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EventWake {
    pub seq: i64,
    pub org: Option<Uuid>,
}

impl EventWake {
    pub fn from_payload(payload: &str) -> Self {
        let value = serde_json::from_str::<Value>(payload).unwrap_or(Value::Null);
        EventWake {
            seq: value.get("seq").and_then(Value::as_i64).unwrap_or(0),
            org: value.get("org_id").and_then(Value::as_str).and_then(|org| org.parse().ok()),
        }
    }

    pub fn concerns(&self, org: Uuid) -> bool {
        self.org.is_none_or(|own| own == org)
    }
}
pub const PLAN_WAITING_NOTE: &str = "needs you: plan waiting for approval";

pub fn plan_waiting_sql(task: &str) -> String {
    format!("({task}.status NOT IN ('done', 'cancelled') AND EXISTS (SELECT 1 FROM agent_sessions s WHERE s.task_id = {task}.id AND s.plan_state = 'awaiting' AND s.status <> 'archived'))")
}

pub const CHANNEL_INBOX: &str = "agent_inbox";
pub const CHANNEL_RUNS: &str = "session_runs";
pub const CHANNEL_SCHEDULES: &str = "schedules_changed";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorldAgent {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub avatar_emoji: String,
    pub cli: String,
    pub model: String,
    pub system_prompt_addendum: Option<String>,
    pub persona: Option<String>,
    pub role: String,
    pub look: Value,
    pub gear: Value,
    pub status: String,
    pub status_detail: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub visibility: String,
    pub paused_reason: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub archive_delete_at: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    pub expertise: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub runner_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default)]
    pub runner_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_powers: Vec<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub approval_mode: String,
    #[sqlx(default)]
    #[serde(default)]
    pub reviewer: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
    #[sqlx(default)]
    #[serde(default)]
    pub daily_token_budget: Option<i64>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default = "default_quest_context")]
    pub quest_context: String,
    pub is_main: bool,
    pub jobs: Value,
}

fn default_quest_context() -> String {
    crate::context::QuestContext::default().as_str().to_string()
}

impl WorldAgent {
    pub fn has_power(&self, power: &str) -> bool {
        !self.revoked_powers.iter().any(|p| p == power)
    }
}
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub role: String,
    pub backend: String,
    pub model: String,
    pub cli_session_id: Option<String>,
    pub parent_session_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub working_dir: Option<String>,
    pub status: String,
    pub usage_baseline: Value,
    pub context_tokens: i64,
    pub review_rounds: i32,
    pub summary: Option<String>,
    pub rules_hash: Option<String>,
    pub asked_by: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub title: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub rewind: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub review: Option<bool>,
    #[sqlx(default)]
    #[serde(default)]
    pub reviewer: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub plan_state: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub attempt: Option<i16>,
    #[sqlx(default)]
    #[serde(default)]
    pub attempt_state: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentSession {
    pub fn planning(&self) -> bool {
        self.plan_state.as_deref() == Some("planning")
    }

    pub fn racing(&self) -> bool {
        self.attempt_state.as_deref() == Some("running")
    }

    pub fn wants_review(&self) -> bool {
        self.review.unwrap_or(true)
    }

    pub fn review_target(&self, agent_default: Option<&Value>) -> (String, String) {
        let choice = self
            .reviewer
            .as_ref()
            .and_then(ReviewerChoice::from_value)
            .or_else(|| agent_default.and_then(ReviewerChoice::from_value));
        match choice {
            Some(c) => (c.cli.unwrap_or_else(|| self.backend.clone()), c.model),
            None => (self.backend.clone(), self.model.clone()),
        }
    }
}

pub const REVIEWER_CLIS: [&str; 5] = ["claude", "codex", "cursor", "opencode", "omp"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewerChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default)]
    pub model: String,
}

impl ReviewerChoice {
    pub fn new(cli: Option<&str>, model: &str) -> std::result::Result<Self, String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("a reviewer needs a model".into());
        }
        if model.starts_with('-') {
            return Err("model names cannot start with a dash".into());
        }
        let cli = cli.map(str::trim).filter(|c| !c.is_empty()).map(str::to_lowercase);
        if let Some(c) = cli.as_deref() {
            if !REVIEWER_CLIS.contains(&c) {
                return Err(format!("unknown reviewer CLI {c}; use one of {}", REVIEWER_CLIS.join(", ")));
            }
        }
        Ok(Self { cli, model: model.to_string() })
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let parsed: Self = serde_json::from_value(value.clone()).ok()?;
        Self::new(parsed.cli.as_deref(), &parsed.model).ok()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn label(&self) -> String {
        match &self.cli {
            Some(cli) => format!("{cli} {}", self.model),
            None => self.model.clone(),
        }
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    #[test]
    fn wakes_name_their_org() {
        let org = Uuid::new_v4();
        let wake = EventWake::from_payload(&json!({ "seq": 7, "type": "x", "org_id": org }).to_string());
        assert_eq!(wake.seq, 7);
        assert!(wake.concerns(org));
        assert!(!wake.concerns(Uuid::new_v4()));
        let legacy = EventWake::from_payload(r#"{"seq": 9, "type": "x"}"#);
        assert_eq!(legacy.seq, 9);
        assert!(legacy.concerns(Uuid::new_v4()));
        assert!(EventWake::from_payload("not json").concerns(org));
    }
}

#[cfg(test)]
mod reviewer_tests {
    use super::*;

    fn session(reviewer: Option<Value>) -> AgentSession {
        serde_json::from_value(json!({
            "id": Uuid::nil(), "agent_id": Uuid::nil(), "role": "task", "backend": "codex", "model": "gpt-5.5",
            "cli_session_id": null, "parent_session_id": null, "task_id": null, "working_dir": null,
            "status": "active", "usage_baseline": {}, "context_tokens": 0, "review_rounds": 0, "summary": null,
            "rules_hash": null, "asked_by": null, "reviewer": reviewer,
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap()
    }

    #[test]
    fn reviewer_resolution_order() {
        let fable = json!({"cli": "claude", "model": "fable"});
        assert_eq!(session(None).review_target(None), ("codex".into(), "gpt-5.5".into()));
        assert_eq!(session(None).review_target(Some(&fable)), ("claude".into(), "fable".into()));
        assert_eq!(
            session(Some(json!({"model": "gpt-5.5-mini"}))).review_target(Some(&fable)),
            ("codex".into(), "gpt-5.5-mini".into())
        );
        assert_eq!(session(Some(json!({"model": "  "}))).review_target(Some(&fable)), ("claude".into(), "fable".into()));
    }

    #[test]
    fn reviewer_choice_validates() {
        assert!(ReviewerChoice::new(Some("Claude"), "fable").unwrap().cli.as_deref() == Some("claude"));
        assert!(ReviewerChoice::new(Some("gemini"), "x").is_err());
        assert!(ReviewerChoice::new(None, "").is_err());
        assert_eq!(ReviewerChoice::new(None, "opus").unwrap().to_value(), json!({"model": "opus"}));
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SessionRun {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub task_id: Option<Uuid>,
    pub kind: String,
    pub status: String,
    pub fork_from: Option<Uuid>,
    pub prompt: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub timeout_secs: i32,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub request: Option<Value>,
}

async fn kept_memory(tx: &mut Transaction<'_, Postgres>, session_id: Uuid, before: DateTime<Utc>) -> Result<String> {
    let kept = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT prompt, output FROM session_runs
         WHERE session_id = $1 AND kind = 'turn' AND status = 'completed' AND created_at < $2
           AND prompt <> '/compact'
         ORDER BY created_at DESC LIMIT 40",
    )
    .bind(session_id)
    .bind(before)
    .fetch_all(&mut **tx)
    .await?;
    Ok(replay_memory(&kept))
}

fn replay_memory(newest_first: &[(String, Option<String>)]) -> String {
    const BUDGET: usize = 60_000;
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0;
    for (prompt, output) in newest_first {
        let part = format!(
            "[what you were told]\n{}\n[what you answered]\n{}",
            prompt.trim(),
            output.as_deref().unwrap_or("").trim()
        );
        used += part.len();
        if used > BUDGET && !parts.is_empty() {
            break;
        }
        parts.push(part);
    }
    parts.reverse();
    format!(
        "Your conversation was rewound to an earlier point and your mind restarted. This is the part you keep, oldest first. Anything after it never happened:\n\n{}",
        parts.join("\n\n")
    )
}

pub enum Rewind {
    None,
    Fresh,
    At { from: String, anchor: String },
}

impl Rewind {
    pub fn from(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::None;
        };
        let text = |k: &str| value.get(k).and_then(Value::as_str).map(String::from);
        match (text("from"), text("at").or_else(|| text("drop"))) {
            (Some(from), Some(anchor)) => Self::At { from, anchor },
            _ => Self::Fresh,
        }
    }

    pub fn anchor(&self) -> Option<&str> {
        match self {
            Self::At { anchor, .. } => Some(anchor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Rewound {
    NotFound,
    Busy,
    Unsupported(String),
    Done { deleted: Vec<Uuid>, rewound_session: bool },
}

#[derive(Debug, Clone, Copy)]
pub enum DiskWaitKey {
    Run,
    Session,
    Quest,
}

pub const WAITING_FOR_DISK_NOTE: &str = "waiting for disk space on its device, starts by itself once there is room";

#[derive(Debug, Clone, Serialize)]
pub struct SessionRunView {
    #[serde(flatten)]
    pub run: SessionRun,
    pub retryable: bool,
    pub retry_mode: Option<String>,
    pub steered_into: Option<Uuid>,
    pub interrupt: bool,
    pub waiting_for: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSteer {
    pub id: Uuid,
    pub text: String,
    pub interrupt: bool,
}

pub const TOOLLESS_BACKENDS: [&str; 2] = ["claude", "omp"];
pub const GUEST_BACKEND: &str = "claude";
pub const GUEST_MODEL: &str = "sonnet";

pub fn toolless_aside(asked_by: Option<&Value>) -> bool {
    ["guest", "lean"]
        .iter()
        .any(|flag| asked_by.and_then(|a| a.get(*flag)).and_then(Value::as_bool) == Some(true))
}

pub const FRESH_NOTES_CAP: usize = 6_000;

pub fn with_notes(memory: String, notes: Option<&str>) -> String {
    let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty()) else {
        return memory;
    };
    let mut kept: String = notes.chars().take(FRESH_NOTES_CAP).collect();
    if kept.len() < notes.len() {
        kept.push_str("\n[notes shortened]");
    }
    format!("{memory}\n\nYour notes:\n{kept}")
}

pub fn default_task_timeout_secs() -> i32 {
    std::env::var("WORLD_TASK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &i32| *v > 0)
        .unwrap_or(3600)
}

pub const DIRECT_PREFIX: &str = "folders/";

pub fn direct_key(folder: Uuid) -> String {
    format!("{DIRECT_PREFIX}{folder}")
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AsideChoice {
    pub cli: Option<String>,
    pub model: Option<String>,
}

impl AsideChoice {
    pub fn parse(cli: Option<&str>, model: Option<&str>) -> Result<Self> {
        let clean = |value: Option<&str>| value.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
        let (cli, model) = (clean(cli), clean(model));
        if let Some(cli) = &cli {
            if !crate::backends::is_agent_cli(cli) {
                anyhow::bail!("unknown CLI {cli}; use one of {}", crate::backends::AGENT_CLIS.join(", "));
            }
        }
        if let Some(model) = &model {
            if model.len() > 80 || model.starts_with('-') || model.chars().any(char::is_whitespace) {
                anyhow::bail!("that model name is not valid");
            }
        }
        Ok(Self { cli, model })
    }
}

pub const FOLLOWUP_ASK: &str =
    "Answer briefly. If you change anything in the quest folder, describe it and end with an updated report block. Loka publishes the work after a done report, so a pull request that is not open yet is never a reason to report blocked.";

pub const REOPENED_ASK: &str = "Answer briefly. This quest ended without being finished and is open again: do what the message asks, then end with the full report block from your task prompt: status done once the work is finished, or blocked with what still stops you. Loka commits, pushes and opens the pull request itself after a done report, so a pull request that is not open yet is never a reason to report blocked.";

pub const REPORT_NUDGE: &str = "[report missing]\nYour last message ended without the report block, so the Loka cannot tell whether this quest is finished. Do not start over and do not redo finished steps. If you started any command in the background, wait for it to finish and read its output. If the work is not finished, keep going from where you stopped. When it is finished, or you are blocked, end your final message with the report block in exactly this form (an HTML comment holding one JSON object, not a markdown section or a YAML code fence), with status done or blocked and the other fields from the task prompt:\n<!-- report\n{\"status\": \"done\", \"summary\": \"What you did in 2-4 sentences\", \"pr_title\": \"...\", \"commit_message\": \"...\", \"checks\": [{\"check\": \"...\", \"how\": \"...\", \"result\": \"pass\"}], \"milestones_done\": [1, 2]}\nreport -->";

pub const REPORT_NUDGE_LAST: &str = "[report missing]\nThis is the last reminder: your last two messages ended without the report block. Do not run any more commands and do not redo any work. Reply now with only the report block, in exactly this form (an HTML comment holding one JSON object, not a markdown section or a YAML code fence). Say plainly in the summary what is finished and what is not, and use status done only when every check passed, otherwise blocked:\n<!-- report\n{\"status\": \"blocked\", \"summary\": \"What is finished and what is not, in 2-4 sentences\", \"pr_title\": \"...\", \"commit_message\": \"...\", \"checks\": [{\"check\": \"...\", \"how\": \"...\", \"result\": \"pass\"}], \"milestones_done\": [1, 2]}\nreport -->";

pub const REPORT_NUDGE_MARK: &str = "[report missing]";

pub const VERDICT_NUDGE: &str = "[verdict missing]\nYour last message ended without the verdict block, so the author cannot tell whether the work passed. Do not start the review over. If you started any command in the background, wait for it to finish and read its output. Then end your final message with the verdict block in exactly this form (an HTML comment holding one JSON object):\n<!-- verdict\n{\"verdict\": \"approve\" | \"changes\", \"summary\": \"...\", \"findings\": [{\"severity\": \"high|medium|low\", \"path\": \"...\", \"issue\": \"...\"}]}\nverdict -->";

pub const VERDICT_NUDGE_MARK: &str = "[verdict missing]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    Queued(Uuid),
    Open,
    Exhausted,
}

impl Nudge {
    pub fn handled(self) -> bool {
        self != Nudge::Exhausted
    }
}

pub const APPROVAL_MODES: [&str; 3] = ["auto", "full", "read_only"];
pub const DEFAULT_MAIN_SUBSCRIPTIONS: &[&str] = &["task.finished", "run.failed", "run.timed_out", "agent.created"];

pub const SUBSCRIBABLE_EVENTS: [(&str, &str); 19] = [
    ("slack.notification", "Slack DMs, mentions, keyword highlights and thread replies your person gets (from Slack capture on their Mac); to follow every message in one channel or thread instead, subscribe with slack_channel or slack_thread"),
    ("github.", "your person's GitHub notifications (github.review_requested, github.mention, github.comment, github.ci, github.assigned, github.state_change, github.other), read on their Mac while the GitHub feed is on; add repo and number to watch one pull request or issue"),
    ("task.finished", "a quest finished, with its report and PR"),
    ("task.assigned", "a quest was assigned to an agent"),
    ("task.started", "an agent started working on a quest"),
    ("task.merged", "a quest's PR was merged"),
    ("task.pr_closed", "a quest's PR was closed without merging"),
    ("task.cancelled", "a quest was cancelled"),
    ("run.failed", "a task, review or follow-up run failed"),
    ("run.timed_out", "a run hit its time limit"),
    ("review.finished", "an independent review finished"),
    ("agent.created", "a new agent joined the world"),
    ("attention.requested", "an agent asked a person to decide something"),
    ("agent.budget", "an agent used most or all of its daily budget"),
    ("channel.message", "a new post in a chat channel you can see"),
    ("timer.done", "a timer went off"),
    ("feed.item", "new items arrived on a feed one of your person's agents watches (RSS, Atom or webhook alerts)"),
    ("item.created", "a building, decoration, critter, timer or list was made"),
    ("voice.ended", "a live voice call ended"),
];

pub const SUBSCRIPTIONS_MAX: usize = 32;

pub fn normalize_event_type(raw: &str) -> Option<String> {
    let t = raw.trim().to_lowercase();
    let valid = !t.is_empty()
        && t.len() <= 64
        && !t.starts_with('.')
        && !t.contains("..")
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
    valid.then_some(t)
}

pub const SLACK_SETTLE_SECS: i64 = 6;
pub const SLACK_SETTLE_MAX_SECS: i64 = 30;
const SETTLING_KINDS: &str = "(starts_with(i.kind, 'slack.') OR starts_with(i.kind, 'github.'))";

pub fn inbox_ready_sql(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = {alias}.id AND i.status = 'pending'
            AND (i.retry_at IS NULL OR i.retry_at <= NOW())
            AND (NOT {SETTLING_KINDS} OR i.created_at < NOW() - make_interval(secs => {SLACK_SETTLE_MAX_SECS})))
          OR NOT EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = {alias}.id AND i.status = 'pending'
            AND {SETTLING_KINDS} AND i.created_at > NOW() - make_interval(secs => {SLACK_SETTLE_SECS})))"
    )
}

pub fn urgent_run_sql(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM agent_tasks ut WHERE ut.id = {alias}.task_id AND ut.pace = 'urgent')")
}

pub fn urgent_inbox_sql(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM agent_inbox ui JOIN agent_tasks ut ON ut.id::text = ui.payload->>'task_id'
           WHERE ui.agent_id = {alias}.id AND ui.status = 'pending' AND ut.pace = 'urgent')"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowupAdvance {
    Wait,
    Advance { was_completed: bool },
}

#[derive(Debug, Clone)]
pub enum TaskResume {
    Fresh,
    Busy { session_id: Uuid, kind: String, status: String },
    AwaitingPlan(Uuid),
    AwaitingClarification(Uuid),
    Resumed { session_id: Uuid, run_id: Uuid },
}

impl TaskResume {
    pub fn problem(&self) -> Option<String> {
        match self {
            TaskResume::Busy { session_id, kind, status } => Some(crate::quests::busy_problem(&crate::quests::ActiveRun {
                session_id: *session_id,
                kind: kind.clone(),
                status: status.clone(),
            })),
            TaskResume::AwaitingPlan(session_id) => Some(format!("session {session_id} has a plan that {}", crate::quests::PLAN_WAITING)),
            TaskResume::AwaitingClarification(session_id) => {
                Some(format!("session {session_id} {}", crate::quests::CLARIFICATION_WAITING))
            }
            TaskResume::Fresh | TaskResume::Resumed { .. } => None,
        }
    }
}

pub const KEY_REJECTED_PAUSE: &str = "Model key rejected";
pub const KEY_REJECTED_REASON: &str = "model_key_unauthorized";

pub fn limit_retry_at(error: &str) -> Option<DateTime<Utc>> {
    if crate::run_causes::update_pause(error) {
        return Some(Utc::now());
    }
    crate::cli_errors::classify(error).and_then(|failure| failure.retry_at(Utc::now()))
}

pub fn requeue_inbox_sql(target: &str) -> String {
    let defer = "$3::timestamptz IS NOT NULL AND created_at > NOW() - INTERVAL '3 days'";
    format!(
        "UPDATE agent_inbox SET
           status = CASE WHEN {defer} THEN 'pending'
                         WHEN attempts >= 3 OR starts_with($2, 'Failed to spawn') THEN 'failed' ELSE 'pending' END,
           attempts = CASE WHEN {defer} THEN GREATEST(attempts - 1, 0) ELSE attempts END,
           retry_at = CASE WHEN {defer} THEN $3::timestamptz ELSE retry_at END,
           error = $2
         WHERE {target}"
    )
}

pub async fn display_name_on<'e>(executor: impl sqlx::PgExecutor<'e>, user_id: Uuid) -> Result<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>("SELECT COALESCE(name, email) FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(executor)
        .await?
        .flatten())
}

pub struct RunClone<'a> {
    pub kind: Option<&'a str>,
    pub fork_from: Option<Uuid>,
    pub prompt: &'a str,
    pub activity: Option<&'a str>,
    pub retry_cause: Option<&'a str>,
    pub not_before: Option<DateTime<Utc>>,
    pub retry: bool,
}

pub async fn clone_run(tx: &mut Transaction<'_, Postgres>, source: Uuid, clone: RunClone<'_>) -> Result<SessionRun> {
    Ok(sqlx::query_as::<_, SessionRun>(
        "INSERT INTO session_runs
           (session_id, agent_id, task_id, kind, status, fork_from, prompt, timeout_secs, activity, retry_cause, not_before, request)
         SELECT session_id, agent_id, task_id, COALESCE($2, kind), 'queued', $3, $4, timeout_secs, $5, $6, $7,
                CASE WHEN $8 THEN COALESCE(request, '{}'::jsonb)
                                  || jsonb_build_object('retry', COALESCE((request->>'retry')::int, 0) + 1)
                     ELSE request END
         FROM session_runs WHERE id = $1
         RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at, request",
    )
    .bind(source)
    .bind(clone.kind)
    .bind(clone.fork_from)
    .bind(clone.prompt)
    .bind(clone.activity)
    .bind(clone.retry_cause)
    .bind(clone.not_before)
    .bind(clone.retry)
    .fetch_one(&mut **tx)
    .await?)
}

pub fn resume_prompt(from: &str, brief: Option<&str>) -> String {
    let brief = brief.map(str::trim).filter(|b| !b.is_empty());
    format!(
        "[follow-up from {from}]\nPick this quest back up in this same session and quest folder. Check what is already done (diff, commits, branch, PR) before changing anything, and keep what works.{}\n\nWhen it is finished, or you are blocked, end with the full report block as the task prompt describes.",
        brief.map(|b| format!("\n\n{b}")).unwrap_or_default()
    )
}

pub fn steer_text(prompt: &str) -> String {
    let body = [FOLLOWUP_ASK, REOPENED_ASK]
        .iter()
        .find_map(|ask| prompt.strip_suffix(ask))
        .unwrap_or(prompt)
        .trim_end();
    format!(
        "{body}\n\nThis arrived while you were working. Take it into account and keep going. If it asks a question, answer it briefly in your next message. When you finish, end with the full report block as usual."
    )
}

pub fn direct_steer_text(prompt: &str) -> String {
    format!(
        "{}\n\nThis arrived while you were working. Take it into account and keep going. If it asks a question, answer it briefly in your next message.",
        prompt.trim_end()
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct RetriedRun {
    pub run: Option<SessionRun>,
    pub retry_mode: String,
}

#[derive(Debug)]
pub enum RetryRunError {
    NotFound,
    Conflict(String),
    Database(sqlx::Error),
    Internal(anyhow::Error),
}

impl fmt::Display for RetryRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "run not found"),
            Self::Conflict(message) => f.write_str(message),
            Self::Database(error) => error.fmt(f),
            Self::Internal(error) => error.fmt(f),
        }
    }
}

impl StdError for RetryRunError {}

impl From<sqlx::Error> for RetryRunError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<anyhow::Error> for RetryRunError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReplayGap {
    pub head: i64,
    pub missed: i64,
}

pub const REPLAY_MAX_EVENTS: i64 = 2000;

#[derive(Debug, PartialEq, Eq)]
pub enum Resume {
    Replay(i64),
    Resync(i64),
}

impl ReplayGap {
    pub fn resume_from(&self, after_seq: i64) -> Resume {
        if after_seq < 0 || after_seq > self.head || self.missed > REPLAY_MAX_EVENTS {
            Resume::Resync(self.head)
        } else {
            Resume::Replay(after_seq)
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorldEvent {
    pub seq: i64,
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub event_type: String,
    pub agent_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub actor_user_id: Option<Uuid>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
}

impl WorldEvent {
    pub fn channel_id(&self) -> Option<Uuid> {
        self.payload.get("channel_id").and_then(Value::as_str).and_then(|id| Uuid::parse_str(id).ok())
    }

    pub fn in_channel(&self, channels: &HashSet<Uuid>) -> bool {
        self.channel_id().is_some_and(|id| channels.contains(&id))
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: i64,
    pub agent_id: Uuid,
    pub kind: String,
    pub event_seq: Option<i64>,
    pub payload: Value,
    pub status: String,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub author_kind: String,
    pub author_user_id: Option<Uuid>,
    pub author_agent_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    pub session_role: Option<String>,
    #[sqlx(default)]
    pub meta: Value,
    #[sqlx(default)]
    pub edited_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_to: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_count: i32,
    #[sqlx(default)]
    #[serde(default)]
    pub last_reply_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_authors: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentSchedule {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub cron_expression: Option<String>,
    pub run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub enabled: bool,
    #[sqlx(default)]
    pub kind: String,
    #[sqlx(default)]
    pub task_id: Option<Uuid>,
    #[sqlx(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

pub const FOLLOW_UP_HOURS_DEFAULT: i64 = 24;
pub const FOLLOW_UP_HOURS_MAX: i64 = 168;
pub const FOLLOW_UP_MIN_MINUTES: i64 = 5;

#[derive(Debug, Clone, FromRow)]
pub struct OwnedSchedule {
    pub agent: String,
    pub agent_slug: String,
    #[sqlx(flatten)]
    pub schedule: AgentSchedule,
}

impl AgentSchedule {
    pub fn is_heartbeat(&self) -> bool {
        self.kind == "heartbeat"
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorldRule {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub scope: String,
    pub body: String,
    pub enabled: bool,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TaskRef {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub assigned_agent_id: Option<Uuid>,
    pub user_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct CancelledTask {
    pub task: TaskRef,
    pub was_open: bool,
    pub dropped: u64,
    pub running: Vec<Uuid>,
    pub archived: u64,
}

#[derive(Debug, Clone)]
pub struct DeletedTask {
    pub task: TaskRef,
    pub subtasks: usize,
    pub dropped: u64,
    pub running: Vec<Uuid>,
    pub archived: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QuestAssignee {
    Agent(Uuid),
    Person(Uuid),
    Nobody,
}

#[derive(Debug, Clone, Default)]
pub struct QuestEdit {
    pub status: Option<String>,
    pub assignee: Option<QuestAssignee>,
}

#[derive(Debug, Clone)]
pub struct QuestEdited {
    pub status: String,
    pub running: Vec<Uuid>,
}

pub const QUEST_STATUSES: [&str; 8] = ["todo", "planning", "plan_review", "in_progress", "review", "done", "failed", "cancelled"];

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TaskBrief {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub repo: Option<String>,
    pub pr_url: Option<String>,
    pub assigned_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<Uuid>,
    pub result_content: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub user_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default)]
    pub plan_first: bool,
    #[sqlx(default)]
    #[serde(default)]
    pub attempts: Option<i16>,
    #[sqlx(default)]
    #[serde(default)]
    pub plan_waiting: bool,
    #[sqlx(default)]
    #[serde(default)]
    pub pace: Option<String>,
    #[sqlx(skip)]
    #[serde(default)]
    pub milestones: Vec<crate::milestones::Milestone>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub id: Uuid,
    pub name: String,
    pub definition: Value,
    pub scope: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
}

pub fn keyless_definition(definition: &Value) -> bool {
    let refs_only = ["env", "headers"].iter().all(|key| match definition.get(*key) {
        None | Some(Value::Null) => true,
        Some(Value::Object(map)) => map.values().all(|v| {
            v.as_str().is_some_and(|text| {
                let text = text.trim();
                text.starts_with("${secret:") && text.ends_with('}') && text.matches("${").count() == 1
            })
        }),
        Some(_) => false,
    });
    let args_clean = definition
        .get("args")
        .and_then(Value::as_array)
        .is_none_or(|args| args.iter().filter_map(Value::as_str).all(|arg| !looks_secret(arg)));
    let url_clean = definition.get("url").and_then(Value::as_str).is_none_or(|url| mask_url(url) == url);
    refs_only && args_clean && url_clean
}

pub(crate) fn looks_secret(text: &str) -> bool {
    if text.contains("${secret:") {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if ["token", "key", "secret", "password", "bearer", "authorization"].iter().any(|word| lower.contains(word)) && text.contains(['=', ':']) {
        return true;
    }
    let value = text.rsplit(['=', ' ']).next().unwrap_or(text);
    value.len() >= 20
        && !value.contains('/')
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic())
}

pub(crate) fn mask_text(text: &str) -> String {
    match text.find(['=', ':']) {
        Some(at) => format!("{}***", &text[..=at]),
        None => "***".into(),
    }
}

pub(crate) fn mask_url(url: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let base = match base.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            match authority.rsplit_once('@') {
                Some((_, host)) => format!("{scheme}://***@{host}{path}"),
                None => base.to_string(),
            }
        }
        None => base.to_string(),
    };
    match query {
        Some(query) => {
            let masked = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) => format!("{name}=***"),
                    None => pair.to_string(),
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{base}?{masked}")
        }
        None => base,
    }
}

impl McpServerEntry {
    pub fn masked(mut self) -> Self {
        for key in ["env", "headers"] {
            if let Some(map) = self.definition.get_mut(key).and_then(Value::as_object_mut) {
                for v in map.values_mut() {
                    *v = Value::String("***".into());
                }
            }
        }
        if let Some(args) = self.definition.get_mut("args").and_then(Value::as_array_mut) {
            for arg in args.iter_mut() {
                if let Some(text) = arg.as_str().filter(|text| looks_secret(text)) {
                    *arg = Value::String(mask_text(text));
                }
            }
        }
        if let Some(url) = self.definition.get_mut("url") {
            if let Some(text) = url.as_str() {
                *url = Value::String(mask_url(text));
            }
        }
        self
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SkillEntry {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub body: String,
    pub source_url: Option<String>,
    pub scope: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitKind {
    McpServer,
    Skill,
}

#[derive(Debug, Clone, Default)]
pub struct NewEvent {
    pub event_type: String,
    pub agent_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub actor_user_id: Option<Uuid>,
    pub payload: Value,
}

impl NewEvent {
    pub fn new(event_type: &str) -> Self {
        Self {
            event_type: event_type.to_string(),
            payload: Value::Object(Default::default()),
            ..Default::default()
        }
    }

    pub fn agent(mut self, id: Uuid) -> Self {
        self.agent_id = Some(id);
        self
    }

    pub fn session(mut self, id: Uuid) -> Self {
        self.session_id = Some(id);
        self
    }

    pub fn run(mut self, id: Uuid) -> Self {
        self.run_id = Some(id);
        self
    }

    pub fn task(mut self, id: Option<Uuid>) -> Self {
        self.task_id = id;
        self
    }

    pub fn actor(mut self, id: Option<Uuid>) -> Self {
        self.actor_user_id = id;
        self
    }

    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

pub struct NewAgent<'a> {
    pub slug: &'a str,
    pub name: &'a str,
    pub avatar_emoji: &'a str,
    pub cli: &'a str,
    pub model: &'a str,
    pub persona: Option<&'a str>,
    pub role: &'a str,
    pub look: Value,
    pub gear: Value,
    pub visibility: &'a str,
    pub owner_user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RunEvent {
    pub id: i64,
    pub run_id: Uuid,
    pub kind: String,
    pub tool: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SessionBrief {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub role: String,
    pub backend: String,
    pub model: String,
    pub status: String,
    pub task_id: Option<Uuid>,
    pub task_title: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub cost_usd: f64,
    pub tokens: i64,
    pub runs: i64,
    pub running: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    pub working_dir: Option<String>,
    #[sqlx(default)]
    pub base_branch: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub plan_state: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub attempt: Option<i16>,
    #[sqlx(default)]
    #[serde(default)]
    pub attempt_state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentUpdate {
    pub handle: Option<String>,
    pub name: Option<String>,
    pub persona: Option<String>,
    pub cli: Option<String>,
    pub model: Option<String>,
    pub avatar_emoji: Option<String>,
    pub look: Option<Value>,
    pub gear: Option<Value>,
    pub visibility: Option<String>,
    pub tags: Option<Vec<String>>,
    pub expertise: Option<String>,
    pub revoked_powers: Option<Vec<String>>,
    pub approval_mode: Option<String>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub reviewer: Option<Value>,
    #[serde(default)]
    pub paused: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub delete_after_days: Option<Value>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub daily_budget_usd: Option<Value>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub daily_token_budget: Option<Value>,
    #[serde(default)]
    pub quest_context: Option<String>,
}

pub const MAX_DAILY_TOKEN_BUDGET: f64 = 10_000_000_000.0;

impl AgentUpdate {
    pub fn budget_valid(&self) -> bool {
        let money = match &self.daily_budget_usd {
            None | Some(Value::Null) => true,
            Some(v) => v.as_f64().is_some_and(|n| (0.0..=100_000.0).contains(&n)),
        };
        let tokens = match &self.daily_token_budget {
            None | Some(Value::Null) => true,
            Some(v) => v.as_f64().is_some_and(|n| (0.0..=MAX_DAILY_TOKEN_BUDGET).contains(&n)),
        };
        money && tokens
    }

    pub fn changes_budget(&self) -> bool {
        self.daily_budget_usd.is_some() || self.daily_token_budget.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleRename {
    Unchanged,
    Renamed { from: String, to: String },
}

#[derive(Debug)]
pub enum RenameHandleError {
    Invalid,
    Taken(String),
    NotFound,
    Internal(anyhow::Error),
}

impl std::fmt::Display for RenameHandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("handles use lowercase letters, digits and dashes"),
            Self::Taken(handle) => write!(f, "@{handle} is taken"),
            Self::NotFound => f.write_str("agent not found"),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenameHandleError {}

impl From<sqlx::Error> for RenameHandleError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}

fn handle_collision(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|d| {
        d.code().as_deref() == Some("23505") && matches!(d.constraint(), Some("agent_handle_taken" | "agent_profiles_slug_key"))
    })
}

const RESERVED_HANDLES: &[&str] = &["channel", "here", "everyone", "all"];

pub fn normalize_handle(raw: &str) -> Option<String> {
    let handle = raw.trim().to_lowercase();
    let valid = !handle.is_empty()
        && handle.len() <= 64
        && handle.split('-').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()));
    valid.then_some(handle)
}

fn explicit_null<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Value>, D::Error> {
    Ok(Some(Value::deserialize(d)?))
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Upload {
    pub id: Uuid,
    pub mime: String,
    pub name: String,
    pub size: i32,
}

pub struct NewMessage<'a> {
    pub agent_id: Uuid,
    pub author_kind: &'a str,
    pub author_user_id: Option<Uuid>,
    pub author_agent_id: Option<Uuid>,
    pub body: &'a str,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub meta: Value,
}

pub struct NewSession<'a> {
    pub agent_id: Uuid,
    pub role: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
    pub parent_session_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub working_dir: Option<&'a str>,
    pub usage_baseline: Value,
}

pub struct NewRun<'a> {
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub task_id: Option<Uuid>,
    pub kind: &'a str,
    pub status: &'a str,
    pub fork_from: Option<Uuid>,
    pub prompt: &'a str,
    pub timeout_secs: i32,
    pub request: Option<crate::run_request::RunRequest>,
}

pub struct RunOutcome<'a> {
    pub status: &'a str,
    pub output: Option<&'a str>,
    pub error: Option<&'a str>,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

const AGENT_COLUMNS: &str = "id, slug, name, avatar_emoji, cli, model, system_prompt_addendum, persona, role, look, gear, status, status_detail, owner_user_id, visibility, paused_reason, archived_at, archive_delete_at, tags, expertise, created_at, revoked_powers, approval_mode, reviewer, daily_budget_usd, daily_token_budget, agent_profiles.org_id, agent_profiles.quest_context,
    (EXISTS (SELECT 1 FROM users u WHERE u.main_agent_id = agent_profiles.id) OR EXISTS (SELECT 1 FROM org_members om WHERE om.main_agent_id = agent_profiles.id)) AS is_main,
    agent_profiles.runner_id,
    (SELECT r.name FROM world_runners r WHERE r.id = agent_profiles.runner_id) AS runner_name,
    COALESCE((SELECT jsonb_agg(jsonb_build_object(
        'run_id', jr.id, 'session_id', jr.session_id, 'task_id', jr.task_id, 'kind', jr.kind, 'status', jr.status,
        'title', COALESCE(jt.title, js.title), 'activity', jr.activity,
        'waiting', CASE WHEN jr.status = 'queued' AND (jr.runner_id IS NULL OR EXISTS (SELECT 1 FROM world_runners wr WHERE wr.id = jr.runner_id AND wr.last_seen_at > NOW() - INTERVAL '75 seconds')) THEN jr.waiting_for END,
        'started_at', COALESCE(jr.started_at, jr.created_at)) ORDER BY jr.created_at)
      FROM session_runs jr
      JOIN agent_sessions js ON js.id = jr.session_id
      LEFT JOIN agent_tasks jt ON jt.id = jr.task_id
      WHERE jr.agent_id = agent_profiles.id
        AND jr.kind IN ('task', 'review', 'followup', 'publish')
        AND jr.status IN ('queued', 'running')), '[]'::jsonb) AS jobs";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeforeBigWork {
    #[default]
    AskWhenUnclear,
    JustBuild,
    AlwaysShowPlan,
}

impl BeforeBigWork {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AskWhenUnclear => "ask_when_unclear",
            Self::JustBuild => "just_build",
            Self::AlwaysShowPlan => "always_show_plan",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ask_when_unclear" => Some(Self::AskWhenUnclear),
            "just_build" => Some(Self::JustBuild),
            "always_show_plan" => Some(Self::AlwaysShowPlan),
            _ => None,
        }
    }
}

impl fmt::Display for BeforeBigWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod before_big_work_tests {
    use super::{BeforeBigWork, WorldStore};
    use anyhow::Result;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    #[test]
    fn preference_codes_round_trip() {
        for value in [BeforeBigWork::AskWhenUnclear, BeforeBigWork::JustBuild, BeforeBigWork::AlwaysShowPlan] {
            assert_eq!(BeforeBigWork::parse(value.as_str()), Some(value));
            assert_eq!(serde_json::from_str::<BeforeBigWork>(&format!("\"{}\"", value.as_str())).unwrap(), value);
        }
        assert_eq!(BeforeBigWork::default(), BeforeBigWork::AskWhenUnclear);
        assert_eq!(BeforeBigWork::parse("always_ask"), None);
    }

    #[tokio::test]
    async fn preference_defaults_and_changes() -> Result<()> {
        let Ok(url) = std::env::var("QUESTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let options = url.parse::<sqlx::postgres::PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@before-big-work.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        assert_eq!(store.before_big_work(user).await?, BeforeBigWork::AskWhenUnclear);
        for value in [BeforeBigWork::JustBuild, BeforeBigWork::AlwaysShowPlan, BeforeBigWork::AskWhenUnclear] {
            store.set_before_big_work(user, value).await?;
            assert_eq!(store.before_big_work(user).await?, value);
        }
        Ok(())
    }
}

pub type MeRow = (Uuid, Option<String>, Option<String>, Option<Uuid>, Option<String>, Option<String>, String);

pub const OWNER_SPEECH_KINDS: &[&str] = &["message", "channel.message", "channel.mention"];
const OWNER_SPOKE: &str = "payload->>'from_owner' = 'true' AND COALESCE(payload->>'shell_share', 'false') <> 'true'";

#[derive(Clone)]
pub struct WorldStore {
    pool: PgPool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Notifications {
    pub items: Vec<BellItem>,
    pub events: Vec<WorldEvent>,
    pub unread: i64,
    pub unread_seqs: Vec<i64>,
    pub by_agent: std::collections::HashMap<Uuid, i64>,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct BellItem {
    pub id: i64,
    pub seq: i64,
    pub kind: String,
    pub agent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub cleared_at: Option<DateTime<Utc>>,
}

pub enum BellClear {
    All,
    Items(Vec<i64>),
    UpTo(i64),
}

impl WorldStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn emit(&self, event: NewEvent, wake: Option<(Uuid, &str)>) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let seq = emit_in(&mut tx, &event, wake).await?;
        tx.commit().await?;
        Ok(seq)
    }

    pub async fn finished_report_unchanged(
        &self,
        session_id: Uuid,
        report: &Value,
        pr_url: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        let last = sqlx::query_as::<_, (Value, Option<String>)>(
            "SELECT e.payload, t.status FROM world_events e
             JOIN agent_sessions s ON s.id = e.session_id
             LEFT JOIN agent_tasks t ON t.id = s.task_id
             WHERE e.session_id = $1 AND e.type = 'task.finished'
             ORDER BY e.seq DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((payload, quest_status)) = last else { return Ok(false) };
        let reopened = error.is_some() && quest_status.as_deref().is_some_and(|status| status != "failed");
        Ok(!reopened
            && payload.get("report") == Some(report)
            && payload.get("pr_url").cloned().unwrap_or(Value::Null) == json!(pr_url)
            && payload.get("error").cloned().unwrap_or(Value::Null) == json!(error))
    }

    pub async fn create_agent(&self, new: NewAgent<'_>) -> Result<WorldAgent> {
        let sql = format!(
            "INSERT INTO agent_profiles
               (slug, name, avatar_emoji, cli, model, persona, role, look, gear, visibility, owner_user_id, org_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             RETURNING {AGENT_COLUMNS}"
        );
        let mut agent = sqlx::query_as::<_, WorldAgent>(&sql)
            .bind(new.slug)
            .bind(new.name)
            .bind(new.avatar_emoji)
            .bind(new.cli)
            .bind(new.model)
            .bind(new.persona)
            .bind(new.role)
            .bind(new.look)
            .bind(new.gear)
            .bind(new.visibility)
            .bind(new.owner_user_id)
            .bind(new.org_id)
            .fetch_one(&self.pool)
            .await?;
        match crate::runners::RunnerStore::new(self.pool.clone()).assign_default_runner(agent.id).await {
            Ok(Some(runner_id)) => agent.runner_id = Some(runner_id),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, agent_id = %agent.id, "could not pick a default device"),
        }
        Ok(agent)
    }

    pub async fn slug_taken(&self, slug: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_profiles WHERE world_handle_key(slug) = world_handle_key($1))
                OR EXISTS (SELECT 1 FROM agent_slug_aliases WHERE world_handle_key(slug) = world_handle_key($1))",
        )
        .bind(slug)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn user_display_name(&self, user_id: Uuid) -> Result<Option<String>> {
        display_name_on(&self.pool, user_id).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_agent_schedule(
        &self,
        user_id: Uuid,
        agent_id: Uuid,
        name: &str,
        prompt: &str,
        cron_expression: Option<&str>,
        run_at: Option<DateTime<Utc>>,
        kind: &str,
    ) -> Result<AgentSchedule> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "INSERT INTO schedules (user_id, agent_id, name, prompt, cron_expression, run_at, kind)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (agent_id) WHERE kind = 'heartbeat'
             DO UPDATE SET prompt = EXCLUDED.prompt, cron_expression = EXCLUDED.cron_expression,
                           enabled = true, next_run_at = NULL
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind",
        )
        .bind(user_id)
        .bind(agent_id)
        .bind(name)
        .bind(prompt)
        .bind(cron_expression)
        .bind(run_at)
        .bind(kind)
        .fetch_one(&self.pool)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_follow_up_schedule(
        &self,
        user_id: Uuid,
        agent_id: Uuid,
        name: &str,
        prompt: &str,
        cron_expression: &str,
        task_id: Option<Uuid>,
        hours: Option<i64>,
    ) -> Result<AgentSchedule> {
        let hours = hours.unwrap_or(FOLLOW_UP_HOURS_DEFAULT).clamp(1, FOLLOW_UP_HOURS_MAX);
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "INSERT INTO schedules (user_id, agent_id, name, prompt, cron_expression, kind, task_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, 'routine', $6, NOW() + make_interval(hours => $7))
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at",
        )
        .bind(user_id)
        .bind(agent_id)
        .bind(name)
        .bind(prompt)
        .bind(cron_expression)
        .bind(task_id)
        .bind(hours as i32)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn drop_expired_schedules(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM schedules WHERE expires_at <= NOW()").execute(&self.pool).await?.rows_affected())
    }

    pub async fn update_agent_schedule(
        &self,
        agent_id: Uuid,
        id: Uuid,
        prompt: Option<&str>,
        cron_expression: Option<&str>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE schedules SET prompt = COALESCE($3, prompt),
                    cron_expression = CASE WHEN $4::TEXT IS NULL THEN cron_expression ELSE $4 END,
                    run_at = CASE WHEN $4::TEXT IS NULL THEN run_at ELSE NULL END,
                    next_run_at = NULL
             WHERE id = $2 AND agent_id = $1",
        )
        .bind(agent_id)
        .bind(id)
        .bind(prompt)
        .bind(cron_expression)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn fire_schedule(&self, schedule: &AgentSchedule) -> Result<bool> {
        if self.tick_pending(schedule.agent_id, schedule.id).await? {
            return Ok(false);
        }
        let mut payload = serde_json::json!({
            "schedule_id": schedule.id,
            "name": schedule.name,
            "prompt": schedule.prompt,
        });
        if schedule.is_heartbeat() {
            payload["heartbeat"] = serde_json::json!(true);
            payload["quiet"] = serde_json::json!(true);
        }
        self.emit(
            NewEvent::new("tick").agent(schedule.agent_id).payload(payload),
            Some((schedule.agent_id, "tick")),
        )
        .await?;
        Ok(true)
    }

    pub async fn tick_pending(&self, agent_id: Uuid, schedule_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_inbox
                            WHERE agent_id = $1 AND kind = 'tick' AND status IN ('pending', 'processing')
                              AND payload->>'schedule_id' = $2::TEXT)",
        )
        .bind(agent_id)
        .bind(schedule_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn list_agent_schedules(&self, agent_id: Uuid) -> Result<Vec<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at FROM schedules
             WHERE agent_id = $1 AND (expires_at IS NULL OR expires_at > NOW()) ORDER BY created_at",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_agent_schedule_enabled(&self, agent_id: Uuid, id: Uuid, enabled: bool) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE schedules SET enabled = $3, next_run_at = NULL WHERE id = $2 AND agent_id = $1",
        )
        .bind(agent_id)
        .bind(id)
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn owner_schedules(&self, owner: Uuid) -> Result<Vec<OwnedSchedule>> {
        Ok(sqlx::query_as::<_, OwnedSchedule>(
            "SELECT a.name AS agent, a.slug AS agent_slug, s.id, s.agent_id, s.name, s.prompt, s.cron_expression, s.run_at,
                    s.next_run_at, s.enabled, s.kind, s.task_id, s.expires_at
             FROM schedules s JOIN agent_profiles a ON a.id = s.agent_id
             WHERE a.owner_user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
             ORDER BY a.name, s.created_at",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_agent_schedule(&self, agent_id: Uuid, id: Uuid) -> Result<bool> {
        let done = sqlx::query("DELETE FROM schedules WHERE id = $2 AND agent_id = $1")
            .bind(agent_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn rules_for(&self, owner: Option<Uuid>, org: Option<Uuid>) -> Result<Vec<WorldRule>> {
        Ok(sqlx::query_as::<_, WorldRule>(
            "SELECT * FROM world_rules
             WHERE enabled AND ((user_id IS NULL AND org_id = $2) OR (user_id = $1 AND (org_id IS NULL OR org_id = $2)))
             ORDER BY position, created_at",
        )
        .bind(owner)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_rules(&self, user_id: Uuid, org: Option<Uuid>) -> Result<Vec<WorldRule>> {
        Ok(sqlx::query_as::<_, WorldRule>(
            "SELECT * FROM world_rules WHERE (user_id IS NULL AND org_id = $2) OR (user_id = $1 AND (org_id IS NULL OR org_id = $2)) ORDER BY position, created_at",
        )
        .bind(user_id)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn create_rule(&self, user_id: Option<Uuid>, org: Option<Uuid>, scope: &str, body: &str) -> Result<WorldRule> {
        Ok(sqlx::query_as::<_, WorldRule>(
            "INSERT INTO world_rules (user_id, scope, body, position, org_id)
             VALUES ($1, $2, $3, COALESCE((SELECT MAX(position) FROM world_rules), 0) + 10, CASE WHEN $1::uuid IS NULL THEN $4::uuid END)
             RETURNING *",
        )
        .bind(user_id)
        .bind(scope)
        .bind(body)
        .bind(org)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn update_rule(
        &self,
        id: Uuid,
        user_id: Uuid,
        global_org: Option<Uuid>,
        scope: Option<&str>,
        body: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Option<WorldRule>> {
        Ok(sqlx::query_as::<_, WorldRule>(
            "UPDATE world_rules
             SET scope = COALESCE($3, scope), body = COALESCE($4, body),
                 enabled = COALESCE($5, enabled), updated_at = NOW()
             WHERE id = $1 AND (user_id = $2 OR (user_id IS NULL AND org_id = $6))
             RETURNING *",
        )
        .bind(id)
        .bind(user_id)
        .bind(scope)
        .bind(body)
        .bind(enabled)
        .bind(global_org)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn delete_rule(&self, id: Uuid, user_id: Uuid, global_org: Option<Uuid>) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM world_rules WHERE id = $1 AND (user_id = $2 OR (user_id IS NULL AND org_id = $3))")
            .bind(id)
            .bind(user_id)
            .bind(global_org)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn set_session_rules_hash(&self, id: Uuid, hash: &str) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET rules_hash = $2 WHERE id = $1")
            .bind(id)
            .bind(hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_agent(&self, name_or_slug: &str) -> Result<Option<WorldAgent>> {
        self.find_agent_in(None, name_or_slug).await
    }

    pub async fn find_agent_in(&self, org: Option<Uuid>, name_or_slug: &str) -> Result<Option<WorldAgent>> {
        let name_or_slug = name_or_slug.trim().trim_start_matches('@').trim();
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent_profiles
             WHERE ($2::uuid IS NULL OR agent_profiles.org_id = $2)
               AND (id::text = $1 OR lower(slug) = lower($1) OR lower(name) = lower($1)
                OR EXISTS (SELECT 1 FROM agent_slug_aliases aa
                           WHERE aa.agent_id = agent_profiles.id AND lower(aa.slug) = lower($1)))
             ORDER BY (lower(slug) = lower($1)) DESC, (lower(name) = lower($1)) DESC,
                      (id::text = $1 OR lower(slug) = lower($1) OR lower(name) = lower($1)) DESC
             LIMIT 1"
        );
        let found = sqlx::query_as::<_, WorldAgent>(&sql)
            .bind(name_or_slug)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        if found.is_some() {
            return Ok(found);
        }
        self.main_agent_of_person(org, name_or_slug).await
    }

    async fn main_agent_of_person(&self, org: Option<Uuid>, name: &str) -> Result<Option<WorldAgent>> {
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent_profiles
             WHERE ($2::uuid IS NULL OR agent_profiles.org_id = $2)
               AND agent_profiles.id IN (
                   SELECT om.main_agent_id FROM org_members om JOIN users u ON u.id = om.user_id
                   WHERE lower(u.name) = lower($1) AND om.main_agent_id IS NOT NULL
                     AND ($2::uuid IS NULL OR om.org_id = $2))
             LIMIT 2"
        );
        let mut found = sqlx::query_as::<_, WorldAgent>(&sql).bind(name).bind(org).fetch_all(&self.pool).await?;
        Ok(if found.len() == 1 { found.pop() } else { None })
    }

    pub async fn rename_handle(&self, id: Uuid, handle: &str) -> std::result::Result<HandleRename, RenameHandleError> {
        let handle = normalize_handle(handle).ok_or(RenameHandleError::Invalid)?;
        let mut tx = self.pool.begin().await?;
        let current: Option<String> = sqlx::query_scalar("SELECT slug FROM agent_profiles WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(slug) = current else {
            return Err(RenameHandleError::NotFound);
        };
        if slug.eq_ignore_ascii_case(&handle) {
            return Ok(HandleRename::Unchanged);
        }
        let key: String = handle.chars().filter(char::is_ascii_alphanumeric).collect();
        if RESERVED_HANDLES.contains(&key.as_str()) {
            return Err(RenameHandleError::Taken(handle));
        }
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM agent_profiles WHERE world_handle_key(slug) = $1 AND id <> $2)
                OR EXISTS (SELECT 1 FROM agent_slug_aliases WHERE world_handle_key(slug) = $1 AND agent_id <> $2)
                OR EXISTS (SELECT 1 FROM users
                           WHERE world_handle_key(name) = $1 OR world_handle_key(split_part(email, '@', 1)) = $1)",
        )
        .bind(&key)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if taken {
            return Err(RenameHandleError::Taken(handle));
        }
        if let Err(e) = sqlx::query("UPDATE agent_profiles SET slug = $2, updated_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(&handle)
            .execute(&mut *tx)
            .await
        {
            return Err(if handle_collision(&e) { RenameHandleError::Taken(handle) } else { e.into() });
        }
        sqlx::query("DELETE FROM agent_slug_aliases WHERE agent_id = $1 AND world_handle_key(slug) = world_handle_key($2)")
            .bind(id)
            .bind(&handle)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO agent_slug_aliases (slug, agent_id) VALUES ($1, $2)
             ON CONFLICT (slug) DO UPDATE SET agent_id = EXCLUDED.agent_id",
        )
        .bind(&slug)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(HandleRename::Renamed { from: slug, to: handle })
    }

    pub async fn main_agent_for(&self, owner: Uuid) -> Result<Option<WorldAgent>> {
        let sql = format!(
            "SELECT {AGENT_COLUMNS} FROM agent_profiles
             WHERE id = COALESCE(
               (SELECT m.main_agent_id FROM org_members m WHERE m.user_id = $1 AND m.org_id = world_active_org($1)),
               (SELECT u.main_agent_id FROM users u WHERE u.id = $1 AND world_active_org($1) IS NULL))"
        );
        Ok(sqlx::query_as::<_, WorldAgent>(&sql).bind(owner).fetch_optional(&self.pool).await?)
    }

    pub async fn set_main_agent(&self, owner: Uuid, agent_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE users SET main_agent_id = $2 WHERE id = $1")
            .bind(owner)
            .bind(agent_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "UPDATE org_members m SET main_agent_id = $2
             FROM agent_profiles a WHERE a.id = $2 AND m.org_id = a.org_id AND m.user_id = $1",
        )
        .bind(owner)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn has_subscriptions(&self, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_subscriptions WHERE agent_id = $1)")
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn subscribe(&self, agent_id: Uuid, event_types: &[&str]) -> Result<()> {
        for t in event_types {
            sqlx::query(
                "INSERT INTO agent_subscriptions (agent_id, event_type) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(agent_id)
            .bind(t)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn unsubscribe(&self, agent_id: Uuid, event_type: &str) -> Result<()> {
        sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1 AND event_type = $2")
            .bind(agent_id)
            .bind(event_type)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_subscriptions(&self, agent_id: Uuid, add: &[String], remove: &[String]) -> Result<Vec<String>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1 AND event_type = ANY($2) AND match = '{}'::jsonb AND task_id IS NULL")
            .bind(agent_id)
            .bind(remove)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO agent_subscriptions (agent_id, event_type) SELECT $1, t FROM UNNEST($2::text[]) t ON CONFLICT DO NOTHING",
        )
        .bind(agent_id)
        .bind(add)
        .execute(&mut *tx)
        .await?;
        let current = sqlx::query_scalar::<_, String>(
            "SELECT event_type FROM agent_subscriptions WHERE agent_id = $1 AND match = '{}'::jsonb AND task_id IS NULL ORDER BY event_type",
        )
        .bind(agent_id)
        .fetch_all(&mut *tx)
        .await?;
        if current.len() > SUBSCRIPTIONS_MAX {
            anyhow::bail!("at most {SUBSCRIPTIONS_MAX} subscriptions per agent");
        }
        tx.commit().await?;
        Ok(current)
    }

    pub async fn inbox_settle_secs(&self, agent_id: Option<Uuid>, local_only: bool) -> Result<Option<f64>> {
        Ok(sqlx::query_scalar::<_, Option<f64>>(&format!(
            "SELECT MIN(wait)::float8 FROM (
               SELECT EXTRACT(EPOCH FROM LEAST(MAX(i.created_at) + make_interval(secs => {SLACK_SETTLE_SECS}),
                                               MIN(i.created_at) + make_interval(secs => {SLACK_SETTLE_MAX_SECS})) - NOW()) AS wait
               FROM agent_inbox i JOIN agent_profiles a ON a.id = i.agent_id
               WHERE i.status = 'pending' AND {SETTLING_KINDS}
                 AND ($1::uuid IS NULL OR i.agent_id = $1)
                 AND (NOT $2 OR a.runner_id IS NULL)
               GROUP BY i.agent_id
             ) w WHERE wait > 0"
        ))
        .bind(agent_id)
        .bind(local_only)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn subscriptions(&self, agent_id: Uuid) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT event_type FROM agent_subscriptions WHERE agent_id = $1 AND match = '{}'::jsonb AND task_id IS NULL ORDER BY event_type",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn owner_tasks(&self, owner: Uuid, org: Option<Uuid>, agent_id: Option<Uuid>, open_only: bool, limit: i64) -> Result<Vec<TaskBrief>> {
        Ok(sqlx::query_as::<_, TaskBrief>(&format!(
            "SELECT t.id, t.title, t.status, t.repo, t.pr_url, t.assigned_agent_id, t.assignee_user_id, t.result_content, t.updated_at,
                    {} AS plan_waiting
             FROM agent_tasks t
             WHERE ($2::uuid IS NULL OR t.assigned_agent_id = $2) AND ($5::uuid IS NULL OR t.org_id = $5)
               AND (NOT $3 OR t.status NOT IN ('done', 'cancelled'))
               AND (EXISTS (SELECT 1 FROM agent_profiles p WHERE p.id = t.assigned_agent_id AND p.owner_user_id = $1)
                    OR (t.assigned_agent_id IS NULL AND (t.user_id = $1 OR t.assignee_user_id = $1)))
             ORDER BY t.updated_at DESC NULLS LAST LIMIT $4",
            plan_waiting_sql("t")
        ))
        .bind(owner)
        .bind(agent_id)
        .bind(open_only)
        .bind(limit)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn hidden_channels(&self, user: Option<Uuid>, agent: Option<Uuid>) -> Result<HashSet<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT c.id FROM channels c
             WHERE c.kind <> 'channel'
               AND NOT EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id
                               AND ((m.member_kind = 'user' AND m.member_id = $1) OR (m.member_kind = 'agent' AND m.member_id = $2)))",
        )
        .bind(user)
        .bind(agent)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    pub async fn hidden_channels_among(&self, user: Option<Uuid>, agent: Option<Uuid>, events: &[WorldEvent]) -> Result<HashSet<Uuid>> {
        let ids: Vec<Uuid> = events.iter().filter_map(WorldEvent::channel_id).collect::<HashSet<_>>().into_iter().collect();
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT c.id FROM channels c
             WHERE c.id = ANY($3) AND c.kind <> 'channel'
               AND NOT EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id
                               AND ((m.member_kind = 'user' AND m.member_id = $1) OR (m.member_kind = 'agent' AND m.member_id = $2)))",
        )
        .bind(user)
        .bind(agent)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    pub async fn agent_tasks(&self, agent_id: Option<Uuid>, open_only: bool, limit: i64) -> Result<Vec<TaskBrief>> {
        Ok(sqlx::query_as::<_, TaskBrief>(&format!(
            "SELECT id, title, status, repo, pr_url, assigned_agent_id, assignee_user_id, result_content, updated_at, {} AS plan_waiting
             FROM agent_tasks
             WHERE ($1::uuid IS NULL OR assigned_agent_id = $1)
               AND (NOT $2 OR status NOT IN ('done', 'cancelled'))
             ORDER BY updated_at DESC NULLS LAST LIMIT $3",
            plan_waiting_sql("agent_tasks")
        ))
        .bind(agent_id)
        .bind(open_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn handoff_memory(&self, agent_id: Uuid, intro: &str) -> Result<String> {
        let mut lines = vec![format!("{intro} Your recent quests:")];
        for t in self.agent_tasks(Some(agent_id), false, 10).await? {
            let result = t
                .result_content
                .map(|r| format!(" :: {}", r.chars().take(300).collect::<String>()))
                .unwrap_or_default();
            lines.push(format!("- [{}] {}{result}", t.status, t.title));
        }
        Ok(lines.join("\n"))
    }

    pub async fn fresh_start_memory(&self, agent: &WorldAgent, intro: &str) -> Result<String> {
        let notes = self.own_notes_skill(agent.id, agent.owner_user_id).await.ok().flatten().map(|skill| skill.body);
        Ok(with_notes(self.handoff_memory(agent.id, intro).await?, notes.as_deref()))
    }

    pub async fn set_session_asked_by(&self, id: Uuid, asked_by: Value) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET asked_by = $2 WHERE id = $1")
            .bind(id)
            .bind(asked_by)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn start_aside(
        &self,
        agent_id: Uuid,
        question: &str,
        asked_by: Value,
        choice: &AsideChoice,
    ) -> Result<SessionRun> {
        self.start_aside_with(agent_id, question, asked_by, None, choice).await
    }

    pub async fn start_aside_with(
        &self,
        agent_id: Uuid,
        question: &str,
        asked_by: Value,
        prompt: Option<String>,
        choice: &AsideChoice,
    ) -> Result<SessionRun> {
        let guest = prompt.is_some();
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let main = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions
             WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR SHARE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .filter(|m| {
            guest
                || m.cli_session_id.is_some()
                || choice.cli.as_deref().is_some_and(|cli| cli != m.backend)
        })
        .ok_or_else(|| anyhow::anyhow!("this agent has no memory yet; message them first"))?;
        let asker = asked_by
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("someone")
            .to_string();
        let asker_user = asked_by
            .get("user_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Uuid>().ok());
        let spoken = asked_by.get("spoken").and_then(Value::as_bool) == Some(true);
        let mut asked_by = asked_by;
        if let Some(obj) = asked_by.as_object_mut() {
            obj.insert("question".into(), Value::String(question.to_string()));
        }
        let switches_cli = !guest && choice.cli.as_deref().is_some_and(|cli| cli != main.backend);
        if switches_cli && choice.model.is_none() {
            anyhow::bail!("pick a model for {}", choice.cli.as_deref().unwrap_or_default());
        }
        let forks = !switches_cli && (!guest || TOOLLESS_BACKENDS.contains(&main.backend.as_str()));
        let (backend, model, baseline) = if switches_cli {
            (choice.cli.clone().unwrap_or_default(), choice.model.clone().unwrap_or_default(), json!({}))
        } else if forks {
            let model = if guest { None } else { choice.model.clone() };
            (main.backend.clone(), model.unwrap_or_else(|| main.model.clone()), main.usage_baseline.clone())
        } else {
            (GUEST_BACKEND.to_string(), GUEST_MODEL.to_string(), json!({}))
        };
        let persona: Option<(String, Option<String>)> = if forks {
            None
        } else {
            sqlx::query_as("SELECT name, persona FROM agent_profiles WHERE id = $1")
                .bind(agent_id)
                .fetch_optional(&mut *tx)
                .await?
        };
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, parent_session_id, working_dir, usage_baseline, asked_by)
             VALUES ($1, 'aside', $2, $3, $4, $5, $6, $7)
             RETURNING *",
        )
        .bind(agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(main.id)
        .bind(main.working_dir.as_deref())
        .bind(&baseline)
        .bind(asked_by)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(user_id) = asker_user {
            sqlx::query(
                "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, session_id, meta)
                 VALUES ($1, 'user', $2, $3, $4, $5)",
            )
            .bind(agent_id)
            .bind(user_id)
            .bind(question)
            .bind(session.id)
            .bind(if spoken { json!({ "spoken": true }) } else { json!({}) })
            .execute(&mut *tx)
            .await?;
        }
        let prompt = prompt.unwrap_or_else(|| {
            let source = if switches_cli {
                "This is a quick side conversation on another model, so you only have the notes below, not your full memory"
            } else {
                "This is a side conversation forked from your memory"
            };
            format!(
                "[aside] {asker} pulled you aside and asks:\n{question}\n\n{source}. Answer in your own voice, in a few sentences. Your Loka tools work here as in your main conversation: when asked, you can create and assign quests, message agents, check on work or change settings within your powers. Do not do coding or long work yourself in this aside; hand it off with create_task. Do not add an actions block, it is ignored here."
            )
        });
        let prompt = crate::voice::with_note(prompt, &json!({ "spoken": spoken }));
        let memory = if switches_cli {
            let notes = self.handoff_memory(agent_id, "Your notes from your main mind.").await?;
            format!("\n\n{notes}")
        } else {
            String::new()
        };
        let prompt = match persona {
            Some((name, persona)) => format!(
                "You are {name}, a resident of the Loka. {}\nYou do not have your usual memory in this conversation.{memory}\n\n{prompt}",
                persona.unwrap_or_default()
            ),
            None => prompt,
        };
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, kind, status, fork_from, prompt, timeout_secs, request)
             VALUES ($1, $2, 'aside', 'queued', $3, $4, 600, $5)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at, request",
        )
        .bind(session.id)
        .bind(agent_id)
        .bind(forks.then_some(main.id))
        .bind(prompt)
        .bind(crate::run_request::RunRequest::asked(&asker, Some(question)).value())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn start_lean_aside(&self, agent_id: Uuid, asked_by: Value, prompt: &str) -> Result<SessionRun> {
        let mut tx = self.pool.begin().await?;
        let main = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions
             WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR SHARE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("your main agent has not started yet; say hello to it first"))?;
        let (backend, model) = match main.backend.as_str() {
            "omp" => ("omp".to_string(), main.model.clone()),
            _ => (GUEST_BACKEND.to_string(), GUEST_MODEL.to_string()),
        };
        let mut asked_by = asked_by;
        if let Some(obj) = asked_by.as_object_mut() {
            obj.insert("lean".into(), Value::Bool(true));
        }
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, parent_session_id, working_dir, usage_baseline, asked_by)
             VALUES ($1, 'aside', $2, $3, $4, $5, '{}'::jsonb, $6)
             RETURNING *",
        )
        .bind(agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(main.id)
        .bind(main.working_dir.as_deref())
        .bind(asked_by)
        .fetch_one(&mut *tx)
        .await?;
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, kind, status, fork_from, prompt, timeout_secs)
             VALUES ($1, $2, 'aside', 'queued', NULL, $3, 600)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at",
        )
        .bind(session.id)
        .bind(agent_id)
        .bind(prompt)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn spend_blocker(&self, agent_id: Uuid) -> Result<Option<String>> {
        let row: Option<(Option<String>, bool)> = sqlx::query_as(
            "SELECT paused_reason, world_agent_over_budget(id) IS NOT NULL FROM agent_profiles WHERE id = $1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            None => Some("that agent is gone".into()),
            Some((Some(reason), _)) => Some(format!("your main agent is paused: {reason}")),
            Some((None, true)) => Some("your main agent has used its daily budget; try again tomorrow or raise the budget".into()),
            Some((None, false)) => None,
        })
    }

    pub async fn kit_creator(&self, kind: KitKind, id: Uuid) -> Result<Option<(String, Option<Uuid>)>> {
        let sql = match kind {
            KitKind::McpServer => "SELECT scope, created_by FROM world_mcp_servers WHERE id = $1",
            KitKind::Skill => "SELECT scope, created_by FROM world_skills WHERE id = $1",
        };
        Ok(sqlx::query_as::<_, (String, Option<Uuid>)>(sql).bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn upsert_mcp_server(
        &self,
        name: &str,
        definition: &Value,
        scope: &str,
        created_by: Option<Uuid>,
        org: Option<Uuid>,
    ) -> Result<McpServerEntry> {
        sqlx::query_as::<_, McpServerEntry>(
            "INSERT INTO world_mcp_servers (name, definition, scope, created_by, org_id) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (name) DO UPDATE SET definition = EXCLUDED.definition, scope = EXCLUDED.scope, enabled = TRUE
             WHERE world_mcp_servers.created_by IS NOT DISTINCT FROM EXCLUDED.created_by
               AND world_mcp_servers.org_id = EXCLUDED.org_id
             RETURNING id, name, definition, scope, enabled, created_at",
        )
        .bind(name)
        .bind(definition)
        .bind(scope)
        .bind(created_by)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("MCP server name belongs to another owner"))
    }

    pub async fn mcp_server_usable_by(&self, server_id: Uuid, owner: Option<Uuid>, org: Option<Uuid>) -> Result<bool> {
        let row: Option<(String, Option<Uuid>, Value)> =
            sqlx::query_as("SELECT scope, created_by, definition FROM world_mcp_servers WHERE id = $1 AND org_id = $2")
                .bind(server_id)
                .bind(org)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some_and(|(scope, created_by, definition)| {
            scope == "global" || (owner.is_some() && created_by == owner) || keyless_definition(&definition)
        }))
    }

    pub async fn list_mcp_servers(&self, org: Option<Uuid>) -> Result<Vec<McpServerEntry>> {
        Ok(sqlx::query_as::<_, McpServerEntry>(
            "SELECT m.id, m.name, m.definition, m.scope, m.enabled, m.created_at,
                    (SELECT MIN(o.name) FROM world_mcp_servers o
                      WHERE o.org_id = m.org_id AND o.id <> m.id AND o.name < m.name
                        AND o.created_by IS NOT DISTINCT FROM m.created_by
                        AND (o.definition - 'shared') = (m.definition - 'shared')) AS duplicate_of
             FROM world_mcp_servers m WHERE m.org_id = $1 ORDER BY m.name",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn runner_owner(&self, runner_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>("SELECT owner_user_id FROM world_runners WHERE id = $1")
            .bind(runner_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn without_mcp_secrets(
        &self,
        agent_id: Uuid,
        environment: std::collections::BTreeMap<String, String>,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        if environment.is_empty() {
            return Ok(environment);
        }
        let servers = self.mcp_servers_for(agent_id).await?;
        Ok(crate::environment::without_secret_references(environment, servers.iter().map(|entry| &entry.definition)))
    }

    pub async fn mcp_servers_for(&self, agent_id: Uuid) -> Result<Vec<McpServerEntry>> {
        Ok(sqlx::query_as::<_, McpServerEntry>(
            "SELECT id, name, definition, scope, enabled, created_at FROM (
               SELECT DISTINCT ON (m.definition - 'shared') m.id, m.name, m.definition, m.scope, m.enabled, m.created_at
               FROM world_mcp_servers m
               WHERE m.enabled AND m.org_id = (SELECT p.org_id FROM agent_profiles p WHERE p.id = $1) AND (m.scope = 'global'
                 OR EXISTS (SELECT 1 FROM agent_mcp_servers a WHERE a.server_id = m.id AND a.agent_id = $1))
               ORDER BY m.definition - 'shared', m.name
             ) kit ORDER BY name",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_mcp_server(&self, id: Uuid, org: Option<Uuid>) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM world_mcp_servers WHERE id = $1 AND org_id = $2")
            .bind(id)
            .bind(org)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_skill(
        &self,
        name: &str,
        description: &str,
        body: &str,
        source_url: Option<&str>,
        scope: &str,
        created_by: Option<Uuid>,
        org: Option<Uuid>,
    ) -> Result<SkillEntry> {
        sqlx::query_as::<_, SkillEntry>(
            "INSERT INTO world_skills (name, description, body, source_url, scope, created_by, org_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (name) DO UPDATE SET description = EXCLUDED.description, body = EXCLUDED.body,
               source_url = EXCLUDED.source_url, scope = EXCLUDED.scope, enabled = TRUE, updated_at = NOW()
             WHERE world_skills.created_by IS NOT DISTINCT FROM EXCLUDED.created_by
               AND world_skills.org_id = EXCLUDED.org_id
             RETURNING id, name, description, body, source_url, scope, enabled, created_at",
        )
        .bind(name)
        .bind(description)
        .bind(body)
        .bind(source_url)
        .bind(scope)
        .bind(created_by)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("skill name belongs to another owner"))
    }

    pub async fn list_skills(&self) -> Result<Vec<SkillEntry>> {
        Ok(sqlx::query_as::<_, SkillEntry>(
            "SELECT id, name, description, body, source_url, scope, enabled, created_at FROM world_skills ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn visible_skills(&self, user: Option<Uuid>, admin: bool, org: Option<Uuid>) -> Result<Vec<SkillEntry>> {
        Ok(sqlx::query_as::<_, SkillEntry>(
            "SELECT id, name, description, body, source_url, scope, enabled, created_at FROM world_skills s
             WHERE s.org_id = $3 AND ($2 OR s.scope = 'global' OR s.created_by = $1
               OR EXISTS (SELECT 1 FROM agent_skills a JOIN agent_profiles p ON p.id = a.agent_id
                          WHERE a.skill_id = s.id AND p.org_id = $3
                            AND (p.owner_user_id = $1 OR (p.visibility = 'public' AND s.name NOT LIKE '%-notes'))))
             ORDER BY name",
        )
        .bind(user)
        .bind(admin)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn skills_for(&self, agent_id: Uuid) -> Result<Vec<SkillEntry>> {
        Ok(sqlx::query_as::<_, SkillEntry>(
            "SELECT id, name, description, body, source_url, scope, enabled, created_at FROM world_skills s
             WHERE s.enabled AND ((s.scope = 'global' AND s.org_id = (SELECT p.org_id FROM agent_profiles p WHERE p.id = $1))
               OR EXISTS (SELECT 1 FROM agent_skills a WHERE a.skill_id = s.id AND a.agent_id = $1))
             ORDER BY name",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn own_notes_skill(&self, agent_id: Uuid, owner: Option<Uuid>) -> Result<Option<SkillEntry>> {
        let found = sqlx::query_as::<_, SkillEntry>(&format!(
            "SELECT id, name, description, body, source_url, scope, enabled, created_at FROM world_skills s
             WHERE {OWN_NOTES} ORDER BY s.created_at LIMIT 2"
        ))
        .bind(agent_id)
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        if found.len() > 1 {
            anyhow::bail!(
                "you have more than one notes skill ({}, {}); ask a person to remove one",
                found[0].name,
                found[1].name
            );
        }
        Ok(found.into_iter().next())
    }

    pub async fn save_own_notes(
        &self,
        agent_id: Uuid,
        owner: Option<Uuid>,
        existing: Option<Uuid>,
        name: &str,
        description: &str,
        body: &str,
    ) -> Result<SkillEntry> {
        let columns = "id, name, description, body, source_url, scope, enabled, created_at";
        if let Some(id) = existing {
            return sqlx::query_as::<_, SkillEntry>(&format!(
                "UPDATE world_skills s SET description = $4, body = $5, enabled = TRUE, updated_at = NOW()
                 WHERE s.id = $3 AND {OWN_NOTES} RETURNING {columns}"
            ))
            .bind(agent_id)
            .bind(owner)
            .bind(id)
            .bind(description)
            .bind(body)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("your notes skill changed hands; try again"));
        }
        let mut tx = self.pool.begin().await?;
        let skill = sqlx::query_as::<_, SkillEntry>(&format!(
            "INSERT INTO world_skills (name, description, body, scope, created_by, org_id)
             VALUES ($1, $2, $3, 'agent', $4, (SELECT org_id FROM agent_profiles WHERE id = $5))
             ON CONFLICT (name) DO NOTHING RETURNING {columns}"
        ))
        .bind(name)
        .bind(description)
        .bind(body)
        .bind(owner)
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("a skill named {name} already exists and is not yours to edit"))?;
        sqlx::query("INSERT INTO agent_skills (agent_id, skill_id) VALUES ($1, $2)")
            .bind(agent_id)
            .bind(skill.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(skill)
    }

    pub async fn delete_skill(&self, id: Uuid, org: Option<Uuid>) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM world_skills WHERE id = $1 AND org_id = $2")
            .bind(id)
            .bind(org)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn attach_mcp_server(&self, agent_id: Uuid, server_id: Uuid, attach: bool) -> Result<()> {
        let sql = if attach {
            "INSERT INTO agent_mcp_servers (agent_id, server_id)
             SELECT a.id, m.id FROM agent_profiles a JOIN world_mcp_servers m ON m.id = $2 AND m.org_id = a.org_id
             WHERE a.id = $1 ON CONFLICT DO NOTHING"
        } else {
            "DELETE FROM agent_mcp_servers WHERE agent_id = $1 AND server_id = $2"
        };
        sqlx::query(sql).bind(agent_id).bind(server_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn attach_skill(&self, agent_id: Uuid, skill_id: Uuid, attach: bool) -> Result<()> {
        let sql = if attach {
            "INSERT INTO agent_skills (agent_id, skill_id)
             SELECT a.id, s.id FROM agent_profiles a JOIN world_skills s ON s.id = $2 AND s.org_id = a.org_id
             WHERE a.id = $1 ON CONFLICT DO NOTHING"
        } else {
            "DELETE FROM agent_skills WHERE agent_id = $1 AND skill_id = $2"
        };
        sqlx::query(sql).bind(agent_id).bind(skill_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_agent(&self, id: Uuid, u: &AgentUpdate) -> Result<Option<WorldAgent>> {
        let sql = format!(
            "UPDATE agent_profiles SET
               name = COALESCE($2, name), persona = COALESCE($3, persona), cli = COALESCE($4, cli),
               model = COALESCE($5, model), avatar_emoji = COALESCE($6, avatar_emoji),
               look = COALESCE($7, look), gear = COALESCE($8, gear), visibility = COALESCE($9, visibility),
               tags = COALESCE($10, tags), expertise = COALESCE($11, expertise),
               revoked_powers = COALESCE($12, revoked_powers),
               approval_mode = COALESCE($13, approval_mode),
               reviewer = CASE WHEN $14::jsonb IS NULL THEN reviewer WHEN $14::jsonb = 'null'::jsonb THEN NULL ELSE $14::jsonb END,
               daily_budget_usd = CASE WHEN $15::jsonb IS NULL THEN daily_budget_usd
                                       WHEN jsonb_typeof($15::jsonb) <> 'number' OR ($15::jsonb #>> '{{}}')::float8 <= 0 THEN NULL
                                       ELSE ($15::jsonb #>> '{{}}')::float8 END,
               daily_token_budget = CASE WHEN $16::jsonb IS NULL THEN daily_token_budget
                                         WHEN jsonb_typeof($16::jsonb) <> 'number' OR ($16::jsonb #>> '{{}}')::float8 < 1 THEN NULL
                                         ELSE round(($16::jsonb #>> '{{}}')::float8)::int8 END,
               quest_context = COALESCE($17, quest_context),
               updated_at = NOW()
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        let trim = |v: &Option<String>| v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        Ok(sqlx::query_as::<_, WorldAgent>(&sql)
            .bind(id)
            .bind(trim(&u.name))
            .bind(trim(&u.persona))
            .bind(trim(&u.cli))
            .bind(trim(&u.model))
            .bind(trim(&u.avatar_emoji))
            .bind(&u.look)
            .bind(&u.gear)
            .bind(trim(&u.visibility))
            .bind(u.tags.as_ref().map(|t| crate::expertise::merge_tags(&[], t)))
            .bind(trim(&u.expertise))
            .bind(u.revoked_powers.as_ref().map(|p| crate::powers::normalize(p)))
            .bind(trim(&u.approval_mode))
            .bind(&u.reviewer)
            .bind(&u.daily_budget_usd)
            .bind(&u.daily_token_budget)
            .bind(u.quest_context.as_deref().and_then(|c| crate::context::QuestContext::parse(c).ok()).map(|c| c.as_str()))
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn set_session_model(&self, id: Uuid, model: &str) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET model = $2, updated_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(model)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn emit_progress(&self, agent_id: Uuid, run_id: Uuid, run_kind: &str, text: &str) -> Result<()> {
        self.emit(
            NewEvent::new("agent.progress")
                .agent(agent_id)
                .run(run_id)
                .payload(serde_json::json!({ "text": text, "run_kind": run_kind })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn set_run_activity(&self, run: &SessionRun, text: &str) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE session_runs SET activity = $2 WHERE id = $1 AND activity IS DISTINCT FROM $2",
        )
        .bind(run.id)
        .bind(text)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed > 0 {
            self.emit(
                NewEvent::new("run.activity")
                    .agent(run.agent_id)
                    .session(run.session_id)
                    .run(run.id)
                    .task(run.task_id)
                    .payload(serde_json::json!({ "text": text, "run_kind": run.kind, "task_id": run.task_id })),
                None,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn insert_run_event(&self, run_id: Uuid, kind: &str, tool: Option<&str>, body: &str) -> Result<()> {
        sqlx::query("INSERT INTO session_run_events (run_id, kind, tool, body) VALUES ($1, $2, $3, $4)")
            .bind(run_id)
            .bind(kind)
            .bind(tool)
            .bind(body)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn run_events(&self, run_id: Uuid, limit: i64) -> Result<Vec<RunEvent>> {
        self.run_events_after(run_id, None, limit).await
    }

    pub async fn run_events_after(&self, run_id: Uuid, after: Option<i64>, limit: i64) -> Result<Vec<RunEvent>> {
        Ok(sqlx::query_as::<_, RunEvent>(
            "SELECT * FROM session_run_events WHERE run_id = $1 AND id > COALESCE($2, 0) ORDER BY id LIMIT $3",
        )
        .bind(run_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn run_events_tail(&self, run_id: Uuid, limit: i64) -> Result<Vec<RunEvent>> {
        let mut rows = sqlx::query_as::<_, RunEvent>("SELECT * FROM session_run_events WHERE run_id = $1 ORDER BY id DESC LIMIT $2")
            .bind(run_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.reverse();
        Ok(rows)
    }

    pub async fn find_owner_runs(&self, owner: Uuid, prefix: &str) -> Result<Vec<SessionRun>> {
        Ok(sqlx::query_as::<_, SessionRun>(
            "SELECT r.id, r.session_id, r.agent_id, r.task_id, r.kind, r.status, r.fork_from, r.prompt, r.output, r.error,
                    r.timeout_secs, r.cost_usd, r.input_tokens, r.cached_input_tokens, r.output_tokens,
                    r.created_at, r.started_at, r.finished_at
             FROM session_runs r JOIN agent_profiles p ON p.id = r.agent_id
             WHERE p.owner_user_id = $1 AND r.id::text LIKE $2 || '%'
             ORDER BY r.created_at DESC LIMIT 5",
        )
        .bind(owner)
        .bind(prefix.trim().to_lowercase())
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn tokens_today(&self, agent_ids: &[Uuid]) -> Result<Vec<(Uuid, i64)>> {
        Ok(sqlx::query_as::<_, (Uuid, i64)>("SELECT id, world_agent_tokens_today(id) FROM agent_profiles WHERE id = ANY($1)")
            .bind(agent_ids)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn metered(&self, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT a.daily_budget_usd IS NOT NULL OR a.daily_token_budget IS NOT NULL
                    OR o.daily_budget_usd IS NOT NULL OR o.daily_token_budget IS NOT NULL
             FROM agent_profiles a LEFT JOIN orgs o ON o.id = a.org_id WHERE a.id = $1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false))
    }

    pub async fn budget_pause(&self, agent_id: Uuid) -> Result<Option<String>> {
        let reason: Option<String> = sqlx::query_scalar("SELECT paused_reason FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok(reason.filter(|r| crate::withdraw::is_budget_pause(r)))
    }

    pub async fn record_live_usage(&self, run_id: Uuid, cost_usd: f64, input_tokens: i64, cached_input_tokens: i64, output_tokens: i64) -> Result<crate::withdraw::StoppedWork> {
        let agent: Option<Uuid> = sqlx::query_scalar(
            "UPDATE session_runs SET cost_usd = GREATEST(cost_usd, $2), input_tokens = GREATEST(input_tokens, $3),
               cached_input_tokens = GREATEST(cached_input_tokens, $4), output_tokens = GREATEST(output_tokens, $5)
             WHERE id = $1 AND status = 'running'
               AND (cost_usd < $2 OR input_tokens < $3 OR cached_input_tokens < $4 OR output_tokens < $5)
             RETURNING agent_id",
        )
        .bind(run_id)
        .bind(cost_usd.max(0.0))
        .bind(input_tokens.max(0))
        .bind(cached_input_tokens.max(0))
        .bind(output_tokens.max(0))
        .fetch_optional(&self.pool)
        .await?;
        let Some(agent) = agent else {
            return Ok(Default::default());
        };
        if self.budget_pause(agent).await?.is_none() {
            return Ok(Default::default());
        }
        crate::withdraw::stop_budget_paused_work(&self.pool, Some(agent)).await
    }

    pub async fn spent_today(&self, agent_ids: &[Uuid]) -> Result<Vec<(Uuid, f64)>> {
        Ok(sqlx::query_as::<_, (Uuid, f64)>("SELECT id, world_agent_spent_today(id) FROM agent_profiles WHERE id = ANY($1)")
            .bind(agent_ids)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn owner_asked_now(&self, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM agent_inbox WHERE agent_id = $1 AND status = 'processing' AND kind = ANY($2) AND {OWNER_SPOKE})
                   AND NOT EXISTS (SELECT 1 FROM agent_inbox WHERE agent_id = $1 AND status = 'processing' AND {})",
                crate::outside::SQL_OUTSIDE_KINDS
            ),
        )
        .bind(agent_id)
        .bind(OWNER_SPEECH_KINDS)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn outside_led_now(&self, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM agent_inbox WHERE agent_id = $1 AND status = 'processing' AND {})",
            crate::outside::SQL_OUTSIDE_KINDS
        ))
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn run_agent_id(&self, id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>("SELECT agent_id FROM session_runs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn run_text(&self, id: Uuid) -> Result<Option<(String, Option<String>)>> {
        Ok(sqlx::query_as::<_, (String, Option<String>)>("SELECT prompt, output FROM session_runs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn session_briefs(&self, agent_id: Option<Uuid>, limit: i64) -> Result<Vec<SessionBrief>> {
        Ok(sqlx::query_as::<_, SessionBrief>(
            "SELECT s.id, s.agent_id, s.role, s.backend, s.model, s.status, s.task_id, t.title AS task_title,
                    COALESCE(s.title, t.title) AS title, s.summary,
                    COALESCE((SELECT SUM(cost_usd) FROM session_runs r WHERE r.session_id = s.id), 0)::float8 AS cost_usd,
                    COALESCE((SELECT SUM(input_tokens + cached_input_tokens + output_tokens) FROM session_runs r WHERE r.session_id = s.id), 0)::int8 AS tokens,
                    (SELECT COUNT(*) FROM session_runs r WHERE r.session_id = s.id) AS runs,
                    EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')) AS running,
                    s.created_at, s.updated_at, s.working_dir, t.target_branch AS base_branch
             FROM agent_sessions s LEFT JOIN agent_tasks t ON t.id = s.task_id
             WHERE ($1::uuid IS NULL OR s.agent_id = $1)
             ORDER BY s.updated_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn find_session_for(&self, agent_id: Option<Uuid>, owner: Option<Uuid>, id_or_prefix: &str) -> Result<Option<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "SELECT s.* FROM agent_sessions s
             WHERE ($1::uuid IS NULL OR s.agent_id = $1)
               AND ($2::uuid IS NULL OR EXISTS (SELECT 1 FROM agent_profiles p WHERE p.id = s.agent_id AND p.owner_user_id = $2))
               AND s.id::text LIKE $3 || '%'
             ORDER BY s.updated_at DESC LIMIT 1",
        )
        .bind(agent_id)
        .bind(owner)
        .bind(id_or_prefix.trim().to_lowercase())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn start_direct(
        &self,
        agent_id: Uuid,
        folder: Uuid,
        runner: Uuid,
        folder_name: &str,
        asked_by: Value,
        choice: &AsideChoice,
    ) -> Result<AgentSession> {
        let (cli, model): (String, String) = sqlx::query_as("SELECT cli, model FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await?;
        let (backend, model) = match (&choice.cli, &choice.model) {
            (Some(other), None) if *other != cli => anyhow::bail!("pick a model for {other}"),
            (Some(other), Some(model)) => (other.clone(), model.clone()),
            (None, Some(model)) => (cli, model.clone()),
            _ => (cli, model),
        };
        let title: String = folder_name.trim().chars().take(80).collect();
        let mut asked_by = asked_by;
        if let Some(obj) = asked_by.as_object_mut() {
            obj.insert("folder".into(), json!({ "id": folder, "name": title, "runner_id": runner }));
        }
        Ok(sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, working_dir, usage_baseline, asked_by, title, runner_id)
             VALUES ($1, 'direct', $2, $3, $4, '{}'::jsonb, $5, $6, $7)
             RETURNING *",
        )
        .bind(agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(direct_key(folder))
        .bind(asked_by)
        .bind(if title.is_empty() { None } else { Some(title) })
        .bind(runner)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn start_followup(&self, session: &AgentSession, text: &str, from: &str, interrupt: bool) -> Result<SessionRun> {
        if session.role == "direct" {
            return self.queue_direct(session, text, interrupt).await;
        }
        if !matches!(session.role.as_str(), "task" | "review") {
            anyhow::bail!("you can only talk to task or review sessions");
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(session.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let current = sqlx::query_as::<_, (String, Option<String>, String, Option<Uuid>, Option<bool>)>(
            "SELECT role, cli_session_id, status, task_id, (asked_by->>'manual')::boolean
             FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        let clarification = sqlx::query_scalar::<_, Option<String>>("SELECT plan_state FROM agent_sessions WHERE id = $1")
            .bind(session.id)
            .fetch_one(&mut *tx)
            .await?;
        if clarification.as_deref() == Some("clarifying") {
            anyhow::bail!("that quest is waiting for answers to its clarification card. Answer the card instead of messaging the session");
        }
        if current.2 == "archived" {
            match current.3 {
                Some(task_id) => anyhow::bail!(
                    "archived sessions cannot receive follow-ups. Resume the quest instead: start_task on task_id {task_id} continues it in a fresh session on the same branch, with the last report handed over"
                ),
                None => anyhow::bail!("archived sessions cannot receive follow-ups"),
            }
        }
        let publishing = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE kind = 'publish' AND status IN ('queued', 'running')
                 AND (session_id = $1 OR task_id = $2)
             )",
        )
        .bind(session.id)
        .bind(current.3)
        .fetch_one(&mut *tx)
        .await?;
        if publishing {
            anyhow::bail!("that quest's work is being published; wait for the publish to finish, then message it again");
        }
        let waiting_on_siblings = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM agent_sessions s
               JOIN agent_sessions o ON o.task_id = s.task_id AND o.id <> s.id
                 AND o.attempt IS NOT NULL AND o.attempt_state = 'running'
               WHERE s.id = $1 AND s.attempt_state = 'finished'
             )",
        )
        .bind(session.id)
        .fetch_one(&mut *tx)
        .await?;
        if waiting_on_siblings {
            anyhow::bail!("that attempt already finished and the other attempts are still running; wait for the best one to be picked, then message it again");
        }
        let running = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE session_id = $1 AND status = 'running' AND kind = $2
             )",
        )
        .bind(session.id)
        .bind(&current.0)
        .fetch_one(&mut *tx)
        .await?;
        let task_finished = match current.3 {
            Some(task_id) => sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM agent_tasks WHERE id = $1 AND status <> 'in_progress')",
            )
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?,
            None => false,
        };
        let review_active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs r
               WHERE r.kind = 'review' AND r.status IN ('queued', 'running')
                 AND (r.task_id = $2 OR r.session_id IN (SELECT id FROM agent_sessions WHERE parent_session_id = $1))
             )",
        )
        .bind(session.id)
        .bind(current.3)
        .fetch_one(&mut *tx)
        .await?;
        let ready = if current.0 == "review" {
            (current.4 == Some(true) || task_finished) && (running || current.1.is_some())
        } else {
            running || current.1.is_some() && (current.2 == "active" || task_finished || !review_active)
        };
        if !ready {
            if current.1.is_some() || current.0 == "review" && running {
                anyhow::bail!("that session is being reviewed; wait for the review to finish or stop the review run, then message it again");
            }
            match current.3 {
                Some(task_id) => anyhow::bail!(
                    "that session has not started yet. If the quest stalled before it began, start_task on task_id {task_id} opens a fresh session for it"
                ),
                None => anyhow::bail!("that session has not started yet"),
            }
        }
        let reopened = current.0 == "task" && current.2 == "failed" && reopen_failed_session(&mut tx, session.id, current.3).await?;
        let (plan_state, plan_first) = sqlx::query_as::<_, (Option<String>, bool)>(
            "SELECT s.plan_state, COALESCE(t.plan_first, FALSE)
             FROM agent_sessions s LEFT JOIN agent_tasks t ON t.id = s.task_id WHERE s.id = $1",
        )
        .bind(session.id)
        .fetch_one(&mut *tx)
        .await?;
        let plan_run_active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM session_runs WHERE session_id = $1 AND kind IN ('task', 'followup') AND status IN ('queued', 'running'))",
        )
        .bind(session.id)
        .fetch_one(&mut *tx)
        .await?;
        let plan_skipped = !plan_first && !plan_run_active && matches!(plan_state.as_deref(), Some("awaiting" | "planning"));
        let replan = !plan_skipped
            && sqlx::query("UPDATE agent_sessions SET plan_state = 'planning' WHERE id = $1 AND plan_state = 'awaiting'")
                .bind(session.id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0;
        if plan_skipped {
            sqlx::query("UPDATE agent_sessions SET plan_state = 'approved' WHERE id = $1")
                .bind(session.id)
                .execute(&mut *tx)
                .await?;
        }
        let ask = if plan_skipped {
            crate::quests::PLAN_SKIPPED
        } else if replan {
            crate::quests::PLAN_AGAIN
        } else if reopened {
            REOPENED_ASK
        } else {
            FOLLOWUP_ASK
        };
        let prompt = format!("[follow-up from {from}]\n{text}\n\n{ask}");
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, interrupt, request)
             VALUES ($1, $2, $3, 'followup', 'queued', $4, 3600, $5, $6)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at, request",
        )
        .bind(session.id)
        .bind(session.agent_id)
        .bind(session.task_id)
        .bind(prompt)
        .bind(interrupt)
        .bind(crate::run_request::RunRequest::asked(from, Some(text)).value())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        if plan_skipped {
            self.announce_plan_skipped(session.agent_id, session.id, run.id, session.task_id).await?;
        } else if replan {
            self.settle_plan_cards(session.agent_id, session.id, crate::quests::PLAN_REVISING_NOTE).await?;
            self.clear_plan_waiting(session.agent_id).await?;
        }
        Ok(run)
    }

    pub async fn resume_after_update(&self, run_id: Uuid) -> Result<bool> {
        let run = sqlx::query_as::<_, (Uuid, String, String, Option<Uuid>)>(
            "SELECT session_id, kind, prompt, fork_from FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((session_id, kind, prompt, fork_from)) = run else {
            return Ok(false);
        };
        if !matches!(kind.as_str(), "direct" | "aside" | "quick") {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, cli_session_id FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((status, cli_session_id)) = session else {
            return Ok(false);
        };
        if status == "archived" {
            return Ok(false);
        }
        let resume = matches!(kind.as_str(), "direct" | "quick") && cli_session_id.is_some();
        let prompt = if resume { crate::run_causes::RESUME_DIRECT_AFTER_UPDATE.to_string() } else { prompt };
        clone_run(
            &mut tx,
            run_id,
            RunClone {
                kind: None,
                fork_from: if resume { None } else { fork_from },
                prompt: &prompt,
                activity: Some("Picking up after the update"),
                retry_cause: Some("update"),
                not_before: None,
                retry: false,
            },
        )
        .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn queue_direct(&self, session: &AgentSession, text: &str, interrupt: bool) -> Result<SessionRun> {
        let mut tx = self.pool.begin().await?;
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        if status == "archived" {
            anyhow::bail!("archived sessions cannot receive messages");
        }
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, kind, status, prompt, timeout_secs, interrupt)
             VALUES ($1, $2, 'direct', 'queued', $3, $4, $5)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at",
        )
        .bind(session.id)
        .bind(session.agent_id)
        .bind(text)
        .bind(default_task_timeout_secs())
        .bind(interrupt)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn nudge_missing(&self, run_id: Uuid, session_id: Uuid, required: Required) -> Result<Nudge> {
        let (role, kinds, kind, reminders, mark, activity) = match required {
            Required::Report => ("task", &["task", "followup"][..], "followup", &[REPORT_NUDGE, REPORT_NUDGE_LAST][..], REPORT_NUDGE_MARK, "Writing up the report"),
            Required::Verdict => ("review", &["review"][..], "review", &[VERDICT_NUDGE][..], VERDICT_NUDGE_MARK, "Waiting for the review verdict"),
        };
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<String>)>(
            "SELECT agent_id, task_id, cli_session_id FROM agent_sessions WHERE id = $1 AND role = $2 FOR UPDATE",
        )
        .bind(session_id)
        .bind(role)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((agent_id, task_id, Some(_))) = session else {
            return Ok(Nudge::Exhausted);
        };
        let (open, sent) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*) FILTER (WHERE status IN ('queued', 'running') AND id <> $4),
                    COUNT(*) FILTER (WHERE COALESCE(starts_with(prompt, $3), false)
                                       AND created_at >= COALESCE((SELECT MAX(created_at) FROM session_runs
                                                                   WHERE session_id = $1 AND kind = ANY($2)
                                                                     AND NOT COALESCE(starts_with(prompt, $3), false)),
                                                                 '-infinity'))
             FROM session_runs WHERE session_id = $1 AND kind = ANY($2)",
        )
        .bind(session_id)
        .bind(kinds)
        .bind(mark)
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
        if open > 0 {
            return Ok(Nudge::Open);
        }
        let Some(prompt) = usize::try_from(sent).ok().and_then(|sent| reminders.get(sent)) else {
            return Ok(Nudge::Exhausted);
        };
        let nudge = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity)
             VALUES ($1, $2, $3, $4, 'queued', $5, 3600, $6)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(kind)
        .bind(prompt)
        .bind(activity)
        .fetch_one(&mut *tx)
        .await?;
        if required == Required::Verdict {
            sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Nudge::Queued(nudge))
    }

    pub async fn fail_missing(&self, run_id: Uuid, required: Required, output: &str) -> Result<crate::run_causes::Diagnosis> {
        let kind = sqlx::query_scalar::<_, String>("SELECT kind FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&self.pool)
            .await?;
        let mut diagnosis = self.record_run_end(run_id, &kind, None, output, false).await?;
        diagnosis.cause = required.cause();
        diagnosis.reason = required.missed().to_string();
        let mut tx = self.pool.begin().await?;
        let flipped = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
            "UPDATE session_runs SET status = 'failed', error = $2, cause = $3, diagnosis = $4
             WHERE id = $1 AND status = 'completed'
             RETURNING agent_id, session_id, task_id",
        )
        .bind(run_id)
        .bind(required.missed())
        .bind(diagnosis.cause.as_str())
        .bind(serde_json::to_value(&diagnosis)?)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((agent_id, session_id, task_id)) = flipped {
            if !run_end_announced(&mut tx, run_id).await? {
                let mut payload = crate::cli_errors::failure_payload(task_id, required.missed(), session_id, &kind);
                diagnosis.merge_into(&mut payload);
                payload["quiet"] = json!(true);
                let event = NewEvent::new("run.failed").agent(agent_id).session(session_id).run(run_id).task(task_id).payload(payload);
                emit_in(&mut tx, &event, None).await?;
            }
        }
        tx.commit().await?;
        Ok(diagnosis)
    }

    pub async fn settle_missing(&self, run_id: Uuid, session_id: Uuid, required: Required, output: &str) -> Result<Nudge> {
        let nudge = self.nudge_missing(run_id, session_id, required).await?;
        if nudge.handled() {
            self.announce_completed(run_id).await?;
        } else {
            self.fail_missing(run_id, required, output).await?;
        }
        Ok(nudge)
    }

    pub async fn announce_completed(&self, run_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let run = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, f64)>(
            "SELECT agent_id, session_id, task_id, cost_usd FROM session_runs WHERE id = $1 AND status = 'completed' FOR UPDATE",
        )
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((agent_id, session_id, task_id, cost_usd)) = run else {
            return Ok(false);
        };
        if run_end_announced(&mut tx, run_id).await? {
            return Ok(false);
        }
        let event = NewEvent::new("run.completed")
            .agent(agent_id)
            .session(session_id)
            .run(run_id)
            .task(task_id)
            .payload(json!({ "cost_usd": cost_usd }));
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn record_run_end(
        &self,
        run_id: Uuid,
        kind: &str,
        error: Option<&str>,
        output: &str,
        timed_out: bool,
    ) -> Result<crate::run_causes::Diagnosis> {
        let (task_id, duration_secs, turns) = sqlx::query_as::<_, (Option<Uuid>, i64, i64)>(
            "SELECT task_id,
                    COALESCE(EXTRACT(EPOCH FROM COALESCE(finished_at, NOW()) - COALESCE(started_at, created_at)), 0)::bigint,
                    (SELECT COUNT(*) FROM session_run_events e
                      WHERE e.run_id = r.id AND e.kind IN ('text', 'tool') AND e.body NOT LIKE $2)
             FROM session_runs r WHERE id = $1",
        )
        .bind(run_id)
        .bind(SESSION_STARTED_LIKE)
        .fetch_one(&self.pool)
        .await?;
        let mut last = sqlx::query_scalar::<_, String>(
            "SELECT body FROM session_run_events WHERE run_id = $1 AND kind IN ('text', 'error', 'result') AND body NOT LIKE $2
             ORDER BY id DESC LIMIT 3",
        )
        .bind(run_id)
        .bind(SESSION_STARTED_LIKE)
        .fetch_all(&self.pool)
        .await?;
        last.reverse();
        let mut diagnosis = crate::run_causes::diagnose(&crate::run_causes::RunEnd { kind, error, output, turns, timed_out });
        diagnosis.duration_secs = duration_secs;
        let mut parts: Vec<&str> = Vec::new();
        match error.map(|e| e.split(crate::cli_errors::FRIENDLY_MARK).last().unwrap_or(e)) {
            Some(error) => {
                parts.extend(last.iter().map(String::as_str));
                parts.push(error);
            }
            None if !output.trim().is_empty() => parts.push(output),
            None => parts.extend(last.iter().map(String::as_str)),
        }
        diagnosis.tail = crate::run_causes::tail_text(&parts);
        if let Some(task_id) = task_id {
            diagnosis.auto_retried = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM session_runs WHERE task_id = $1 AND retry_cause = $2)",
            )
            .bind(task_id)
            .bind(diagnosis.cause.as_str())
            .fetch_one(&self.pool)
            .await?;
        }
        sqlx::query("UPDATE session_runs SET cause = $2, diagnosis = $3 WHERE id = $1")
            .bind(run_id)
            .bind(diagnosis.cause.as_str())
            .bind(serde_json::to_value(&diagnosis)?)
            .execute(&self.pool)
            .await?;
        Ok(diagnosis)
    }

    pub async fn auto_retry(&self, run_id: Uuid, diagnosis: &crate::run_causes::Diagnosis) -> Result<Option<Uuid>> {
        if diagnosis.fork_too_long() {
            return self.fresh_retry(run_id, diagnosis).await;
        }
        if !diagnosis.transient() || diagnosis.auto_retried {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await?;
        let run = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, String, String, Option<Uuid>)>(
            "SELECT session_id, agent_id, task_id, kind, prompt, fork_from FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((session_id, agent_id, Some(task_id), kind, prompt, fork_from)) = run else {
            return Ok(None);
        };
        if !matches!(kind.as_str(), "task" | "followup" | "review") {
            return Ok(None);
        }
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1 FOR UPDATE")
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?;
        if !status.is_some_and(|status| !matches!(status.as_str(), "cancelled" | "done")) {
            return Ok(None);
        }
        let update = diagnosis.for_update();
        let retried = !update
            && sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM session_runs WHERE task_id = $1 AND retry_cause = $2)",
            )
            .bind(task_id)
            .bind(diagnosis.cause.as_str())
            .fetch_one(&mut *tx)
            .await?;
        if retried {
            return Ok(None);
        }
        let session = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, cli_session_id FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((session_status, cli_session_id)) = session else {
            return Ok(None);
        };
        if session_status == "archived" || kind == "followup" && cli_session_id.is_none() {
            return Ok(None);
        }
        let resume = diagnosis.cause == crate::run_causes::Cause::CliError
            && kind != "review"
            && cli_session_id.is_some();
        let (kind, prompt, fork_from) = if resume {
            let prompt = if update { crate::run_causes::RESUME_AFTER_UPDATE } else { crate::run_causes::RESUME_AFTER_ERROR };
            ("followup".to_string(), prompt.to_string(), None)
        } else {
            (kind, prompt, fork_from)
        };
        let id = clone_run(
            &mut tx,
            run_id,
            RunClone {
                kind: Some(&kind),
                fork_from,
                prompt: &prompt,
                activity: Some(if update { "Picking up after the update" } else { "Trying again after a hiccup" }),
                retry_cause: Some(if update { "update" } else { diagnosis.cause.as_str() }),
                not_before: Some(diagnosis.retry_at(Utc::now())),
                retry: !update,
            },
        )
        .await?
        .id;
        sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let retry_at = diagnosis.retry_at(Utc::now());
        let mut payload = json!({
            "task_id": task_id,
            "session_id": session_id,
            "failed_run_id": run_id,
            "retry_run_id": id,
            "retry_at": retry_at,
            "quiet": true,
        });
        diagnosis.merge_into(&mut payload);
        self.emit(
            NewEvent::new("run.retrying").agent(agent_id).session(session_id).run(id).task(Some(task_id)).payload(payload),
            None,
        )
        .await?;
        Ok(Some(id))
    }

    pub async fn disk_waiting(&self, by: DiskWaitKey, ids: &[Uuid]) -> Result<std::collections::HashSet<Uuid>> {
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let column = match by {
            DiskWaitKey::Run => "id",
            DiskWaitKey::Session => "session_id",
            DiskWaitKey::Quest => "task_id",
        };
        let waiting = sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT DISTINCT {column} FROM session_runs WHERE {column} = ANY($1) AND status = 'queued' AND waiting_for = 'disk'"
        ))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(waiting.into_iter().collect())
    }

    pub async fn resume_disk_stranded(&self) -> Result<Vec<Uuid>> {
        let mut tx = self.pool.begin().await?;
        let runs = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<Uuid>, String)>(
            "UPDATE session_runs r SET status = 'queued', waiting_for = 'disk', error = NULL, cause = NULL, diagnosis = NULL,
               finished_at = NULL, claimed_by = NULL, started_at = NULL, heartbeat_at = NULL, lease_expires_at = NULL
             FROM agent_tasks t, agent_profiles a
             WHERE r.status = 'failed' AND r.kind = ANY($1) AND r.runner_id IS NOT NULL
               AND r.diagnosis->>'failure' = 'disk_full' AND r.diagnosis->>'stage' = 'start'
               AND COALESCE((r.diagnosis->>'turns')::bigint, 0) = 0
               AND NOT EXISTS (SELECT 1 FROM session_run_events e WHERE e.run_id = r.id AND e.kind IN ('text', 'tool') AND e.body NOT LIKE $2)
               AND r.finished_at > NOW() - INTERVAL '3 days'
               AND t.id = r.task_id AND t.status = 'failed'
               AND a.id = r.agent_id AND a.paused_reason IS NULL
               AND NOT EXISTS (SELECT 1 FROM session_runs n WHERE n.session_id = r.session_id AND (n.created_at, n.id) > (r.created_at, r.id))
               AND NOT EXISTS (SELECT 1 FROM session_runs live WHERE live.task_id = r.task_id AND live.status IN ('queued', 'running'))
             RETURNING r.id, r.session_id, r.agent_id, r.task_id, r.kind",
        )
        .bind(crate::runners::WORK_RUN_KINDS.map(String::from).to_vec())
        .bind(SESSION_STARTED_LIKE)
        .fetch_all(&mut *tx)
        .await?;
        for (run_id, session_id, agent_id, task_id, kind) in &runs {
            sqlx::query(
                "UPDATE agent_sessions SET status = 'active', updated_at = NOW()
                 WHERE status = 'failed' AND role IN ('task', 'review') AND attempt_state IS NULL
                   AND (id = $1 OR id = (SELECT parent_session_id FROM agent_sessions WHERE id = $1))",
            )
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
            reopen_quest(&mut tx, *task_id).await?;
            emit_in(
                &mut tx,
                &NewEvent::new("run.waiting").agent(*agent_id).session(*session_id).run(*run_id).task(*task_id).payload(json!({
                    "waiting": true,
                    "reason": "disk",
                    "run_kind": kind,
                    "task_id": task_id,
                    "quiet": true,
                })),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(runs.into_iter().map(|run| run.0).collect())
    }

    pub async fn run_diagnosis(&self, run_id: Uuid) -> Result<Option<crate::run_causes::Diagnosis>> {
        let value = sqlx::query_scalar::<_, Option<Value>>("SELECT diagnosis FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok(value.and_then(|value| serde_json::from_value(value).ok()))
    }

    pub async fn latest_diagnosis(&self, session_id: Uuid) -> Result<Option<crate::run_causes::Diagnosis>> {
        let value = sqlx::query_scalar::<_, Option<Value>>(
            "SELECT r.diagnosis FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
             WHERE (r.session_id = $1 OR s.parent_session_id = $1)
               AND r.kind IN ('task', 'followup', 'review', 'publish')
               AND r.status NOT IN ('queued', 'running')
             ORDER BY r.created_at DESC, r.id DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        Ok(value.and_then(|value| serde_json::from_value(value).ok()))
    }

    pub async fn resume_task_session(&self, agent_id: Uuid, task_id: Uuid, from: &str, brief: Option<&str>) -> Result<TaskResume> {
        let prompt = resume_prompt(from, brief);
        let mut tx = self.pool.begin().await?;
        let facts = crate::quests::load_resume_facts(&mut tx, agent_id, task_id, true).await?;
        let (session_id, plan_skipped) = match crate::quests::decide_resume(&facts) {
            crate::quests::ResumePlan::Refused(reason) => anyhow::bail!(reason),
            crate::quests::ResumePlan::Busy(active) => {
                return Ok(TaskResume::Busy { session_id: active.session_id, kind: active.kind, status: active.status })
            }
            crate::quests::ResumePlan::AwaitingPlan(session_id) => return Ok(TaskResume::AwaitingPlan(session_id)),
            crate::quests::ResumePlan::AwaitingClarification(session_id) => return Ok(TaskResume::AwaitingClarification(session_id)),
            crate::quests::ResumePlan::Fresh => return Ok(TaskResume::Fresh),
            crate::quests::ResumePlan::Resume { session_id, plan_skipped } => (session_id, plan_skipped),
        };
        if plan_skipped {
            sqlx::query("UPDATE agent_sessions SET plan_state = 'approved' WHERE id = $1")
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
        }
        let prompt = if plan_skipped { format!("{}\n\n{prompt}", crate::quests::PLAN_SKIPPED) } else { prompt };
        sqlx::query("UPDATE agent_sessions SET status = 'active', review_rounds = 0, updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        reopen_quest(&mut tx, Some(task_id)).await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity, request)
             VALUES ($1, $2, $3, 'followup', 'queued', $4, 3600, 'Picking the quest back up', $5)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(prompt)
        .bind(crate::run_request::RunRequest::asked(from, brief).value())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        if plan_skipped {
            self.announce_plan_skipped(agent_id, session_id, run_id, Some(task_id)).await?;
        }
        Ok(TaskResume::Resumed { session_id, run_id })
    }

    pub async fn tidy_failed_turns(&self, agent_id: Option<Uuid>, older_than_hours: i64) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM session_runs r
             USING agent_sessions s
             WHERE s.id = r.session_id AND s.role = 'main' AND r.kind = 'turn'
               AND r.status IN ('failed', 'timed_out')
               AND ($1::uuid IS NULL OR r.agent_id = $1)
               AND r.created_at < NOW() - make_interval(hours => $2::int)
               AND COALESCE(r.input_tokens, 0) + COALESCE(r.cached_input_tokens, 0) + COALESCE(r.output_tokens, 0) = 0
               AND COALESCE(r.cost_usd, 0) = 0
               AND NULLIF(BTRIM(COALESCE(r.output, '')), '') IS NULL
               AND EXISTS (
                 SELECT 1 FROM session_runs newer
                 WHERE newer.session_id = r.session_id AND newer.created_at > r.created_at
               )",
        )
        .bind(agent_id)
        .bind(older_than_hours as i32)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn pending_steers(&self, session_id: Uuid) -> Result<Vec<PendingSteer>> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, bool)>(
            "SELECT id, kind, prompt, interrupt FROM session_runs
             WHERE session_id = $1 AND kind IN ('followup', 'direct') AND status = 'queued'
             ORDER BY created_at, id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, prompt, interrupt)| PendingSteer {
                id,
                text: if kind == "direct" { direct_steer_text(&prompt) } else { steer_text(&prompt) },
                interrupt,
            })
            .collect())
    }

    pub async fn should_compact(&self, session_id: Uuid, run_id: Uuid, used: i64, limit: i64) -> Result<bool> {
        self.compaction_due(session_id, run_id, used, limit, "compact_floor").await
    }

    pub async fn should_auto_compact(&self, session_id: Uuid, run_id: Uuid, used: i64, limit: i64) -> Result<bool> {
        self.compaction_due(session_id, run_id, used, limit, "auto_compact_floor").await
    }

    pub async fn clear_auto_compact_floor(&self, agent_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET auto_compact_floor = NULL WHERE agent_id = $1 AND auto_compact_floor IS NOT NULL")
            .bind(agent_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn compaction_due(&self, session_id: Uuid, run_id: Uuid, used: i64, limit: i64, floor_column: &'static str) -> Result<bool> {
        let after_compaction: bool = sqlx::query_scalar(
            "SELECT COALESCE((SELECT prompt = $3 FROM session_runs
               WHERE session_id = $1 AND id <> $2 AND status = 'completed' AND kind = 'turn'
               ORDER BY created_at DESC, id DESC LIMIT 1), false)",
        )
        .bind(session_id)
        .bind(run_id)
        .bind(COMPACT_PROMPT)
        .fetch_one(&self.pool)
        .await?;
        if after_compaction {
            let floor = (used > limit).then_some(used.min(i32::MAX as i64) as i32);
            sqlx::query(&format!("UPDATE agent_sessions SET {floor_column} = $2 WHERE id = $1"))
                .bind(session_id)
                .bind(floor)
                .execute(&self.pool)
                .await?;
            if floor.is_some() {
                tracing::info!(session = %session_id, tokens = used, limit, "Compaction could not get under the limit, compacting again only after the context grows");
                return Ok(false);
            }
        }
        let floor: Option<i32> = sqlx::query_scalar(&format!("SELECT {floor_column} FROM agent_sessions WHERE id = $1"))
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok(used > compact_limit(limit, floor.map(i64::from)))
    }

    pub async fn mark_steered(&self, followup_id: Uuid, into_run: Uuid) -> Result<bool> {
        Ok(!self.mark_steered_many(&[followup_id], into_run).await?.is_empty())
    }

    pub async fn mark_steered_many(&self, followup_ids: &[Uuid], into_run: Uuid) -> Result<Vec<Uuid>> {
        if followup_ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            "UPDATE session_runs f
             SET status = 'completed', steered_into = r.id, started_at = NOW(), finished_at = NOW(),
                 output = 'Delivered into the running work.'
             FROM session_runs r
             WHERE f.id = ANY($1) AND f.kind IN ('followup', 'direct') AND f.status = 'queued'
               AND r.id = $2 AND r.session_id = f.session_id AND r.status = 'running'
             RETURNING f.id",
        )
        .bind(followup_ids)
        .bind(into_run)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn request_stop(&self, run_id: Uuid, by: &str) -> Result<Option<bool>> {
        let Some(on_runner) = sqlx::query_scalar::<_, bool>(
            "UPDATE session_runs SET stop_requested_by = $2
             WHERE id = $1 AND status = 'running'
             RETURNING lease_id IS NOT NULL",
        )
        .bind(run_id)
        .bind(by)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        sqlx::query("SELECT pg_notify('session_runs', $1)")
            .bind(run_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(Some(on_runner))
    }

    pub async fn stop_requested(&self, run_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>("SELECT stop_requested_by FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten())
    }

    pub async fn stop_runner_run(&self, run_id: Uuid, error: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let stopped = sqlx::query(
            "UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW()
             WHERE id = $1 AND status = 'running' AND lease_id IS NOT NULL",
        )
        .bind(run_id)
        .bind(error)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if stopped {
            sqlx::query(
                "UPDATE runner_leases SET status = 'cancelled', finished_at = NOW()
                 WHERE run_id = $1 AND status = 'active'",
            )
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(stopped)
    }

    pub async fn settle_merged_quests(&self) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "WITH moved AS (
               UPDATE agent_tasks t SET status = 'done'
               WHERE t.pr_url IS NOT NULL AND t.status IN ('review', 'in_progress')
                 AND EXISTS (SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'merge' AND m.status = 'done')
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs o
                   WHERE o.kind IN ('task', 'followup', 'review', 'publish') AND o.status IN ('queued', 'running')
                     AND (o.task_id = t.id OR o.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
                 )
               RETURNING t.id
             ), closed AS (
               UPDATE task_milestones SET status = 'done', done_at = NOW(), next_check_at = NULL, updated_at = NOW()
               WHERE task_id IN (SELECT id FROM moved) AND kind = 'work' AND status IN ('pending', 'active')
             )
             SELECT id FROM moved",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn settle_cancelled_reviews(&self) -> Result<Vec<Uuid>> {
        let stuck = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid, Uuid, Option<String>)>(
            "SELECT DISTINCT ON (t.id) t.id, r.agent_id, r.id, s.id, p.id, r.error
             FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id AND s.role = 'review'
             JOIN agent_sessions p ON p.id = s.parent_session_id AND p.status <> 'archived'
             JOIN agent_tasks t ON t.id = p.task_id AND t.status = 'in_progress'
             WHERE r.kind = 'review' AND r.status = 'cancelled'
               AND s.asked_by->>'manual' IS DISTINCT FROM 'true'
               AND s.asked_by->'compare' IS NULL
               AND NOT EXISTS (SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'merge' AND m.status = 'done')
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs o
                 WHERE o.kind IN ('task', 'followup', 'review', 'publish')
                   AND (o.task_id = t.id OR o.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
                   AND (o.status IN ('queued', 'running') OR o.created_at > r.created_at)
               )
             ORDER BY t.id, r.created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut settled = Vec::with_capacity(stuck.len());
        for (task_id, agent_id, run_id, session_id, task_session_id, error) in stuck {
            let mut tx = self.pool.begin().await?;
            let moved = sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1 AND status = 'in_progress'")
                .bind(task_id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0;
            if !moved {
                continue;
            }
            let summary = error.unwrap_or_else(|| "The review run was cancelled".to_string());
            emit_in(
                &mut tx,
                &NewEvent::new("review.finished")
                    .agent(agent_id)
                    .session(session_id)
                    .run(run_id)
                    .task(Some(task_id))
                    .payload(serde_json::json!({
                        "task_id": task_id,
                        "task_session_id": task_session_id,
                        "cancelled": true,
                        "review": { "verdict": "review cancelled", "summary": summary },
                    })),
                Some((agent_id, "review.finished")),
            )
            .await?;
            tx.commit().await?;
            settled.push(task_id);
        }
        Ok(settled)
    }

    pub async fn quest_merged_pr(&self, task_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT t.pr_url FROM agent_tasks t
             WHERE t.id = $1 AND t.pr_url IS NOT NULL AND t.status <> 'cancelled'
               AND (t.status = 'done' OR EXISTS (
                 SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'merge' AND m.status = 'done'))",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn settle_cancelled_work(&self) -> Result<Vec<Uuid>> {
        let stuck = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid, String, bool, Option<String>)>(
            "SELECT t.id, r.agent_id, r.id, r.session_id, r.kind, r.started_at IS NOT NULL, r.error
             FROM (
               SELECT DISTINCT ON (t.id) t.id, t.assigned_agent_id, r.id AS run_id
               FROM agent_tasks t
               JOIN session_runs r ON r.kind IN ('task', 'followup', 'review', 'publish')
                 AND (r.task_id = t.id OR r.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
               WHERE t.status = 'in_progress'
               ORDER BY t.id, r.created_at DESC
             ) t
             JOIN session_runs r ON r.id = t.run_id AND r.status = 'cancelled' AND r.agent_id = t.assigned_agent_id
             JOIN agent_sessions s ON s.id = r.session_id AND s.status <> 'archived'
             WHERE NOT (r.kind = 'review' AND s.asked_by->'compare' IS NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs o
                 WHERE o.kind IN ('task', 'followup', 'review', 'publish') AND o.status IN ('queued', 'running')
                   AND (o.task_id = t.id OR o.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
               )",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut settled = Vec::with_capacity(stuck.len());
        for (task_id, agent_id, run_id, session_id, kind, started, why) in stuck {
            let mut tx = self.pool.begin().await?;
            let locked = sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_tasks WHERE id = $1 AND status = 'in_progress' FOR UPDATE")
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?;
            if locked.is_none() {
                continue;
            }
            let superseded = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM session_runs o, session_runs r
                   WHERE r.id = $2 AND o.kind IN ('task', 'followup', 'review', 'publish')
                     AND (o.task_id = $1 OR o.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))
                     AND (o.status IN ('queued', 'running') OR o.created_at > r.created_at)
                 )",
            )
            .bind(task_id)
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
            if superseded {
                continue;
            }
            let reason = match why.as_deref().map(str::trim).filter(|why| !why.is_empty()) {
                Some(why) => format!("It was cancelled: {why}"),
                None => "It was cancelled before it finished".to_string(),
            };
            let error = format!("The {kind} run was cancelled, so the quest stopped. {reason}");
            sqlx::query("UPDATE agent_tasks SET status = 'failed', error_message = $2 WHERE id = $1")
                .bind(task_id)
                .bind(&error)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE session_runs SET cause = COALESCE(cause, 'cancelled') WHERE id = $1")
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            let mut payload = crate::cli_errors::failure_payload(Some(task_id), &error, session_id, &kind);
            if let Some(fields) = payload.as_object_mut() {
                fields.insert("cause".into(), json!("cancelled"));
                fields.insert("stage".into(), json!(if started { "run" } else { "queue" }));
                fields.insert("reason".into(), json!(reason));
            }
            emit_in(
                &mut tx,
                &NewEvent::new("run.failed").agent(agent_id).session(session_id).run(run_id).task(Some(task_id)).payload(payload),
                Some((agent_id, "run.failed")),
            )
            .await?;
            tx.commit().await?;
            settled.push(task_id);
        }
        Ok(settled)
    }

    pub async fn find_tasks_by_prefix(&self, prefix: &str) -> Result<Vec<TaskRef>> {
        self.find_tasks_by_prefix_in(None, prefix).await
    }

    pub async fn find_tasks_by_prefix_in(&self, org: Option<Uuid>, prefix: &str) -> Result<Vec<TaskRef>> {
        Ok(sqlx::query_as::<_, TaskRef>(
            "SELECT id, title, status, assigned_agent_id, user_id FROM agent_tasks
             WHERE id::text LIKE $1 || '%' AND ($2::uuid IS NULL OR org_id = $2) ORDER BY updated_at DESC NULLS LAST LIMIT 5",
        )
        .bind(prefix.trim().to_lowercase())
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn cancel_task(&self, task_id: Uuid, by: &str, reason: Option<&str>) -> Result<CancelledTask> {
        let mut tx = self.pool.begin().await?;
        let task = lock_task(&mut tx, task_id).await?;
        let was_open = !matches!(task.status.as_str(), "done" | "cancelled");
        if was_open {
            sqlx::query("UPDATE agent_tasks SET status = 'cancelled' WHERE id = $1")
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
        }
        let cleanup = clean_up_task(&mut tx, task_id, &closing_note("cancelled", by, reason)).await?;
        let mut event = NewEvent::new("task.cancelled");
        event.agent_id = task.assigned_agent_id;
        event.task_id = Some(task_id);
        event.payload = json!({
            "title": task.title,
            "by": by,
            "reason": reason,
            "was_open": was_open,
            "dropped_runs": cleanup.dropped,
            "stopping_runs": cleanup.running.len(),
        });
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&cleanup.clarification_agents).await;
        Ok(CancelledTask { task, was_open, dropped: cleanup.dropped, running: cleanup.running, archived: cleanup.archived })
    }

    pub async fn delete_task(&self, task_id: Uuid, by: &str, reason: Option<&str>) -> Result<DeletedTask> {
        let mut tx = self.pool.begin().await?;
        let task = lock_task(&mut tx, task_id).await?;
        let subtasks = sqlx::query_as::<_, TaskRef>(
            "SELECT id, title, status, assigned_agent_id, user_id FROM agent_tasks WHERE parent_task_id = $1 FOR UPDATE",
        )
        .bind(task_id)
        .fetch_all(&mut *tx)
        .await?;
        let open: Vec<&TaskRef> = subtasks.iter().filter(|t| !matches!(t.status.as_str(), "done" | "cancelled")).collect();
        if !open.is_empty() {
            anyhow::bail!(
                "it has {} open subtasks ({}); cancel or delete those first",
                open.len(),
                open.iter().map(|t| t.id.to_string()[..8].to_string()).collect::<Vec<_>>().join(", ")
            );
        }
        let note = closing_note("deleted", by, reason);
        let mut cleanup = clean_up_task(&mut tx, task_id, &note).await?;
        for sub in &subtasks {
            let more = clean_up_task(&mut tx, sub.id, &note).await?;
            cleanup.dropped += more.dropped;
            cleanup.archived += more.archived;
            cleanup.running.extend(more.running);
            cleanup.clarification_agents.extend(more.clarification_agents);
        }
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task_id).execute(&mut *tx).await?;
        let mut event = NewEvent::new("task.deleted");
        event.agent_id = task.assigned_agent_id;
        event.payload = json!({
            "task_id": task_id,
            "title": task.title,
            "status": task.status,
            "by": by,
            "reason": reason,
            "subtasks": subtasks.len(),
            "dropped_runs": cleanup.dropped,
            "stopping_runs": cleanup.running.len(),
        });
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&cleanup.clarification_agents).await;
        Ok(DeletedTask { task, subtasks: subtasks.len(), dropped: cleanup.dropped, running: cleanup.running, archived: cleanup.archived })
    }

    pub async fn quests(&self, viewer: Uuid, org: Uuid, limit: i64) -> Result<Vec<TaskBrief>> {
        Ok(sqlx::query_as::<_, TaskBrief>(&format!(
            "SELECT id, title, status, repo, pr_url, assigned_agent_id, assignee_user_id, result_content, updated_at, user_id,
                    plan_first, attempts, pace, {} AS plan_waiting
             FROM agent_tasks
             WHERE org_id = $3 AND (assigned_agent_id IS NOT NULL
                OR (COALESCE(source, '') <> 'chat' AND (user_id = $1 OR assignee_user_id = $1)))
             ORDER BY updated_at DESC NULLS LAST LIMIT $2",
            plan_waiting_sql("agent_tasks")
        ))
        .bind(viewer)
        .bind(limit)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn plan_waiting(&self, task_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(&format!("SELECT {} FROM agent_tasks t WHERE t.id = $1", plan_waiting_sql("t")))
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or(false))
    }

    pub const MIN_TIME_LIMIT_MINUTES: i64 = 5;
    pub const MAX_TIME_LIMIT_MINUTES: i64 = 24 * 60;

    pub async fn set_task_time_limit(&self, task_id: Uuid, minutes: Option<i64>) -> Result<Option<i32>> {
        let secs = match minutes {
            Some(m) if !(Self::MIN_TIME_LIMIT_MINUTES..=Self::MAX_TIME_LIMIT_MINUTES).contains(&m) => anyhow::bail!(
                "a time limit must be between {} minutes and {} hours",
                Self::MIN_TIME_LIMIT_MINUTES,
                Self::MAX_TIME_LIMIT_MINUTES / 60
            ),
            Some(m) => Some((m * 60) as i32),
            None => None,
        };
        let mut tx = self.pool.begin().await?;
        let found = sqlx::query("UPDATE agent_tasks SET time_limit_secs = $2, updated_at = NOW() WHERE id = $1")
            .bind(task_id)
            .bind(secs)
            .execute(&mut *tx)
            .await?;
        if found.rows_affected() == 0 {
            anyhow::bail!("no such quest");
        }
        sqlx::query(
            "UPDATE session_runs r SET timeout_secs = COALESCE($2, $3)
             WHERE r.status IN ('queued', 'running') AND r.kind IN ('task', 'followup', 'review')
               AND (r.task_id = $1 OR r.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))",
        )
        .bind(task_id)
        .bind(secs)
        .bind(default_task_timeout_secs())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(secs)
    }

        pub async fn edit_quest(&self, task_id: Uuid, edit: &QuestEdit, actor: Uuid, by: &str) -> Result<QuestEdited> {
        let mut tx = self.pool.begin().await?;
        let (title, from_status, from_agent, from_user): (String, String, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT title, status, assigned_agent_id, assignee_user_id FROM agent_tasks WHERE id = $1 FOR UPDATE",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        let closed = |s: &str| matches!(s, "done" | "cancelled" | "failed");
        let mut status = edit.status.clone().unwrap_or_else(|| from_status.clone());
        let (agent, person) = match edit.assignee {
            Some(QuestAssignee::Agent(id)) => (Some(id), None),
            Some(QuestAssignee::Person(id)) => (None, Some(id)),
            Some(QuestAssignee::Nobody) => (None, None),
            None => (from_agent, from_user),
        };
        let mut running = Vec::new();
        let mut clarification_agents = Vec::new();
        if let Some(old) = from_agent.filter(|old| agent != Some(*old)) {
            let (released, settled) = release_quest(&mut tx, task_id, old, &format!("Quest handed off by {by}")).await?;
            running.extend(released);
            clarification_agents.extend(settled);
        }
        if edit.status.is_none() && agent.is_some() && agent != from_agent && closed(&status) {
            status = "todo".into();
        }
        sqlx::query("UPDATE agent_tasks SET status = $2, assigned_agent_id = $3, assignee_user_id = $4 WHERE id = $1")
            .bind(task_id)
            .bind(&status)
            .bind(agent)
            .bind(person)
            .execute(&mut *tx)
            .await?;
        if status != from_status && matches!(status.as_str(), "done" | "cancelled") {
            let verb = if status == "done" { "marked done" } else { "cancelled" };
            let cleanup = clean_up_task(&mut tx, task_id, &closing_note(verb, by, None)).await?;
            running.extend(cleanup.running);
            clarification_agents.extend(cleanup.clarification_agents);
        } else if status != from_status && matches!(status.as_str(), "todo" | "planning" | "plan_review" | "review") {
            clarification_agents.extend(
                settle_task_clarifications(&mut tx, task_id, None, &format!("Quest moved to {status} by {by}")).await?,
            );
        }
        let mut event = NewEvent::new("task.updated");
        event.agent_id = agent.or(from_agent);
        event.task_id = Some(task_id);
        event.actor_user_id = Some(actor);
        event.payload = json!({
            "title": title,
            "by": by,
            "status": status,
            "from_status": from_status,
            "assigned_agent_id": agent,
            "assignee_user_id": person,
            "from_agent_id": from_agent,
            "from_user_id": from_user,
            "stopping_runs": running.len(),
        });
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&clarification_agents).await;
        Ok(QuestEdited { status, running })
    }

    pub async fn complete_task_run(&self, id: Uuid, run_created_at: DateTime<Utc>) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let has_followup = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE session_id = $1 AND kind = 'followup' AND created_at > $2 AND steered_into IS NULL
             )",
        )
        .bind(id)
        .bind(run_created_at)
        .fetch_one(&mut *tx)
        .await?;
        let completed = !has_followup
            && sqlx::query("UPDATE agent_sessions SET status = 'completed', updated_at = NOW() WHERE id = $1 AND status <> 'archived'")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                == 1;
        tx.commit().await?;
        Ok(completed)
    }

    pub async fn complete_followup(&self, id: Uuid) -> Result<FollowupAdvance> {
        let mut tx = self.pool.begin().await?;
        let (role, status) = sqlx::query_as::<_, (String, String)>("SELECT role, status FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let waiting = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE session_id = $1 AND kind = 'followup' AND status IN ('queued', 'running')
             )",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let advance = match (role.as_str(), status.as_str()) {
            ("task", "active" | "completed") if waiting => FollowupAdvance::Wait,
            ("task", "active") => {
                sqlx::query("UPDATE agent_sessions SET status = 'completed', updated_at = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                FollowupAdvance::Advance { was_completed: false }
            }
            ("task", "completed") => FollowupAdvance::Advance { was_completed: true },
            _ => FollowupAdvance::Wait,
        };
        tx.commit().await?;
        Ok(advance)
    }

    pub async fn fail_queued_followups(&self, session_id: Uuid, error: &str) -> Result<()> {
        sqlx::query(
            "UPDATE session_runs
             SET status = 'failed', error = $2, finished_at = NOW()
             WHERE session_id = $1 AND kind = 'followup' AND status = 'queued'",
        )
        .bind(session_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn request_review(
        &self,
        task_session: &AgentSession,
        reviewer: Option<&WorldAgent>,
        choice: Option<&ReviewerChoice>,
        focus: Option<&str>,
        requested_by: Uuid,
    ) -> Result<SessionRun> {
        let mut tx = self.pool.begin().await?;
        let agent_default = sqlx::query_scalar::<_, Option<Value>>("SELECT reviewer FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(task_session.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let current = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(task_session.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        if current.role != "task" {
            anyhow::bail!("only task sessions can be reviewed");
        }
        if current.working_dir.as_deref().is_none_or(|dir| dir.trim().is_empty()) {
            anyhow::bail!("that session has no saved quest folder to review. Resume the quest with start_task, then review its new session");
        }
        let task_id = current
            .task_id
            .ok_or_else(|| anyhow::anyhow!("that session has no task"))?;
        let (backend, model) = match (choice, reviewer) {
            (Some(c), _) => (c.cli.clone().unwrap_or_else(|| current.backend.clone()), c.model.clone()),
            (None, Some(r)) => (r.cli.clone(), r.model.clone()),
            (None, None) => current.review_target(agent_default.as_ref()),
        };
        let review = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, parent_session_id, task_id, working_dir, usage_baseline, asked_by)
             VALUES ($1, 'review', $2, $3, $4, $5, $6, '{}'::jsonb, $7)
             RETURNING *",
        )
        .bind(current.agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(current.id)
        .bind(task_id)
        .bind(current.working_dir.as_deref())
        .bind(serde_json::json!({ "manual": true, "agent_id": requested_by, "reviewer": reviewer.map(|r| r.name.clone()).unwrap_or_else(|| format!("{backend} {model}")) }))
        .fetch_one(&mut *tx)
        .await?;
        let asked = crate::run_request::RunRequest::review(focus).value();
        let focus = focus.map(|f| format!("\nFocus especially on: {f}\n")).unwrap_or_default();
        let prompt = format!(
            "You are an independent reviewer. Another session did the work in this quest folder and you have no stake in it.{focus}\nInspect the changes (git status and git diff if this is a repository, otherwise read the files). Look only for substantive problems: bugs, missing pieces, security, broken builds or tests. Do not edit files.\n\nEnd with:\n<!-- verdict\n{{\"verdict\": \"approve\" | \"changes\", \"summary\": \"...\", \"findings\": [{{\"severity\": \"high|medium|low\", \"path\": \"...\", \"issue\": \"...\"}}]}}\nverdict -->"
        );
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, request)
             VALUES ($1, $2, $3, 'review', 'queued', $4, 3600, $5)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at, request",
        )
        .bind(review.id)
        .bind(current.agent_id)
        .bind(task_id)
        .bind(prompt)
        .bind(asked)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn get_run(&self, id: Uuid) -> Result<Option<SessionRun>> {
        Ok(sqlx::query_as::<_, SessionRun>(
            "SELECT id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                    timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                    created_at, started_at, finished_at, request
             FROM session_runs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn retry_run(&self, id: Uuid, actor_user_id: Uuid) -> std::result::Result<RetriedRun, RetryRunError> {
        let mut tx = self.pool.begin().await?;
        let run = sqlx::query_as::<_, SessionRun>(
            "SELECT id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                    timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                    created_at, started_at, finished_at, request
             FROM session_runs WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RetryRunError::NotFound)?;
        let session = sqlx::query_as::<_, AgentSession>("SELECT * FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(run.session_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| RetryRunError::Conflict("session no longer exists".into()))?;
        if !matches!(run.status.as_str(), "failed" | "timed_out") {
            return Err(RetryRunError::Conflict("only failed or timed out runs can be retried".into()));
        }
        if session.status == "archived" {
            return Err(RetryRunError::Conflict("archived sessions cannot be retried".into()));
        }
        if session.working_dir.as_deref().is_none_or(|path| path.trim().is_empty()) {
            return Err(RetryRunError::Conflict("the saved quest folder is missing".into()));
        }
        let newer = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE session_id = $1 AND (created_at, id) > ($2, $3)
             )",
        )
        .bind(session.id)
        .bind(run.created_at)
        .bind(run.id)
        .fetch_one(&mut *tx)
        .await?;
        if newer {
            return Err(RetryRunError::Conflict("a newer run already exists for this session".into()));
        }
        let task = match run.task_id {
            Some(task_id) => Some(
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT status, pr_url FROM agent_tasks WHERE id = $1 FOR UPDATE",
                )
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| RetryRunError::Conflict("the linked quest no longer exists".into()))?,
            ),
            None => None,
        };
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE status IN ('queued', 'running')
                 AND (session_id = $1 OR ($2::uuid IS NOT NULL AND task_id = $2))
             )",
        )
        .bind(session.id)
        .bind(run.task_id)
        .fetch_one(&mut *tx)
        .await?;
        if active {
            return Err(RetryRunError::Conflict("this work already has an active run".into()));
        }
        if run.task_id.is_some() && matches!(session.role.as_str(), "task" | "review") {
            let obsolete = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM agent_sessions
                   WHERE task_id = $1 AND role = $2 AND (created_at, id) > ($3, $4)
                 )",
            )
            .bind(run.task_id)
            .bind(&session.role)
            .bind(session.created_at)
            .bind(session.id)
            .fetch_one(&mut *tx)
            .await?;
            if obsolete {
                return Err(RetryRunError::Conflict("a newer attempt replaced this session".into()));
            }
        }
        let retry_mode = if session.cli_session_id.is_some() { "resume" } else { "restart" }.to_string();
        if run.kind == "turn" {
            if session.role != "main" || run.prompt.trim() == "/compact" {
                return Err(RetryRunError::Conflict("this main session run cannot be retried".into()));
            }
            let inbox_active = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM agent_inbox
                   WHERE agent_id = $1 AND status IN ('pending', 'processing')
                 )",
            )
            .bind(run.agent_id)
            .fetch_one(&mut *tx)
            .await?;
            if inbox_active {
                return Err(RetryRunError::Conflict("this turn is already scheduled to retry".into()));
            }
            let already_requested = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM world_events
                   WHERE run_id = $1 AND type = 'message.created' AND payload->>'retry' = 'true'
                 )",
            )
            .bind(run.id)
            .fetch_one(&mut *tx)
            .await?;
            if already_requested {
                return Err(RetryRunError::Conflict("this run has already been retried".into()));
            }
            let body = format!(
                "[retry failed turn]\nResume this failed turn from your saved conversation. Inspect current state first and do not repeat side effects that already completed.\n\nPrevious input:\n{}",
                run.prompt
            );
            let message = sqlx::query_as::<_, AgentMessage>(
                "INSERT INTO agent_messages
                   (agent_id, author_kind, author_user_id, body, session_id, run_id)
                 VALUES ($1, 'user', $2, $3, $4, $5) RETURNING *, NULL::text AS session_role",
            )
            .bind(run.agent_id)
            .bind(actor_user_id)
            .bind(&body)
            .bind(session.id)
            .bind(run.id)
            .fetch_one(&mut *tx)
            .await?;
            emit_in(
                &mut tx,
                &NewEvent::new("message.created")
                    .agent(run.agent_id)
                    .session(session.id)
                    .run(run.id)
                    .actor(Some(actor_user_id))
                    .payload(serde_json::json!({
                        "message_id": message.id,
                        "author": "user",
                        "author_kind": "user",
                        "body": body,
                        "retry": true,
                    })),
                Some((run.agent_id, "message")),
            )
            .await?;
            tx.commit().await?;
            return Ok(RetriedRun { run: None, retry_mode });
        }
        if !matches!(run.kind.as_str(), "task" | "review" | "followup" | "aside") {
            return Err(RetryRunError::Conflict("this run type cannot be retried".into()));
        }
        let manual_review = session
            .asked_by
            .as_ref()
            .and_then(|asked_by| asked_by.get("manual"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match run.kind.as_str() {
            "task" => {
                let Some((task_status, pr_url)) = task.as_ref() else {
                    return Err(RetryRunError::Conflict("task runs need a linked quest".into()));
                };
                if session.role != "task" || session.status != "failed" || task_status != "failed" || pr_url.is_some() {
                    return Err(RetryRunError::Conflict("this task attempt is no longer retryable".into()));
                }
                sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
                    .bind(session.id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1")
                    .bind(run.task_id)
                    .execute(&mut *tx)
                    .await?;
            }
            "review" => {
                if session.role != "review" || session.status != "failed" {
                    return Err(RetryRunError::Conflict("this review attempt is no longer retryable".into()));
                }
                sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
                    .bind(session.id)
                    .execute(&mut *tx)
                    .await?;
                if !manual_review {
                    let Some((task_status, _)) = task.as_ref() else {
                        return Err(RetryRunError::Conflict("automatic reviews need a linked quest".into()));
                    };
                    if task_status != "failed" {
                        return Err(RetryRunError::Conflict("this review attempt is no longer retryable".into()));
                    }
                    let parent = session
                        .parent_session_id
                        .ok_or_else(|| RetryRunError::Conflict("the reviewed task session is missing".into()))?;
                    let updated = sqlx::query(
                        "UPDATE agent_sessions SET status = 'completed', updated_at = NOW()
                         WHERE id = $1 AND role = 'task'",
                    )
                    .bind(parent)
                    .execute(&mut *tx)
                    .await?;
                    if updated.rows_affected() != 1 {
                        return Err(RetryRunError::Conflict("the reviewed task session is missing".into()));
                    }
                    sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1")
                        .bind(run.task_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            "followup" => {
                if !matches!(session.role.as_str(), "task" | "review") {
                    return Err(RetryRunError::Conflict("this follow-up is no longer retryable".into()));
                }
                if session.status == "failed" {
                    let Some((task_status, _)) = task.as_ref() else {
                        return Err(RetryRunError::Conflict("this follow-up is no longer retryable".into()));
                    };
                    if session.role != "task" || task_status != "failed" {
                        return Err(RetryRunError::Conflict("this follow-up is no longer retryable".into()));
                    }
                    sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
                        .bind(session.id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1")
                        .bind(run.task_id)
                        .execute(&mut *tx)
                        .await?;
                } else if session.status != "completed" {
                    return Err(RetryRunError::Conflict("this follow-up is no longer retryable".into()));
                }
            }
            "aside" => {
                if session.role != "aside" || session.status != "failed" {
                    return Err(RetryRunError::Conflict("this aside is no longer retryable".into()));
                }
                sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
                    .bind(session.id)
                    .execute(&mut *tx)
                    .await?;
            }
            _ => unreachable!(),
        }
        let prompt = format!(
            "{}\n\n[retry]\nContinue from the saved conversation and quest folder. Inspect existing work first and do not repeat side effects that already completed.",
            run.prompt
        );
        let retried = clone_run(
            &mut tx,
            run.id,
            RunClone {
                kind: None,
                fork_from: if session.cli_session_id.is_some() { None } else { run.fork_from },
                prompt: &prompt,
                activity: None,
                retry_cause: None,
                not_before: None,
                retry: true,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(RetriedRun { run: Some(retried), retry_mode })
    }

    pub async fn notifications(&self, user_id: Uuid, org: Option<Uuid>, limit: i64, cleared: bool) -> Result<Notifications> {
        let org = org.unwrap_or_else(Uuid::nil);
        let items = sqlx::query_as::<_, BellItem>(if cleared {
            "SELECT id, event_seq AS seq, kind, agent_id, created_at, cleared_at FROM world_notifications
              WHERE user_id = $1 AND org_id = $2 AND cleared_at IS NOT NULL
              ORDER BY cleared_at DESC LIMIT $3"
        } else {
            "SELECT id, event_seq AS seq, kind, agent_id, created_at, cleared_at FROM world_notifications
              WHERE user_id = $1 AND org_id = $2 AND cleared_at IS NULL
              ORDER BY id DESC LIMIT $3"
        })
        .bind(user_id)
        .bind(org)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let seqs: Vec<i64> = items.iter().map(|i| i.seq).collect();
        let events = sqlx::query_as::<_, WorldEvent>("SELECT * FROM world_events WHERE seq = ANY($1) ORDER BY seq")
            .bind(&seqs)
            .fetch_all(&self.pool)
            .await?;
        let counts = sqlx::query_as::<_, (Option<Uuid>, i64, i64)>(
            "SELECT agent_id, COUNT(*), COUNT(*) FILTER (WHERE kind IN ('message', 'ask')) FROM world_notifications
              WHERE user_id = $1 AND org_id = $2 AND cleared_at IS NULL GROUP BY agent_id",
        )
        .bind(user_id)
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        let unread = counts.iter().map(|(_, n, _)| *n).sum();
        let by_agent = counts.into_iter().filter(|(_, _, chat)| *chat > 0).filter_map(|(agent, _, chat)| agent.map(|a| (a, chat))).collect();
        let unread_seqs = if cleared { Vec::new() } else { seqs };
        Ok(Notifications { items, events, unread, unread_seqs, by_agent })
    }

    pub async fn clear_notifications(&self, user_id: Uuid, org: Option<Uuid>, which: BellClear) -> Result<u64> {
        let org = org.unwrap_or_else(Uuid::nil);
        let (ids, upto) = match which {
            BellClear::All => (None, None),
            BellClear::Items(ids) => (Some(ids), None),
            BellClear::UpTo(seq) => (None, Some(seq)),
        };
        Ok(sqlx::query(
            "UPDATE world_notifications SET cleared_at = NOW()
              WHERE user_id = $1 AND org_id = $2 AND cleared_at IS NULL
                AND ($3::BIGINT[] IS NULL OR id = ANY($3))
                AND ($4::BIGINT IS NULL OR event_seq <= $4)",
        )
        .bind(user_id)
        .bind(org)
        .bind(ids)
        .bind(upto)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn mark_agent_thread_seen(&self, user_id: Uuid, agent_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE world_notifications SET cleared_at = NOW()
              WHERE user_id = $1 AND agent_id = $2 AND kind = 'message' AND cleared_at IS NULL",
        )
        .bind(user_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_notifications_seen(&self, user_id: Uuid, seq: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO notification_reads (user_id, last_seen_seq) VALUES ($1, $2)
             ON CONFLICT (user_id) DO UPDATE SET last_seen_seq = GREATEST(notification_reads.last_seen_seq, $2), updated_at = NOW()",
        )
        .bind(user_id)
        .bind(seq)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn me(&self, user_id: Uuid) -> Result<Option<MeRow>> {
        Ok(sqlx::query_as::<_, MeRow>("SELECT id, name, email, main_agent_id, status_emoji, status_text, before_big_work FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn before_big_work(&self, user_id: Uuid) -> Result<BeforeBigWork> {
        let value = sqlx::query_scalar::<_, String>("SELECT before_big_work FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(value.as_deref().and_then(BeforeBigWork::parse).unwrap_or_default())
    }

    pub async fn agent_before_big_work(&self, agent: &WorldAgent) -> Result<BeforeBigWork> {
        match agent.owner_user_id {
            Some(owner) => self.before_big_work(owner).await,
            None => Ok(BeforeBigWork::default()),
        }
    }

    pub async fn set_before_big_work(&self, user_id: Uuid, value: BeforeBigWork) -> Result<()> {
        sqlx::query("UPDATE users SET before_big_work = $2 WHERE id = $1")
            .bind(user_id)
            .bind(value.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_user_status(&self, user_id: Uuid, emoji: Option<&str>, text: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE users SET status_emoji = $2, status_text = $3 WHERE id = $1")
            .bind(user_id)
            .bind(emoji)
            .bind(text)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_user_name(&self, user_id: Uuid, name: &str) -> Result<()> {
        sqlx::query("UPDATE users SET name = $2 WHERE id = $1")
            .bind(user_id)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_paused(&self, id: Uuid, reason: Option<&str>) -> Result<bool> {
        let changed = sqlx::query(
            "UPDATE agent_profiles SET paused_reason = $2, updated_at = NOW()
             WHERE id = $1 AND paused_reason IS DISTINCT FROM $2",
        )
        .bind(id)
        .bind(reason)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0;
        if changed && reason.is_none() {
            sqlx::query("SELECT pg_notify($1, $2), pg_notify($3, $2)")
                .bind(CHANNEL_INBOX)
                .bind(id.to_string())
                .bind(CHANNEL_RUNS)
                .execute(&self.pool)
                .await?;
        }
        Ok(changed)
    }

    pub async fn release_budget_pauses(&self, agent_id: Option<Uuid>) -> Result<Vec<Uuid>> {
        let released = sqlx::query_scalar::<_, Uuid>(
            "UPDATE agent_profiles SET paused_reason = NULL, budget_paused_on = NULL, updated_at = NOW()
             WHERE budget_paused_on IS NOT NULL AND ($1::uuid IS NULL OR id = $1)
               AND starts_with(COALESCE(paused_reason, ''), 'Daily budget of')
               AND (budget_paused_on < (NOW() AT TIME ZONE 'UTC')::date
                    OR world_agent_over_budget(id) IS NULL)
             RETURNING id",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        for id in &released {
            sqlx::query("SELECT pg_notify($1, $2), pg_notify($3, $2)")
                .bind(CHANNEL_INBOX)
                .bind(id.to_string())
                .bind(CHANNEL_RUNS)
                .execute(&self.pool)
                .await?;
            self.emit(
                NewEvent::new("agent.resumed").agent(*id).payload(serde_json::json!({ "reason": "daily budget reset" })),
                None,
            )
            .await?;
        }
        Ok(released)
    }

    pub async fn pause_for_rejected_key(&self, agent: &WorldAgent, error: &str) -> Result<bool> {
        if !crate::cli_errors::is_rejected_key(error) {
            return Ok(false);
        }
        let config = if agent.cli == "omp" { self.omp_config(agent.id).await? } else { None };
        let key = crate::providers::model_key_label(&agent.cli, &agent.model, config.as_deref());
        let reason = format!("{KEY_REJECTED_PAUSE}: {key}");
        let paused = sqlx::query(
            "UPDATE agent_profiles SET paused_reason = $2, updated_at = NOW()
             WHERE id = $1 AND paused_reason IS NULL",
        )
        .bind(agent.id)
        .bind(&reason)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0;
        if !paused {
            return Ok(false);
        }
        let text = format!(
            "Its model key was rejected ({key}), so it is paused with its schedules and heartbeat instead of retrying. Update or re-equip the key in Settings > Keys and it resumes on its own, or resume it by hand once the CLI login on its device is fixed."
        );
        self.emit(
            NewEvent::new("agent.paused").agent(agent.id).payload(serde_json::json!({ "name": agent.name, "reason": reason })),
            None,
        )
        .await?;
        self.emit(
            NewEvent::new("attention.requested").agent(agent.id).payload(serde_json::json!({
                "text": text,
                "agent": agent.name,
                "reason": KEY_REJECTED_REASON,
                "key": key,
                "choices": [],
            })),
            None,
        )
        .await?;
        Ok(true)
    }

    pub async fn release_key_pauses(&self, owner: Uuid, agent_id: Option<Uuid>, key: Option<&str>) -> Result<Vec<Uuid>> {
        let released = sqlx::query_scalar::<_, Uuid>(
            "UPDATE agent_profiles SET paused_reason = NULL, updated_at = NOW()
             WHERE owner_user_id = $1 AND ($2::uuid IS NULL OR id = $2)
               AND starts_with(COALESCE(paused_reason, ''), $3)
               AND ($4::text IS NULL OR strpos(paused_reason, $4) > 0)
             RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .bind(KEY_REJECTED_PAUSE)
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        for id in &released {
            sqlx::query("SELECT pg_notify($1, $2), pg_notify($3, $2)")
                .bind(CHANNEL_INBOX)
                .bind(id.to_string())
                .bind(CHANNEL_RUNS)
                .execute(&self.pool)
                .await?;
            self.emit(
                NewEvent::new("agent.resumed").agent(*id).payload(serde_json::json!({ "reason": "model key updated" })),
                None,
            )
            .await?;
        }
        Ok(released)
    }

    pub async fn delete_agent(&self, id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM channel_members WHERE member_kind = 'agent' AND member_id = $1").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_agents(&self) -> Result<Vec<WorldAgent>> {
        let sql = format!("SELECT {AGENT_COLUMNS} FROM agent_profiles ORDER BY created_at");
        Ok(sqlx::query_as::<_, WorldAgent>(&sql).fetch_all(&self.pool).await?)
    }

    pub async fn list_agents_in(&self, org: Option<Uuid>) -> Result<Vec<WorldAgent>> {
        let sql = format!("SELECT {AGENT_COLUMNS} FROM agent_profiles WHERE agent_profiles.org_id = $1 ORDER BY created_at");
        Ok(sqlx::query_as::<_, WorldAgent>(&sql).bind(org).fetch_all(&self.pool).await?)
    }

    pub async fn get_agent(&self, id: Uuid) -> Result<Option<WorldAgent>> {
        let sql = format!("SELECT {AGENT_COLUMNS} FROM agent_profiles WHERE id = $1");
        Ok(sqlx::query_as::<_, WorldAgent>(&sql).bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn set_agent_status(&self, id: Uuid, status: &str, detail: Option<&str>) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE agent_profiles SET status = $2, status_detail = $3, updated_at = NOW()
             WHERE id = $1 AND (status IS DISTINCT FROM $2 OR status_detail IS DISTINCT FROM $3)",
        )
        .bind(id)
        .bind(status)
        .bind(detail)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed > 0 {
            self.emit(
                NewEvent::new("agent.status")
                    .agent(id)
                    .payload(serde_json::json!({ "status": status, "detail": detail })),
                None,
            )
            .await?;
        }
        Ok(())
    }

    async fn clear_clarification_waiting(&self, agents: &[Uuid]) -> Result<()> {
        for agent_id in agents {
            let reset = sqlx::query_scalar::<_, Uuid>(
                "UPDATE agent_profiles SET status = 'idle', status_detail = NULL, updated_at = NOW()
                 WHERE id = $1 AND status = 'waiting' AND status_detail = 'Waiting for answers'
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_sessions
                     WHERE agent_id = $1 AND plan_state = 'clarifying' AND status <> 'archived'
                   ) RETURNING id",
            )
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await?;
            if reset.is_some() {
                self.emit(
                    NewEvent::new("agent.status").agent(*agent_id).payload(json!({ "status": "idle", "detail": null })),
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn claim_agent_inbox(&self, worker: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "UPDATE agent_profiles SET inbox_claimed_by = $1, inbox_claimed_at = NOW()
             WHERE id = (
               SELECT a.id FROM agent_profiles a
               WHERE a.paused_reason IS NULL
                 AND a.runner_id IS NULL
                 AND a.inbox_claimed_by IS NULL
                 AND EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending' AND (i.retry_at IS NULL OR i.retry_at <= NOW()))
                 AND {ready}
               ORDER BY {urgent} DESC, (SELECT MIN(i.id) FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending')
               FOR UPDATE SKIP LOCKED
               LIMIT 1
             )
             RETURNING id",
            ready = inbox_ready_sql("a"),
            urgent = urgent_inbox_sql("a")
        ))
        .bind(worker)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn heartbeat_agent_claim(&self, agent_id: Uuid, worker: &str) -> Result<()> {
        sqlx::query(
            "UPDATE agent_profiles SET inbox_claimed_at = NOW() WHERE id = $1 AND inbox_claimed_by = $2",
        )
        .bind(agent_id)
        .bind(worker)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn release_agent_inbox(&self, agent_id: Uuid, worker: &str) -> Result<()> {
        sqlx::query(
            "UPDATE agent_profiles SET inbox_claimed_by = NULL, inbox_claimed_at = NULL
             WHERE id = $1 AND inbox_claimed_by = $2",
        )
        .bind(agent_id)
        .bind(worker)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn take_pending_inbox(&self, agent_id: Uuid, limit: i64) -> Result<Vec<InboxItem>> {
        self.drop_started_assignments(agent_id).await?;
        let mut items = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing', attempts = attempts + 1
             WHERE id IN (
               SELECT id FROM agent_inbox WHERE agent_id = $1 AND status = 'pending' AND NOT guest
                 AND (retry_at IS NULL OR retry_at <= NOW())
                 AND (NOT __OUTSIDE__ OR NOT EXISTS (
                   SELECT 1 FROM agent_inbox o
                   WHERE o.agent_id = $1 AND o.status = 'pending' AND NOT o.guest
                     AND (o.retry_at IS NULL OR o.retry_at <= NOW())
                     AND o.kind = ANY($3) AND __OWNER_SPOKE__))
               ORDER BY id LIMIT $2
             )
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at"
                .replace("__OUTSIDE__", crate::outside::SQL_OUTSIDE_KINDS)
                .replace("__OWNER_SPOKE__", &OWNER_SPOKE.replace("payload", "o.payload"))
                .as_str(),
        )
        .bind(agent_id)
        .bind(limit)
        .bind(OWNER_SPEECH_KINDS)
        .fetch_all(&self.pool)
        .await?;
        items.sort_by_key(|i| i.id);
        Ok(items)
    }

    pub async fn drop_started_assignments(&self, agent_id: Uuid) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE agent_inbox i SET status = 'done', error = 'Already started', done_at = NOW()
             WHERE i.agent_id = $1 AND i.status = 'pending' AND i.kind = 'task.assigned'
               AND EXISTS (
                 SELECT 1 FROM session_runs r
                 WHERE r.agent_id = i.agent_id AND r.task_id::text = i.payload->>'task_id'
                   AND r.kind IN ('task', 'followup') AND r.created_at >= i.created_at)",
        )
        .bind(agent_id)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn finish_inbox(&self, ids: &[i64], status: &str, error: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE agent_inbox SET status = $2, error = $3, done_at = NOW() WHERE id = ANY($1)",
        )
        .bind(ids)
        .bind(status)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_inbox(&self, ids: &[i64], error: &str) -> Result<()> {
        sqlx::query(&requeue_inbox_sql("id = ANY($1)"))
            .bind(ids)
            .bind(error)
            .bind(limit_retry_at(error))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn main_session(&self, agent_id: Uuid) -> Result<Option<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active'",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn request_context_reset(&self, agent_id: Uuid, payload: Value) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status IN ('pending', 'processing'))",
        )
        .bind(agent_id)
        .bind(RESET_INBOX_KIND)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            return Ok(false);
        }
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, $2, $3)")
            .bind(agent_id)
            .bind(RESET_INBOX_KIND)
            .bind(&payload)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn repend_inbox(&self, ids: &[i64]) -> Result<()> {
        sqlx::query(
            "UPDATE agent_inbox SET status = 'pending', attempts = GREATEST(attempts - 1, 0)
             WHERE id = ANY($1) AND status = 'processing'",
        )
        .bind(ids)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn notes_updated_since(&self, agent_id: Uuid, owner: Option<Uuid>, since: DateTime<Utc>) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM world_skills s WHERE {OWN_NOTES} AND s.updated_at >= $3)"
        ))
        .bind(agent_id)
        .bind(owner)
        .bind(since)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn mark_context_reset(&self, agent_id: Uuid, session_id: Uuid, old_session: Uuid) -> Result<()> {
        let message = self
            .insert_message(NewMessage {
                agent_id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: None,
                body: "Context reset",
                session_id: Some(session_id),
                run_id: None,
                meta: json!({ "kind": "reset" }),
            })
            .await?;
        self.emit(
            NewEvent::new("message.created")
                .agent(agent_id)
                .payload(json!({ "message_id": message.id, "author_kind": "system", "reset": true })),
            None,
        )
        .await?;
        self.emit(
            NewEvent::new("context.reset")
                .agent(agent_id)
                .session(session_id)
                .payload(json!({ "old_session": old_session })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn cancel_queued_turns(&self, session_id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let runs = sqlx::query_scalar::<_, Uuid>(
            "UPDATE session_runs SET status = 'cancelled', finished_at = NOW()
             WHERE session_id = $1 AND status = 'queued' AND kind = 'turn' RETURNING id",
        )
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await?;
        if !runs.is_empty() {
            sqlx::query(
                "UPDATE agent_inbox SET status = 'pending', attempts = GREATEST(attempts - 1, 0)
                 WHERE status = 'processing' AND id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = ANY($1))",
            )
            .bind(&runs)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn restart_main_session(&self, agent_id: Uuid, backend: &str, model: &str, notes: &str) -> Result<AgentSession> {
        let fork_backends = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT q.backend FROM session_runs r
             JOIN agent_sessions s ON s.id = r.fork_from
             JOIN agent_sessions q ON q.id = r.session_id
             WHERE s.agent_id = $1 AND s.role = 'main' AND s.status = 'active' AND r.status = 'queued'
               AND r.kind IN ('task', 'followup', 'review')",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        let mut memories = Vec::with_capacity(fork_backends.len());
        if !fork_backends.is_empty() {
            if let Some(agent) = self.get_agent(agent_id).await? {
                for fork_backend in fork_backends {
                    let intro = crate::context::fresh_intro(&agent.name, &fork_backend, None);
                    memories.push((self.fresh_start_memory(&agent, &intro).await?, fork_backend));
                }
            }
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let old = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR UPDATE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let old_id = old.as_ref().map(|session| session.id);
        let working_dir = old.as_ref().and_then(|session| session.working_dir.clone());
        if let Some(id) = old_id {
            sqlx::query("UPDATE agent_sessions SET status = 'archived', updated_at = NOW() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for (memory, fork_backend) in &memories {
                sqlx::query(
                    "WITH moved AS (
                       UPDATE session_runs r SET fork_from = NULL, prompt = $2 || E'\\n\\n' || r.prompt
                       FROM agent_sessions q
                       WHERE q.id = r.session_id AND q.backend = $3
                         AND r.fork_from = $1 AND r.status = 'queued' AND r.kind IN ('task', 'followup', 'review')
                       RETURNING r.session_id
                     )
                     UPDATE agent_sessions SET usage_baseline = '{}'::jsonb
                     WHERE id IN (SELECT session_id FROM moved) AND cli_session_id IS NULL",
                )
                .bind(id)
                .bind(memory)
                .bind(fork_backend)
                .execute(&mut *tx)
                .await?;
            }
        }
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir, summary, usage_baseline)
             VALUES ($1, 'main', $2, $3, $4, $5, '{}'::jsonb) RETURNING *",
        )
        .bind(agent_id)
        .bind(backend)
        .bind(model)
        .bind(working_dir)
        .bind(notes)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    pub async fn get_session(&self, id: Uuid) -> Result<Option<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>("SELECT * FROM agent_sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn list_sessions(&self, agent_id: Uuid) -> Result<Vec<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 100",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn create_session(&self, new: NewSession<'_>) -> Result<AgentSession> {
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(new.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, parent_session_id, task_id, working_dir, usage_baseline)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING *",
        )
        .bind(new.agent_id)
        .bind(new.role)
        .bind(new.backend)
        .bind(new.model)
        .bind(new.parent_session_id)
        .bind(new.task_id)
        .bind(new.working_dir)
        .bind(new.usage_baseline)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    pub async fn set_session_review(&self, id: Uuid, review: bool, reviewer: Option<&ReviewerChoice>) -> Result<AgentSession> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "UPDATE agent_sessions SET review = $2, reviewer = $3, updated_at = NOW() WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(review)
        .bind(reviewer.map(ReviewerChoice::to_value))
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn take_fork_baseline(&self, child: Uuid, parent: Uuid) -> Result<Option<Value>> {
        Ok(sqlx::query_scalar::<_, Value>(
            "UPDATE agent_sessions c SET usage_baseline = p.usage_baseline, updated_at = NOW()
             FROM agent_sessions p
             WHERE c.id = $1 AND p.id = $2 AND c.cli_session_id IS NULL AND c.backend = p.backend
             RETURNING c.usage_baseline",
        )
        .bind(child)
        .bind(parent)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn update_session_after_run(
        &self,
        id: Uuid,
        cli_session_id: Option<&str>,
        usage_baseline: Value,
        context_tokens: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        crate::run_recording::raise_session_baseline(&mut tx, id, &usage_baseline).await?;
        crate::run_recording::write_session_after_run(&mut tx, id, cli_session_id, Some(context_tokens)).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_session_cli_id(&self, id: Uuid, cli_session_id: &str) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2, updated_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(cli_session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_session_status(&self, id: Uuid, status: &str, summary: Option<&str>) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE agent_sessions SET status = $2, summary = COALESCE($3, summary), updated_at = NOW()
             WHERE id = $1 AND (status <> 'archived' OR $2 = 'archived')",
        )
        .bind(id)
        .bind(status)
        .bind(summary)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }

    pub async fn bump_review_round(&self, id: Uuid) -> Result<i32> {
        Ok(sqlx::query_scalar::<_, i32>(
            "UPDATE agent_sessions SET review_rounds = review_rounds + 1, updated_at = NOW()
             WHERE id = $1 RETURNING review_rounds",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn latest_review_output(&self, task_session_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT r.output FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id
             WHERE s.parent_session_id = $1 AND s.role = 'review' AND r.kind = 'review'
               AND (r.status = 'completed' OR r.cause = 'no_verdict')
               AND COALESCE((s.asked_by->>'manual')::boolean, false) = false
               AND r.created_at > COALESCE(
                 (SELECT MAX(created_at) FROM session_runs
                  WHERE session_id = $1 AND kind IN ('task', 'followup') AND NOT unchanged_work),
                 '-infinity')
             ORDER BY r.created_at DESC, r.id DESC LIMIT 1",
        )
        .bind(task_session_id)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    pub async fn mark_unchanged_work(&self, run_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE session_runs SET unchanged_work = TRUE WHERE id = $1 AND kind = 'followup'")
            .bind(run_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_run(&self, new: NewRun<'_>) -> Result<SessionRun> {
        Ok(sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs
               (session_id, agent_id, task_id, kind, status, fork_from, prompt, timeout_secs, started_at, request)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CASE WHEN $5 = 'running' THEN NOW() END, $9)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at, request",
        )
        .bind(new.session_id)
        .bind(new.agent_id)
        .bind(new.task_id)
        .bind(new.kind)
        .bind(new.status)
        .bind(new.fork_from)
        .bind(new.prompt)
        .bind(new.timeout_secs)
        .bind(new.request.as_ref().map(crate::run_request::RunRequest::value))
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn claim_queued_run(&self, worker: &str) -> Result<Option<SessionRun>> {
        Ok(sqlx::query_as::<_, SessionRun>(&format!(
            "UPDATE session_runs SET status = 'running', claimed_by = $1, started_at = NOW(), heartbeat_at = NOW()
             WHERE id = (
               SELECT q.id FROM session_runs q
               JOIN agent_sessions s ON s.id = q.session_id
               JOIN agent_profiles a ON a.id = q.agent_id
               WHERE q.status = 'queued'
                 AND q.runner_id IS NULL
                 AND s.runner_id IS NULL
                 AND a.paused_reason IS NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs r WHERE r.session_id = q.session_id AND r.status = 'running'
                 )
                 AND (q.kind <> 'followup' OR s.cli_session_id IS NOT NULL)
                 AND (q.not_before IS NULL OR q.not_before <= NOW())
               ORDER BY {urgent} DESC, q.created_at FOR UPDATE OF s SKIP LOCKED LIMIT 1
             )
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at",
            urgent = urgent_run_sql("q")
        ))
        .bind(worker)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn heartbeat_run(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE session_runs SET heartbeat_at = NOW() WHERE id = $1 AND status = 'running'")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_run(&self, id: Uuid, outcome: RunOutcome<'_>) -> Result<bool> {
        finish_run_on(&self.pool, id, outcome).await
    }

    pub async fn complete_run(
        &self,
        run: &SessionRun,
        output: &str,
        reported: &crate::run_recording::Reported,
        cli_session_id: Option<&str>,
        context_tokens: i64,
    ) -> Result<bool> {
        let reported = crate::run_recording::accounts_spend(&run.kind).then_some(reported);
        let mut tx = self.pool.begin().await?;
        let live = sqlx::query_scalar::<_, bool>("SELECT status IN ('queued', 'running') FROM session_runs WHERE id = $1 FOR UPDATE")
            .bind(run.id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
        if !live {
            if let Some(reported) = reported {
                crate::run_recording::keep_spend_of_closed_run(&mut tx, run.id, run.session_id, reported).await?;
                tx.commit().await?;
            }
            return Ok(false);
        }
        let billed = match reported {
            Some(reported) => crate::run_recording::bill_session(&mut tx, run.session_id, reported).await?,
            None => None,
        };
        let outcome = RunOutcome {
            status: "completed",
            output: Some(output),
            error: None,
            cost_usd: billed.as_ref().map_or(0.0, |billed| billed.cost_usd),
            input_tokens: billed.as_ref().map_or(0, |billed| billed.input_tokens),
            cached_input_tokens: billed.as_ref().map_or(0, |billed| billed.cached_input_tokens),
            output_tokens: billed.as_ref().map_or(0, |billed| billed.output_tokens),
        };
        finish_run_on(&mut *tx, run.id, outcome).await?;
        crate::run_recording::write_session_after_run(&mut tx, run.session_id, cli_session_id, Some(context_tokens)).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn requeue_for_space(&self, run: &SessionRun, error: &str) -> Result<bool> {
        if !crate::run_recording::waits_for_space(&run.kind, error) {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        if !crate::run_recording::requeue_for_space(&mut tx, run.id, None).await? {
            return Ok(false);
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn release_space_wait(&self, run: &SessionRun) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let released = sqlx::query_scalar::<_, bool>(
            "WITH prev AS (SELECT id, waiting_for FROM session_runs WHERE id = $1 FOR UPDATE),
                  cleared AS (UPDATE session_runs r SET waiting_for = NULL FROM prev WHERE r.id = prev.id AND prev.waiting_for IS NOT NULL RETURNING r.id)
             SELECT EXISTS (SELECT 1 FROM cleared)",
        )
        .bind(run.id)
        .fetch_one(&mut *tx)
        .await?;
        if released {
            crate::run_recording::emit_waiting(&mut tx, run.id, run.session_id, run.agent_id, run.task_id, &run.kind, false).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn previous_run_id(&self, session_id: Uuid, run_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT p.id FROM session_runs p, session_runs r
             WHERE r.id = $2 AND p.session_id = $1 AND p.id <> $2
               AND (p.created_at, p.id) < (r.created_at, r.id)
             ORDER BY p.created_at DESC, p.id DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn last_run_id(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM session_runs WHERE session_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn list_runs(&self, agent_id: Uuid, limit: i64) -> Result<Vec<SessionRun>> {
        Ok(sqlx::query_as::<_, SessionRun>(
            "SELECT id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                    timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                    created_at, started_at, finished_at
             FROM session_runs WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_runs_for_session(&self, session_id: Uuid) -> Result<Vec<SessionRunView>> {
        self.list_runs_page(session_id, None, i64::MAX, false).await
    }

    pub async fn list_runs_page(
        &self,
        session_id: Uuid,
        before: Option<DateTime<Utc>>,
        limit: i64,
        light: bool,
    ) -> Result<Vec<SessionRunView>> {
        let runs = sqlx::query_as::<_, SessionRun>(
            "SELECT * FROM (
               SELECT id, session_id, agent_id, task_id, kind, status, fork_from,
                      CASE WHEN $4 THEN '' ELSE prompt END AS prompt,
                      CASE WHEN $4 THEN NULL ELSE output END AS output, error,
                      timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                      created_at, started_at, finished_at, request
               FROM session_runs
               WHERE session_id = $1 AND ($2::timestamptz IS NULL OR created_at < $2)
               ORDER BY created_at DESC LIMIT $3
             ) page ORDER BY created_at",
        )
        .bind(session_id)
        .bind(before)
        .bind(limit.max(1))
        .bind(light)
        .fetch_all(&self.pool)
        .await?;
        let ids: Vec<Uuid> = runs.iter().map(|run| run.id).collect();
        let states = sqlx::query_as::<_, (Uuid, bool, Option<String>, Option<Uuid>, bool, Option<String>)>(
            "SELECT r.id,
                    COALESCE(r.status IN ('failed', 'timed_out')
                    AND s.status <> 'archived'
                    AND NULLIF(BTRIM(s.working_dir), '') IS NOT NULL
                    AND NOT EXISTS (
                      SELECT 1 FROM session_runs newer
                      WHERE newer.session_id = r.session_id
                        AND (newer.created_at, newer.id) > (r.created_at, r.id)
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM session_runs active
                      WHERE active.status IN ('queued', 'running')
                        AND (active.session_id = r.session_id OR (r.task_id IS NOT NULL AND active.task_id = r.task_id))
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM agent_sessions newer_session
                      WHERE r.task_id IS NOT NULL AND s.role IN ('task', 'review')
                        AND newer_session.task_id = r.task_id AND newer_session.role = s.role
                        AND (newer_session.created_at, newer_session.id) > (s.created_at, s.id)
                    )
                    AND CASE r.kind
                      WHEN 'turn' THEN s.role = 'main' AND r.prompt <> '/compact'
                        AND NOT EXISTS (
                          SELECT 1 FROM agent_inbox i
                          WHERE i.agent_id = r.agent_id AND i.status IN ('pending', 'processing')
                        )
                        AND NOT EXISTS (
                          SELECT 1 FROM world_events e
                          WHERE e.run_id = r.id AND e.type = 'message.created' AND e.payload->>'retry' = 'true'
                        )
                      WHEN 'task' THEN s.role = 'task' AND s.status = 'failed'
                        AND t.status = 'failed' AND t.pr_url IS NULL
                      WHEN 'review' THEN s.role = 'review' AND s.status = 'failed'
                        AND (COALESCE((s.asked_by->>'manual')::boolean, false)
                          OR (t.status = 'failed' AND s.parent_session_id IS NOT NULL))
                      WHEN 'followup' THEN s.role IN ('task', 'review')
                        AND (s.status = 'completed'
                          OR (s.role = 'task' AND s.status = 'failed' AND t.status = 'failed'))
                      WHEN 'aside' THEN s.role = 'aside' AND s.status = 'failed'
                      ELSE false
                    END, false),
                    CASE WHEN s.cli_session_id IS NULL THEN 'restart' ELSE 'resume' END,
                    r.steered_into, r.interrupt, CASE WHEN r.status = 'queued' THEN r.waiting_for END
             FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id
             LEFT JOIN agent_tasks t ON t.id = r.task_id
             WHERE r.session_id = $1 AND r.id = ANY($2)",
        )
        .bind(session_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(id, retryable, mode, steered_into, interrupt, waiting_for)| {
            (id, (retryable, retryable.then_some(mode).flatten(), steered_into, interrupt, waiting_for))
        })
        .collect::<HashMap<_, _>>();
        Ok(runs
            .into_iter()
            .map(|run| {
                let (retryable, retry_mode, steered_into, interrupt, waiting_for) = states.get(&run.id).cloned().unwrap_or_default();
                SessionRunView { run, retryable, retry_mode, steered_into, interrupt, waiting_for }
            })
            .collect())
    }

    pub async fn active_task_runs(&self, agent_id: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_runs WHERE agent_id = $1 AND kind <> 'turn' AND status IN ('queued', 'running')",
        )
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn insert_message(&self, m: NewMessage<'_>) -> Result<AgentMessage> {
        self.insert_reply(m, None).await
    }

    pub async fn insert_reply(&self, m: NewMessage<'_>, reply_to: Option<Uuid>) -> Result<AgentMessage> {
        self.insert_reply_with(&self.pool, m, reply_to).await
    }

    pub async fn insert_reply_with<'e, E: sqlx::PgExecutor<'e>>(&self, executor: E, m: NewMessage<'_>, reply_to: Option<Uuid>) -> Result<AgentMessage> {
        Ok(sqlx::query_as::<_, AgentMessage>(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, author_agent_id, body, session_id, run_id, meta, reply_to)
             VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, '{}'::jsonb), $9) RETURNING *",
        )
        .bind(m.agent_id)
        .bind(m.author_kind)
        .bind(m.author_user_id)
        .bind(m.author_agent_id)
        .bind(m.body)
        .bind(m.session_id)
        .bind(m.run_id)
        .bind(if m.meta.is_null() { None } else { Some(&m.meta) })
        .bind(reply_to)
        .fetch_one(executor)
        .await?)
    }

    pub async fn insert_upload(&self, mime: &str, name: &str, bytes: &[u8], user: Option<Uuid>) -> Result<Upload> {
        Ok(sqlx::query_as::<_, Upload>(
            "INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by) VALUES ($1, $2, $3, $4, $5)
             RETURNING id, mime, name, size",
        )
        .bind(mime)
        .bind(name)
        .bind(bytes.len() as i32)
        .bind(bytes)
        .bind(user)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn get_upload(&self, id: Uuid, org: Option<Uuid>) -> Result<Option<(String, Vec<u8>)>> {
        Ok(sqlx::query_as::<_, (String, Vec<u8>)>("SELECT mime, bytes FROM world_uploads WHERE id = $1 AND ($2::uuid IS NULL OR org_id = $2)")
            .bind(id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn upload_infos(&self, ids: &[Uuid]) -> Result<Vec<(Uuid, String, i32)>> {
        Ok(sqlx::query_as::<_, (Uuid, String, i32)>("SELECT id, mime, size FROM world_uploads WHERE id = ANY($1)").bind(ids).fetch_all(&self.pool).await?)
    }

    pub async fn upload_name(&self, id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>("SELECT name FROM world_uploads WHERE id = $1").bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn own_uploads(&self, ids: &[Uuid], user: Uuid, org: Option<Uuid>) -> Result<Vec<Upload>> {
        Ok(sqlx::query_as::<_, Upload>(
            "SELECT id, mime, name, size FROM world_uploads
             WHERE id = ANY($1) AND uploaded_by = $2 AND org_id IS NOT DISTINCT FROM $3",
        )
        .bind(ids)
        .bind(user)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn upload_for_viewer(&self, id: Uuid, viewer: &crate::access::Viewer) -> Result<Option<(String, Vec<u8>)>> {
        let Some(org) = viewer.org else {
            return Ok(None);
        };
        let admin = viewer.role == crate::access::Role::Admin;
        Ok(sqlx::query_as::<_, (String, Vec<u8>)>(
            "SELECT u.mime, u.bytes FROM world_uploads u
             WHERE u.id = $1 AND u.org_id = $2
               AND (
                 u.uploaded_by = $3
                 OR EXISTS (
                   SELECT 1 FROM agent_messages m JOIN agent_profiles a ON a.id = m.agent_id
                   WHERE m.meta ? 'attachments'
                     AND m.meta->'attachments' @> jsonb_build_array(jsonb_build_object('id', u.id::text))
                     AND a.org_id = $2
                     AND ($4 OR a.visibility = 'public' OR a.owner_user_id = $3)
                     AND COALESCE((m.meta->>'thread_user_id')::uuid, CASE WHEN m.author_kind = 'user' THEN m.author_user_id END, a.owner_user_id) = $3
                 )
                 OR EXISTS (
                   SELECT 1 FROM channel_messages m JOIN channels c ON c.id = m.channel_id
                   WHERE m.meta ? 'attachments'
                     AND m.meta->'attachments' @> jsonb_build_array(jsonb_build_object('id', u.id::text))
                     AND c.org_id = $2
                     AND (c.kind = 'channel' OR EXISTS (
                       SELECT 1 FROM channel_members cm
                       WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $3
                     ))
                 )
                 OR EXISTS (
                   SELECT 1 FROM world_reports r
                   WHERE r.org_id = $2
                     AND ($4 OR r.visibility = 'public' OR r.owner_user_id = $3)
                     AND strpos(r.body, '/api/world/uploads/' || u.id::text) > 0
                 )
                 OR EXISTS (
                   SELECT 1 FROM world_practice_attempts p JOIN world_reports r ON r.id = p.report_id
                   WHERE p.image_upload_id = u.id AND r.org_id = $2
                     AND ($4 OR r.visibility = 'public' OR r.owner_user_id = $3)
                 )
               )",
        )
        .bind(id)
        .bind(org)
        .bind(viewer.user)
        .bind(admin)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn edit_user_message(&self, agent_id: Uuid, message_id: Uuid, user_id: Uuid, body: &str) -> Result<Option<(AgentMessage, String)>> {
        let mut tx = self.pool.begin().await?;
        let previous = sqlx::query_scalar::<_, String>(
            "SELECT body FROM agent_messages
             WHERE id = $1 AND agent_id = $2 AND author_kind = 'user' AND author_user_id = $3
             FOR UPDATE",
        )
        .bind(message_id)
        .bind(agent_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(previous) = previous else {
            return Ok(None);
        };
        let message = sqlx::query_as::<_, AgentMessage>(
            "UPDATE agent_messages SET body = $2, edited_at = now() WHERE id = $1
             RETURNING *, (SELECT role FROM agent_sessions s WHERE s.id = agent_messages.session_id) AS session_role",
        )
        .bind(message_id)
        .bind(body)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((message, previous)))
    }

    pub async fn replace_pending_message(
        &self,
        agent_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
        body: &str,
        meta: &Value,
        payload: &Value,
    ) -> Result<Option<AgentMessage>> {
        let mut tx = self.pool.begin().await?;
        let replaced = sqlx::query(
            "UPDATE agent_inbox SET payload = ($3 - 'from_owner' - 'trusted')
               || jsonb_build_object('from_owner', COALESCE(payload->'from_owner', 'false'::jsonb), 'trusted', COALESCE(payload->'trusted', 'false'::jsonb))
             WHERE agent_id = $1 AND kind = 'message' AND status = 'pending' AND payload->>'message_id' = $2",
        )
        .bind(agent_id)
        .bind(message_id.to_string())
        .bind(payload)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if replaced == 0 {
            return Ok(None);
        }
        let message = sqlx::query_as::<_, AgentMessage>(
            "UPDATE agent_messages SET body = $4, meta = meta || $5, edited_at = now()
             WHERE id = $1 AND agent_id = $2 AND author_kind = 'user' AND author_user_id = $3
             RETURNING *, (SELECT role FROM agent_sessions s WHERE s.id = agent_messages.session_id) AS session_role",
        )
        .bind(message_id)
        .bind(agent_id)
        .bind(user_id)
        .bind(body)
        .bind(meta)
        .fetch_optional(&mut *tx)
        .await?;
        if message.is_some() {
            tx.commit().await?;
        }
        Ok(message)
    }

    pub async fn rewind_to_message(&self, agent_id: Uuid, message_id: Uuid, user_id: Uuid) -> Result<Rewound> {
        let mut tx = self.pool.begin().await?;
        let claimed = sqlx::query_scalar::<_, Option<String>>(
            "SELECT inbox_claimed_by FROM agent_profiles WHERE id = $1 FOR UPDATE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(claimed) = claimed else {
            return Ok(Rewound::NotFound);
        };
        if claimed.is_some() {
            return Ok(Rewound::Busy);
        }
        let Some(sent_at) = sqlx::query_scalar::<_, DateTime<Utc>>(
            "SELECT created_at FROM agent_messages
             WHERE id = $1 AND agent_id = $2 AND author_kind = 'user' AND author_user_id = $3",
        )
        .bind(message_id)
        .bind(agent_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(Rewound::NotFound);
        };
        let main = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR UPDATE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(main) = &main {
            let busy = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM session_runs WHERE session_id = $1 AND status IN ('queued', 'running'))",
            )
            .bind(main.id)
            .fetch_one(&mut *tx)
            .await?;
            if busy {
                return Ok(Rewound::Busy);
            }
        }
        let item = sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT status, created_at FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message' AND payload->>'message_id' = $2
             ORDER BY id LIMIT 1",
        )
        .bind(agent_id)
        .bind(message_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let mut rewind = None;
        if let (Some(main), Some((status, queued_at))) = (&main, &item) {
            if status != "pending" {
                let dropped = sqlx::query_as::<_, (DateTime<Utc>, String)>(
                    "SELECT created_at, prompt FROM session_runs
                     WHERE session_id = $1 AND kind = 'turn' AND created_at >= $2
                     ORDER BY created_at LIMIT 1",
                )
                .bind(main.id)
                .bind(queued_at)
                .fetch_optional(&mut *tx)
                .await?;
                let Some((dropped_at, dropped_prompt)) = dropped else {
                    return Ok(Rewound::Unsupported(
                        "That message was answered by an earlier mind of this agent, so it cannot be rewound".into(),
                    ));
                };
                let kept = sqlx::query_as::<_, (Option<String>,)>(
                    "SELECT checkpoint FROM session_runs
                     WHERE session_id = $1 AND kind = 'turn' AND status = 'completed' AND created_at < $2
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(main.id)
                .bind(dropped_at)
                .fetch_optional(&mut *tx)
                .await?;
                rewind = Some(match (kept, main.backend.as_str()) {
                    (None, _) => json!({ "fresh": true }),
                    (Some((Some(at),)), "claude" | "cursor" | "opencode") => json!({ "from": main.cli_session_id, "at": at, "before": dropped_at }),
                    (Some(_), "codex" | "omp") => json!({
                        "from": main.cli_session_id,
                        "drop": dropped_prompt.chars().take(400).collect::<String>(),
                        "before": dropped_at,
                    }),
                    (Some(_), _) => json!({ "fresh": true, "memory": kept_memory(&mut tx, main.id, dropped_at).await? }),
                });
            }
        }
        let deleted = sqlx::query_scalar::<_, Uuid>(&format!(
            "DELETE FROM agent_messages m
             WHERE m.agent_id = $1 AND m.created_at >= $2
               AND COALESCE(m.meta->>'kind', '') <> 'quest'
               AND NOT EXISTS (SELECT 1 FROM agent_sessions s WHERE s.id = m.session_id AND s.role = 'aside')
               AND {}
             RETURNING m.id",
            crate::guests::thread_filter("$1", "$3")
        ))
        .bind(agent_id)
        .bind(sent_at)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let ids: Vec<String> = deleted.iter().map(Uuid::to_string).collect();
        sqlx::query(
            "DELETE FROM agent_inbox
             WHERE agent_id = $1 AND status = 'pending' AND payload->>'message_id' = ANY($2)",
        )
        .bind(agent_id)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
        if let (Some(main), Some(rewind)) = (&main, &rewind) {
            let fresh = rewind.get("fresh").is_some();
            sqlx::query(
                "UPDATE agent_sessions
                 SET rewind = $2 - 'memory',
                     summary = CASE WHEN $3 THEN COALESCE($2->>'memory', summary) ELSE summary END,
                     cli_session_id = CASE WHEN $3 THEN NULL ELSE cli_session_id END,
                     usage_baseline = CASE WHEN $3 THEN '{}'::jsonb ELSE usage_baseline END,
                     rules_hash = CASE WHEN $3 THEN NULL ELSE rules_hash END,
                     context_tokens = CASE WHEN $3 THEN 0 ELSE context_tokens END,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(main.id)
            .bind(rewind)
            .bind(fresh)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Rewound::Done { deleted, rewound_session: rewind.is_some() })
    }

    pub async fn fall_back_to_replay(&self, session_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let rewind = sqlx::query_scalar::<_, Option<Value>>(
            "SELECT rewind FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let Some(before) = rewind
            .as_ref()
            .filter(|r| r.get("fresh").is_none())
            .and_then(|r| r.get("before"))
            .and_then(|v| serde_json::from_value::<DateTime<Utc>>(v.clone()).ok())
        else {
            return Ok(false);
        };
        let memory = kept_memory(&mut tx, session_id, before).await?;
        sqlx::query(
            "UPDATE agent_sessions
             SET rewind = $2, summary = $3, cli_session_id = NULL, usage_baseline = '{}'::jsonb,
                 rules_hash = NULL, context_tokens = 0, updated_at = NOW()
             WHERE id = $1",
        )
        .bind(session_id)
        .bind(json!({ "fresh": true }))
        .bind(memory)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn post_quest_started(&self, agent_id: Uuid, task_id: Uuid, task_session_id: Uuid, title: &str, brief: Option<&str>) -> Result<()> {
        let main = self.main_session(agent_id).await?;
        let msg = self
            .insert_message(NewMessage {
                agent_id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: Some(agent_id),
                body: brief.map(str::trim).unwrap_or(""),
                session_id: main.map(|m| m.id),
                run_id: None,
                meta: json!({
                    "kind": "quest",
                    "title": title,
                    "status": "started",
                    "task_id": task_id,
                    "session_id": task_session_id,
                }),
            })
            .await?;
        self.emit(
            NewEvent::new("message.created")
                .agent(agent_id)
                .task(Some(task_id))
                .payload(json!({ "message_id": msg.id, "author_kind": "system", "quest": "started", "quiet": true })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn clear_session_rewind(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET rewind = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_run_checkpoint(&self, run_id: Uuid, checkpoint: Option<&str>) -> Result<()> {
        if let Some(checkpoint) = checkpoint.filter(|c| !c.is_empty()) {
            sqlx::query("UPDATE session_runs SET checkpoint = $2 WHERE id = $1")
                .bind(run_id)
                .bind(checkpoint)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn list_messages(&self, agent_id: Uuid, viewer: Uuid, limit: i64) -> Result<Vec<AgentMessage>> {
        self.list_messages_page(agent_id, viewer, limit, None, None).await
    }

    pub async fn list_messages_page(
        &self,
        agent_id: Uuid,
        viewer: Uuid,
        limit: i64,
        after: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
    ) -> Result<Vec<AgentMessage>> {
        let order = if after.is_some() { "ASC" } else { "DESC" };
        let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        let window = format!(
            "AND ($3::timestamptz IS NULL OR m.created_at >= $3 OR m.edited_at >= $3)
             AND ($4::timestamptz IS NULL OR m.created_at < $4)
             AND m.reply_to IS NULL
             AND s.role IS DISTINCT FROM 'quick'
             AND {}",
            crate::guests::thread_filter_known(owner == Some(viewer), "$5")
        );
        let sql = format!(
            "SELECT * FROM (
               (SELECT m.*, s.role AS session_role FROM agent_messages m
                LEFT JOIN agent_sessions s ON s.id = m.session_id
                WHERE m.agent_id = $1 {window}
                ORDER BY m.created_at {order} LIMIT $2)
               UNION ALL
               (SELECT m.*, s.role AS session_role FROM agent_messages m
                LEFT JOIN agent_sessions s ON s.id = m.session_id
                WHERE m.author_kind = 'agent' AND m.author_agent_id = $1 AND m.agent_id <> $1 AND m.author_agent_id <> m.agent_id {window}
                ORDER BY m.created_at {order} LIMIT $2)
             ) page
             ORDER BY page.created_at {order} LIMIT $2"
        );
        let mut rows = sqlx::query_as::<_, AgentMessage>(&sql)
            .bind(agent_id)
            .bind(limit)
            .bind(after)
            .bind(before)
            .bind(viewer)
            .fetch_all(&self.pool)
            .await?;
        if after.is_none() {
            rows.reverse();
        }
        self.fill_reply_authors(&mut rows, viewer, owner == Some(viewer)).await?;
        Ok(rows)
    }

    async fn fill_reply_authors(&self, rows: &mut [AgentMessage], viewer: Uuid, viewer_owns_agent: bool) -> Result<()> {
        let roots: Vec<Uuid> = rows.iter().filter(|m| m.reply_count > 0).map(|m| m.id).collect();
        if roots.is_empty() {
            return Ok(());
        }
        let authors = sqlx::query_as::<_, (Uuid, String, Option<Uuid>, Option<Uuid>, Option<String>)>(&format!(
            "SELECT m.reply_to, m.author_kind, m.author_user_id, m.author_agent_id,
                    COALESCE(a.name, u.name, split_part(u.email, '@', 1)) AS name
             FROM agent_messages m
             LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
             LEFT JOIN users u ON u.id = m.author_user_id
             WHERE m.reply_to = ANY($1) AND {}
             GROUP BY 1, 2, 3, 4, 5
             ORDER BY MAX(m.created_at) DESC",
            crate::guests::thread_filter_known(viewer_owns_agent, "$2")
        ))
        .bind(&roots)
        .bind(viewer)
        .fetch_all(&self.pool)
        .await?;
        let mut by_root: HashMap<Uuid, Vec<Value>> = HashMap::new();
        for (root, kind, user_id, agent_id, name) in authors {
            let list = by_root.entry(root).or_default();
            if list.len() < 4 {
                list.push(json!({ "kind": kind, "user_id": user_id, "agent_id": agent_id, "name": name }));
            }
        }
        for row in rows.iter_mut() {
            if let Some(list) = by_root.remove(&row.id) {
                row.reply_authors = Some(Value::Array(list));
            }
        }
        Ok(())
    }

    pub async fn events_since_in(&self, org: Uuid, after_seq: i64, limit: i64) -> Result<Vec<WorldEvent>> {
        Ok(sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND org_id = $3 ORDER BY seq LIMIT $2",
        )
        .bind(after_seq)
        .bind(limit)
        .bind(org)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn replay_gap(&self, org: Uuid, after_seq: i64, cap: i64) -> Result<ReplayGap> {
        let (head, missed): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COALESCE(MAX(seq), 0) FROM world_events WHERE org_id = $2),
                    (SELECT COUNT(*) FROM (SELECT 1 FROM world_events WHERE seq > $1 AND org_id = $2 LIMIT $3) gap)",
        )
        .bind(after_seq)
        .bind(org)
        .bind(cap + 1)
        .fetch_one(&self.pool)
        .await?;
        Ok(ReplayGap { head, missed })
    }

    pub async fn recent_events(&self, agent_id: Option<Uuid>, limit: i64) -> Result<Vec<WorldEvent>> {
        self.recent_events_in(None, agent_id, limit).await
    }

    pub async fn recent_events_in(&self, org: Option<Uuid>, agent_id: Option<Uuid>, limit: i64) -> Result<Vec<WorldEvent>> {
        let mut rows = sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM world_events WHERE ($1::uuid IS NULL OR agent_id = $1) AND ($3::uuid IS NULL OR org_id = $3) ORDER BY seq DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    pub async fn agent_feed(&self, org: Option<Uuid>, agent_id: Option<Uuid>, types: &[String], limit: i64) -> Result<Vec<WorldEvent>> {
        let patterns: Vec<String> = types
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| match t.strip_suffix('*') {
                Some(prefix) => format!("{}%", prefix.replace('%', "")),
                None if t.ends_with('.') => format!("{}%", t.replace('%', "")),
                None => t.replace('%', ""),
            })
            .collect();
        let mut rows = sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM world_events
             WHERE (cardinality($1::text[]) = 0 OR type LIKE ANY($1))
               AND ($2::uuid IS NULL OR agent_id = $2)
               AND ($4::uuid IS NULL OR org_id = $4)
               AND COALESCE(payload->>'quiet', '') <> 'true'
             ORDER BY seq DESC LIMIT $3",
        )
        .bind(&patterns)
        .bind(agent_id)
        .bind(limit)
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    pub async fn next_schedule_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT MIN(s.next_run_at) FROM schedules s
             JOIN agent_profiles a ON a.id = s.agent_id
             WHERE s.agent_id IS NOT NULL AND s.enabled = true AND a.runner_id IS NULL",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn next_runner_schedule_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT MIN(s.next_run_at) FROM schedules s
             JOIN agent_profiles a ON a.id = s.agent_id
             JOIN world_runners r ON r.id = a.runner_id
             JOIN users u ON u.id = r.owner_user_id
             WHERE s.agent_id IS NOT NULL AND s.enabled = true AND a.runner_id IS NOT NULL
               AND a.owner_user_id = r.owner_user_id AND r.status NOT IN ('paused', 'revoked')
               AND u.world_role IN ('write', 'admin')",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn schedules_missing_next_run(&self) -> Result<Vec<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, kind FROM schedules
             WHERE agent_id IS NOT NULL AND enabled = true AND next_run_at IS NULL
               AND EXISTS (SELECT 1 FROM agent_profiles a WHERE a.id = schedules.agent_id AND a.runner_id IS NULL)",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn runner_schedules_missing_next_run(&self) -> Result<Vec<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT s.id, s.agent_id, s.name, s.prompt, s.cron_expression, s.run_at, s.next_run_at, s.kind
             FROM schedules s
             JOIN agent_profiles a ON a.id = s.agent_id
             JOIN world_runners r ON r.id = a.runner_id
             JOIN users u ON u.id = r.owner_user_id
             WHERE s.agent_id IS NOT NULL AND s.enabled = true AND s.next_run_at IS NULL
               AND a.owner_user_id = r.owner_user_id AND r.status NOT IN ('paused', 'revoked')
               AND u.world_role IN ('write', 'admin')",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_schedule_next_run(&self, id: Uuid, next: Option<DateTime<Utc>>, disable: bool) -> Result<()> {
        sqlx::query(
            "UPDATE schedules SET next_run_at = $2, enabled = CASE WHEN $3 THEN false ELSE enabled END WHERE id = $1",
        )
        .bind(id)
        .bind(next)
        .bind(disable)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_due_schedules(&self) -> Result<Vec<AgentSchedule>> {
        self.drop_expired_schedules().await?;
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "UPDATE schedules SET next_run_at = NULL
             WHERE id IN (
               SELECT id FROM schedules
               WHERE agent_id IS NOT NULL AND enabled = true AND next_run_at <= NOW()
                 AND EXISTS (SELECT 1 FROM agent_profiles a WHERE a.id = schedules.agent_id AND a.runner_id IS NULL)
               FOR UPDATE SKIP LOCKED
             )
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, kind",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn claim_due_runner_schedules(&self) -> Result<Vec<AgentSchedule>> {
        self.drop_expired_schedules().await?;
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "UPDATE schedules SET next_run_at = NULL
             WHERE id IN (
               SELECT s.id FROM schedules s
               JOIN agent_profiles a ON a.id = s.agent_id
               JOIN world_runners r ON r.id = a.runner_id
               JOIN users u ON u.id = r.owner_user_id
               WHERE s.agent_id IS NOT NULL AND s.enabled = true AND s.next_run_at <= NOW()
                 AND a.owner_user_id = r.owner_user_id AND r.status NOT IN ('paused', 'revoked')
                 AND u.world_role IN ('write', 'admin')
               FOR UPDATE OF s SKIP LOCKED
             )
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, kind",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn sweep_stale(&self, agent_claim_secs: i64, run_heartbeat_secs: i64) -> Result<Vec<SessionRun>> {
        sqlx::query(
            "UPDATE agent_profiles SET inbox_claimed_by = NULL, inbox_claimed_at = NULL
             WHERE inbox_claimed_by IS NOT NULL AND inbox_claimed_at < NOW() - make_interval(secs => $1)",
        )
        .bind(agent_claim_secs as f64)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE agent_inbox i SET status = 'pending'
             WHERE status = 'processing'
               AND NOT EXISTS (SELECT 1 FROM agent_profiles p WHERE p.id = i.agent_id AND p.inbox_claimed_by IS NOT NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM runner_run_inbox x
                 JOIN session_runs r ON r.id = x.run_id
                 LEFT JOIN runner_leases l ON l.run_id = r.id
                 WHERE x.inbox_id = i.id
                   AND (r.status IN ('queued', 'running') OR l.status = 'finishing'))",
        )
        .execute(&self.pool)
        .await?;
        Ok(sqlx::query_as::<_, SessionRun>(
            "UPDATE session_runs SET status = 'failed', error = 'worker stopped responding', finished_at = NOW()
             WHERE status = 'running' AND kind <> 'turn' AND runner_id IS NULL
               AND heartbeat_at < NOW() - make_interval(secs => $1)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at",
        )
        .bind(run_heartbeat_secs as f64)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn agent_cost(&self, agent_id: Uuid) -> Result<(f64, i64)> {
        Ok(sqlx::query_as::<_, (f64, i64)>(
            "SELECT COALESCE((SELECT cost_usd::float8 FROM agent_usage_totals WHERE agent_id = $1), 0),
                    COALESCE((SELECT tokens FROM agent_usage_totals WHERE agent_id = $1), 0)",
        )
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn agent_costs(&self, agent_ids: &[Uuid]) -> Result<HashMap<Uuid, (f64, i64)>> {
        if agent_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query_as::<_, (Uuid, f64, i64)>(
            "SELECT agent_id, cost_usd::float8, tokens FROM agent_usage_totals WHERE agent_id = ANY($1)",
        )
        .bind(agent_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id, cost, tokens)| (id, (cost, tokens))).collect())
    }

    pub async fn agent_usage(&self, agent_id: Uuid, days: i32) -> Result<AgentUsage> {
        let mut total = sqlx::query_as::<_, AgentUsageRow>(
            "SELECT 'total' AS bucket, COUNT(*)::int8 AS runs, COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    COALESCE(SUM(r.input_tokens), 0)::int8 AS input_tokens,
                    COALESCE(SUM(r.cached_input_tokens), 0)::int8 AS cached_input_tokens,
                    COALESCE(SUM(r.output_tokens), 0)::int8 AS output_tokens
             FROM session_runs r
             WHERE r.agent_id = $1 AND r.created_at >= NOW() - make_interval(days => $2)",
        )
        .bind(agent_id)
        .bind(days)
        .fetch_one(&self.pool)
        .await?;
        total.bucket = format!("Last {days} days");

        let per_day = sqlx::query_as::<_, AgentUsageRow>(
            "SELECT to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS bucket, COUNT(*)::int8 AS runs,
                    COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    COALESCE(SUM(r.input_tokens), 0)::int8 AS input_tokens,
                    COALESCE(SUM(r.cached_input_tokens), 0)::int8 AS cached_input_tokens,
                    COALESCE(SUM(r.output_tokens), 0)::int8 AS output_tokens
             FROM session_runs r
             WHERE r.agent_id = $1 AND r.created_at >= NOW() - make_interval(days => $2)
             GROUP BY 1 ORDER BY 1 DESC",
        )
        .bind(agent_id)
        .bind(days)
        .fetch_all(&self.pool)
        .await?;

        let per_kind = sqlx::query_as::<_, AgentUsageRow>(
            "SELECT r.kind AS bucket, COUNT(*)::int8 AS runs, COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    COALESCE(SUM(r.input_tokens), 0)::int8 AS input_tokens,
                    COALESCE(SUM(r.cached_input_tokens), 0)::int8 AS cached_input_tokens,
                    COALESCE(SUM(r.output_tokens), 0)::int8 AS output_tokens
             FROM session_runs r
             WHERE r.agent_id = $1 AND r.created_at >= NOW() - make_interval(days => $2)
             GROUP BY 1 ORDER BY 3 DESC, 2 DESC",
        )
        .bind(agent_id)
        .bind(days)
        .fetch_all(&self.pool)
        .await?;

        let per_backend = sqlx::query_as::<_, AgentUsageRow>(
            "SELECT s.backend || ' ' || s.model AS bucket, COUNT(*)::int8 AS runs, COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    COALESCE(SUM(r.input_tokens), 0)::int8 AS input_tokens,
                    COALESCE(SUM(r.cached_input_tokens), 0)::int8 AS cached_input_tokens,
                    COALESCE(SUM(r.output_tokens), 0)::int8 AS output_tokens
             FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id
             WHERE r.agent_id = $1 AND r.created_at >= NOW() - make_interval(days => $2)
             GROUP BY 1 ORDER BY 3 DESC, 2 DESC",
        )
        .bind(agent_id)
        .bind(days)
        .fetch_all(&self.pool)
        .await?;

        Ok(AgentUsage { days, total, per_day, per_kind, per_backend })
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentUsageRow {
    pub bucket: String,
    pub runs: i64,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    pub days: i32,
    pub total: AgentUsageRow,
    pub per_day: Vec<AgentUsageRow>,
    pub per_kind: Vec<AgentUsageRow>,
    pub per_backend: Vec<AgentUsageRow>,
}

const QUIET_WINDOW_MINUTES: i32 = 30;

fn failure_limit(event: &NewEvent) -> Option<i64> {
    let payload = &event.payload;
    match event.event_type.as_str() {
        "agent.status" if payload.get("status").and_then(Value::as_str) == Some("error") => Some(1),
        "run.failed" | "run.timed_out" => Some(3),
        "task.finished" if payload.get("error").is_some_and(|error| !error.is_null()) => Some(3),
        _ => None,
    }
}

async fn over_failure_limit(tx: &mut Transaction<'_, Postgres>, event: &NewEvent) -> Result<bool> {
    let (Some(agent_id), Some(limit)) = (event.agent_id, failure_limit(event)) else {
        return Ok(false);
    };
    let recent = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM world_events
         WHERE agent_id = $1 AND type = $2
           AND created_at > NOW() - make_interval(mins => $3)
           AND COALESCE(payload->>'quiet', '') <> 'true'
           AND (type <> 'agent.status' OR payload->>'status' = 'error')
           AND (type <> 'task.finished' OR COALESCE(jsonb_typeof(payload->'error'), 'null') <> 'null')",
    )
    .bind(agent_id)
    .bind(&event.event_type)
    .bind(QUIET_WINDOW_MINUTES)
    .fetch_one(&mut **tx)
    .await?;
    Ok(recent >= limit)
}

struct TaskCleanup {
    dropped: u64,
    running: Vec<Uuid>,
    archived: u64,
    clarification_agents: Vec<Uuid>,
}

fn closing_note(verb: &str, by: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("Quest {verb} by {by}: {reason}"),
        None => format!("Quest {verb} by {by}"),
    }
}

async fn reopen_failed_session(tx: &mut Transaction<'_, Postgres>, session_id: Uuid, task_id: Option<Uuid>) -> Result<bool> {
    let reopened = sqlx::query(
        "UPDATE agent_sessions s SET status = 'active', review_rounds = 0, updated_at = NOW()
         WHERE s.id = $1 AND s.role = 'task' AND s.status = 'failed'
           AND (s.attempt_state IS NULL OR s.attempt_state = 'won')
           AND NOT EXISTS (SELECT 1 FROM agent_tasks t WHERE t.id = s.task_id AND t.status = 'cancelled')",
    )
    .bind(session_id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;
    if !reopened {
        return Ok(false);
    }
    reopen_quest(tx, task_id).await
}

pub(crate) async fn reopen_quest(tx: &mut Transaction<'_, Postgres>, task_id: Option<Uuid>) -> Result<bool> {
    let Some(task_id) = task_id else { return Ok(false) };
    Ok(sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1 AND status = 'failed'")
        .bind(task_id)
        .execute(&mut **tx)
        .await?
        .rows_affected()
        == 1)
}

async fn release_quest(tx: &mut Transaction<'_, Postgres>, task_id: Uuid, agent_id: Uuid, note: &str) -> Result<(Vec<Uuid>, Vec<Uuid>)> {
    let work = "r.agent_id = $2 AND r.kind IN ('task', 'followup', 'review', 'publish')
                AND (r.task_id = $1 OR r.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))";
    sqlx::query(&format!(
        "UPDATE session_runs r SET status = 'cancelled', error = $3, finished_at = NOW()
         WHERE r.status = 'queued' AND {work}"
    ))
    .bind(task_id)
    .bind(agent_id)
    .bind(note)
    .execute(&mut **tx)
    .await?;
    let running = sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT r.id FROM session_runs r WHERE r.status = 'running' AND {work} ORDER BY r.created_at"
    ))
    .bind(task_id)
    .bind(agent_id)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE agent_sessions SET status = 'archived', updated_at = NOW()
         WHERE task_id = $1 AND agent_id = $2 AND role IN ('task', 'review') AND status <> 'archived'",
    )
    .bind(task_id)
    .bind(agent_id)
    .execute(&mut **tx)
    .await?;
    let clarification_agents = settle_task_clarifications(tx, task_id, Some(agent_id), note).await?;
    sqlx::query(
        "UPDATE agent_inbox SET status = 'done'
         WHERE agent_id = $2 AND status = 'pending' AND kind = 'task.assigned' AND payload->>'task_id' = $1::text",
    )
    .bind(task_id)
    .bind(agent_id)
    .execute(&mut **tx)
    .await?;
    Ok((running, clarification_agents))
}

async fn lock_task(tx: &mut Transaction<'_, Postgres>, task_id: Uuid) -> Result<TaskRef> {
    sqlx::query_as::<_, TaskRef>("SELECT id, title, status, assigned_agent_id, user_id FROM agent_tasks WHERE id = $1 FOR UPDATE")
        .bind(task_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("task not found"))
}

async fn clean_up_task(tx: &mut Transaction<'_, Postgres>, task_id: Uuid, note: &str) -> Result<TaskCleanup> {
    let work = "r.kind IN ('task', 'followup', 'review', 'publish')
                AND (r.task_id = $1 OR r.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))";
    let dropped = sqlx::query(&format!(
        "UPDATE session_runs r SET status = 'cancelled', error = $2, finished_at = NOW()
         WHERE r.status = 'queued' AND {work}"
    ))
    .bind(task_id)
    .bind(note)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let running = sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT r.id FROM session_runs r WHERE r.status = 'running' AND {work} ORDER BY r.created_at"
    ))
    .bind(task_id)
    .fetch_all(&mut **tx)
    .await?;
    let archived = sqlx::query(
        "UPDATE agent_sessions SET status = 'archived', updated_at = NOW()
         WHERE task_id = $1 AND role IN ('task', 'review') AND status <> 'archived'",
    )
    .bind(task_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let clarification_agents = settle_task_clarifications(tx, task_id, None, note).await?;
    sqlx::query(
        "UPDATE task_milestones SET status = 'skipped', detail = 'Quest closed', next_check_at = NULL, updated_at = NOW()
         WHERE task_id = $1 AND status IN ('pending', 'active')",
    )
    .bind(task_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE agent_inbox SET status = 'done'
         WHERE status = 'pending' AND kind = 'task.assigned' AND payload->>'task_id' = $1::text",
    )
    .bind(task_id)
    .execute(&mut **tx)
    .await?;
    Ok(TaskCleanup { dropped, running, archived, clarification_agents })
}

async fn settle_task_clarifications(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    note: &str,
) -> Result<Vec<Uuid>> {
    let cards = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid)>(
        "SELECT m.id, s.id, s.agent_id, a.owner_user_id
         FROM agent_sessions s
         JOIN agent_profiles a ON a.id = s.agent_id AND a.owner_user_id IS NOT NULL
         JOIN agent_messages m ON m.agent_id = s.agent_id AND m.meta->'clarification'->>'session_id' = s.id::text
         WHERE s.task_id = $1 AND ($2::uuid IS NULL OR s.agent_id = $2) AND s.plan_state = 'clarifying'
           AND NOT EXISTS (SELECT 1 FROM agent_messages answer WHERE answer.agent_id = m.agent_id AND answer.meta->>'answers' = m.id::text)
         ORDER BY m.created_at FOR UPDATE OF s, m",
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut agents = Vec::new();
    for (card_id, session_id, agent_id, owner) in cards {
        let message_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta)
             VALUES ($1, 'user', $2, $3, $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(owner)
        .bind(note)
        .bind(json!({ "answers": card_id, "auto": true }))
        .fetch_one(&mut **tx)
        .await?;
        emit_in(
            tx,
            &NewEvent::new("message.created")
                .agent(agent_id)
                .session(session_id)
                .task(Some(task_id))
                .actor(Some(owner))
                .payload(json!({
                    "message_id": message_id,
                    "author": "Loka",
                    "author_kind": "user",
                    "body": note,
                    "quiet": true,
                    "answer_card": card_id,
                })),
            None,
        )
        .await?;
        agents.push(agent_id);
    }
    agents.extend(sqlx::query_scalar::<_, Uuid>(
        "UPDATE agent_sessions SET plan_state = NULL, updated_at = NOW()
         WHERE task_id = $1 AND ($2::uuid IS NULL OR agent_id = $2) AND plan_state = 'clarifying'
         RETURNING agent_id",
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_all(&mut **tx)
    .await?);
    agents.sort_unstable();
    agents.dedup();
    Ok(agents)
}

pub const RUN_END_EVENTS: [&str; 3] = ["run.completed", "run.failed", "run.timed_out"];

async fn run_end_announced(tx: &mut Transaction<'_, Postgres>, run_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM world_events WHERE run_id = $1 AND type = ANY($2))")
        .bind(run_id)
        .bind(&RUN_END_EVENTS[..])
        .fetch_one(&mut **tx)
        .await?)
}

async fn finish_run_on(executor: impl sqlx::PgExecutor<'_>, id: Uuid, outcome: RunOutcome<'_>) -> Result<bool> {
    let live: Option<bool> = sqlx::query_scalar(
        "WITH prev AS (SELECT id, status IN ('queued', 'running') AS live FROM session_runs WHERE id = $1 FOR UPDATE)
         UPDATE session_runs r
         SET status = CASE WHEN prev.live THEN $2 ELSE r.status END,
             output = CASE WHEN prev.live THEN $3 ELSE r.output END,
             error = CASE WHEN prev.live THEN $4 ELSE r.error END,
             cost_usd = CASE WHEN prev.live AND $2 = 'completed' THEN $5 ELSE GREATEST(r.cost_usd, $5) END,
             input_tokens = CASE WHEN prev.live AND $2 = 'completed' THEN $6 ELSE GREATEST(r.input_tokens, $6) END,
             cached_input_tokens = CASE WHEN prev.live AND $2 = 'completed' THEN $7 ELSE GREATEST(r.cached_input_tokens, $7) END,
             output_tokens = CASE WHEN prev.live AND $2 = 'completed' THEN $8 ELSE GREATEST(r.output_tokens, $8) END,
             finished_at = CASE WHEN prev.live THEN NOW() ELSE COALESCE(r.finished_at, NOW()) END
         FROM prev WHERE r.id = prev.id
         RETURNING prev.live",
    )
    .bind(id)
    .bind(outcome.status)
    .bind(outcome.output.map(|output| output.replace('\0', "")))
    .bind(outcome.error.map(|error| error.replace('\0', "")))
    .bind(outcome.cost_usd)
    .bind(outcome.input_tokens)
    .bind(outcome.cached_input_tokens)
    .bind(outcome.output_tokens)
    .fetch_optional(executor)
    .await?;
    Ok(live.unwrap_or(false))
}

pub async fn emit_in(
    tx: &mut Transaction<'_, Postgres>,
    event: &NewEvent,
    wake: Option<(Uuid, &str)>,
) -> Result<i64> {
    let quiet = over_failure_limit(tx, event).await?;
    let mut payload = event.payload.clone();
    if let (true, Some(fields)) = (quiet, payload.as_object_mut()) {
        fields.insert("quiet".into(), Value::Bool(true));
    }
    let wake = if quiet { None } else { wake };
    let seq = sqlx::query_scalar::<_, i64>(
        "INSERT INTO world_events (type, agent_id, session_id, run_id, task_id, actor_user_id, payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING seq",
    )
    .bind(&event.event_type)
    .bind(event.agent_id)
    .bind(event.session_id)
    .bind(event.run_id)
    .bind(event.task_id)
    .bind(event.actor_user_id)
    .bind(&payload)
    .fetch_one(&mut **tx)
    .await?;
    if let Some((agent_id, kind)) = wake {
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload) VALUES ($1, $2, $3, $4)")
            .bind(agent_id)
            .bind(kind)
            .bind(seq)
            .bind(&payload)
            .execute(&mut **tx)
            .await?;
    }
    Ok(seq)
}

pub fn raw_skill_url(url: &str) -> String {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        if let Some((repo, path)) = rest.split_once("/blob/") {
            let full = format!("https://raw.githubusercontent.com/{repo}/{path}");
            return if full.ends_with(".md") { full } else { format!("{}/SKILL.md", full.trim_end_matches('/')) };
        }
        if let Some((repo, path)) = rest.split_once("/tree/") {
            return format!("https://raw.githubusercontent.com/{repo}/{}/SKILL.md", path.trim_end_matches('/'));
        }
    }
    url.to_string()
}

pub async fn fetch_skill(url: &str) -> Result<(Option<String>, Option<String>, String)> {
    let raw = raw_skill_url(url);
    let text = crate::feeds::get_public_text(&raw, 200_000).await?;
    Ok(parse_skill_markdown(&text))
}

const OWN_NOTES: &str = "s.name LIKE '%-notes' AND s.scope = 'agent' AND s.created_by IS NOT DISTINCT FROM $2
     AND EXISTS (SELECT 1 FROM agent_skills a WHERE a.skill_id = s.id AND a.agent_id = $1)
     AND NOT EXISTS (SELECT 1 FROM agent_skills a WHERE a.skill_id = s.id AND a.agent_id <> $1)";

pub const RESET_INBOX_KIND: &str = "context.reset";

pub fn is_reset_item(item: &InboxItem) -> bool {
    item.kind == RESET_INBOX_KIND
}

pub fn partition_reset_items(items: Vec<InboxItem>) -> (Vec<InboxItem>, Vec<InboxItem>) {
    items.into_iter().partition(is_reset_item)
}

pub const NOTES_MAX_BYTES: usize = 8 * 1024;

pub fn check_notes(name: &str, markdown: &str) -> Result<String> {
    if markdown.len() > NOTES_MAX_BYTES {
        anyhow::bail!("notes are {} bytes; keep them under {NOTES_MAX_BYTES}", markdown.len());
    }
    let (front_name, description, body) = parse_skill_markdown(markdown);
    if front_name.as_deref() != Some(name) {
        anyhow::bail!("start with frontmatter whose name is {name}: ---\\nname: {name}\\ndescription: ...\\n---");
    }
    let description = description.ok_or_else(|| anyhow::anyhow!("add a description to the frontmatter"))?;
    if body.is_empty() {
        anyhow::bail!("add your notes below the frontmatter");
    }
    Ok(description)
}

pub const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("suggest-quests", include_str!("../../skills/suggest-quests/SKILL.md")),
    ("repo-team", include_str!("../../skills/repo-team/SKILL.md")),
    ("goal-kit", include_str!("../../skills/goal-kit/SKILL.md")),
];

pub fn builtin_skill(name: &str) -> Option<&'static str> {
    BUILTIN_SKILLS.iter().find(|(n, _)| *n == name.trim()).map(|(_, body)| *body)
}

pub fn parse_skill_markdown(markdown: &str) -> (Option<String>, Option<String>, String) {
    let trimmed = markdown.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (None, None, markdown.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None, markdown.to_string());
    };
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['-', '\n', '\r']);
    let field = |key: &str| {
        front.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .and_then(|v| v.trim_start().strip_prefix(':'))
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|v| !v.is_empty())
        })
    };
    (
        field("name"),
        field("description"),
        if body.trim().is_empty() { String::new() } else { markdown.to_string() },
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    fn steer_db() -> &'static tokio::sync::RwLock<()> {
        static STEER_DB: std::sync::OnceLock<tokio::sync::RwLock<()>> = std::sync::OnceLock::new();
        STEER_DB.get_or_init(|| tokio::sync::RwLock::new(()))
    }

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::{parse_skill_markdown, raw_skill_url, AgentUpdate, NewEvent, Nudge, QuestEdit, ReplayGap, Resume, RetryRunError, TaskResume, WorldStore, REPLAY_MAX_EVENTS, VERDICT_NUDGE};
    use crate::run_causes::Required;
    use chrono::{DateTime, Utc};
    use serde_json::{json, Value};
    use uuid::Uuid;

    #[test]
    fn converts_github_urls_to_raw() {
        assert_eq!(
            raw_skill_url("https://github.com/a/b/tree/main/skills/deploy"),
            "https://raw.githubusercontent.com/a/b/main/skills/deploy/SKILL.md"
        );
        assert_eq!(
            raw_skill_url("https://github.com/a/b/blob/main/x/SKILL.md"),
            "https://raw.githubusercontent.com/a/b/main/x/SKILL.md"
        );
    }

    #[test]
    fn parses_frontmatter() {
        let source = "---\nname: deploy\ndescription: \"Ship it\"\nmetadata:\n  owner: infra\n---\n# Steps\nrun";
        let (name, desc, body) = parse_skill_markdown(source);
        assert_eq!(name.as_deref(), Some("deploy"));
        assert_eq!(desc.as_deref(), Some("Ship it"));
        assert_eq!(body, source);
        let (n, d, b) = parse_skill_markdown("plain");
        assert!(n.is_none() && d.is_none());
        assert_eq!(b, "plain");
    }

    #[test]
    fn rejects_empty_skill_body() {
        let (name, desc, body) = parse_skill_markdown("---\nname: deploy\ndescription: Ship it\n---\n  \n");
        assert_eq!(name.as_deref(), Some("deploy"));
        assert_eq!(desc.as_deref(), Some("Ship it"));
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn retry_lifecycle_is_atomic() -> anyhow::Result<()> {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return Ok(());
        };
        let admin = PgPoolOptions::new().max_connections(1).connect(&database_url).await?;
        let schema = format!("world_retry_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await?;
        let options = PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new().max_connections(8).connect_with(options).await?;
        sqlx::raw_sql(
            "CREATE TABLE agent_tasks (
               id UUID PRIMARY KEY, status TEXT NOT NULL, pr_url TEXT, error_message TEXT
             );
             CREATE TABLE agent_profiles (id UUID PRIMARY KEY);
             CREATE TABLE agent_sessions (
               id UUID PRIMARY KEY, agent_id UUID NOT NULL, role TEXT NOT NULL, backend TEXT NOT NULL,
               model TEXT NOT NULL, cli_session_id TEXT, parent_session_id UUID, task_id UUID,
               working_dir TEXT, status TEXT NOT NULL, usage_baseline JSONB NOT NULL DEFAULT '{}',
               context_tokens BIGINT NOT NULL DEFAULT 0, review_rounds INTEGER NOT NULL DEFAULT 0,
               summary TEXT, rules_hash TEXT, asked_by JSONB, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
               updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
             );
             CREATE TABLE session_runs (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), session_id UUID NOT NULL, agent_id UUID NOT NULL,
               task_id UUID, kind TEXT NOT NULL, status TEXT NOT NULL, fork_from UUID, prompt TEXT NOT NULL,
               output TEXT, error TEXT, timeout_secs INTEGER NOT NULL DEFAULT 3600,
               cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0, input_tokens BIGINT NOT NULL DEFAULT 0,
               cached_input_tokens BIGINT NOT NULL DEFAULT 0, output_tokens BIGINT NOT NULL DEFAULT 0,
               claimed_by TEXT, heartbeat_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
               started_at TIMESTAMPTZ, finished_at TIMESTAMPTZ, request JSONB
             );
             CREATE TABLE world_events (
               seq BIGSERIAL PRIMARY KEY, type TEXT NOT NULL, agent_id UUID, session_id UUID, run_id UUID,
               task_id UUID, actor_user_id UUID, payload JSONB NOT NULL DEFAULT '{}', created_at TIMESTAMPTZ DEFAULT NOW()
             );
             CREATE TABLE agent_messages (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), agent_id UUID NOT NULL, session_id UUID,
               run_id UUID, author_kind TEXT NOT NULL, author_user_id UUID, author_agent_id UUID,
               body TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
               meta JSONB NOT NULL DEFAULT '{}'
             );
             CREATE TABLE agent_inbox (
               id BIGSERIAL PRIMARY KEY, agent_id UUID NOT NULL, kind TEXT NOT NULL, event_seq BIGINT,
               payload JSONB NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'pending', attempts INTEGER DEFAULT 0,
               created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
             );",
        )
        .execute(&pool)
        .await?;
        let agent = Uuid::new_v4();
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_profiles (id) VALUES ($1)")
            .bind(agent)
            .execute(&pool)
            .await?;
        let workspace = std::env::temp_dir().join(format!("world-retry-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace)?;
        let workspace = workspace.to_string_lossy().to_string();
        let store = WorldStore::new(pool.clone());

        let failed_task = Uuid::new_v4();
        let task_session = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'failed', NULL, 'boom')")
            .bind(failed_task)
            .execute(&pool)
            .await?;
        insert_session(&pool, task_session, agent, "task", "failed", Some(failed_task), None, Some("saved"), Some(&workspace), None).await?;
        sqlx::query("UPDATE agent_sessions SET usage_baseline = '{\"input_tokens\":12}' WHERE id = $1")
            .bind(task_session)
            .execute(&pool)
            .await?;
        store.set_session_cli_id(task_session, "checkpoint").await?;
        let checkpoint: (Option<String>, serde_json::Value, String) = sqlx::query_as(
            "SELECT cli_session_id, usage_baseline, status FROM agent_sessions WHERE id = $1",
        )
        .bind(task_session)
        .fetch_one(&pool)
        .await?;
        assert_eq!(checkpoint, (Some("checkpoint".into()), serde_json::json!({ "input_tokens": 12 }), "failed".into()));
        let task_run = insert_run(&pool, task_session, agent, Some(failed_task), "task", "failed", Some(Uuid::new_v4())).await?;
        let retried = store.retry_run(task_run, user).await?;
        assert_eq!(retried.retry_mode, "resume");
        let retried_run = retried.run.unwrap();
        assert_eq!(retried_run.kind, "task");
        assert!(retried_run.fork_from.is_none());
        assert_eq!(retried_run.request, Some(serde_json::json!({ "retry": 1 })));
        let recovered: (String, Option<String>, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, s.status FROM agent_tasks t JOIN agent_sessions s ON s.task_id = t.id WHERE t.id = $1",
        )
        .bind(failed_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(recovered, ("in_progress".into(), None, "active".into()));

        let review_task = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let review = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'failed', NULL, 'review failed')")
            .bind(review_task)
            .execute(&pool)
            .await?;
        insert_session(&pool, parent, agent, "task", "failed", Some(review_task), None, Some("executor"), Some(&workspace), None).await?;
        insert_session(&pool, review, agent, "review", "failed", Some(review_task), Some(parent), None, Some(&workspace), None).await?;
        let review_run = insert_run(&pool, review, agent, Some(review_task), "review", "timed_out", None).await?;
        store.retry_run(review_run, user).await?;
        let review_state: (String, String, String) = sqlx::query_as(
            "SELECT r.status, p.status, t.status FROM agent_sessions r JOIN agent_sessions p ON p.id = r.parent_session_id JOIN agent_tasks t ON t.id = r.task_id WHERE r.id = $1",
        )
        .bind(review)
        .fetch_one(&pool)
        .await?;
        assert_eq!(review_state, ("active".into(), "completed".into(), "in_progress".into()));

        let manual_task = Uuid::new_v4();
        let manual_review = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'done', NULL, NULL)")
            .bind(manual_task)
            .execute(&pool)
            .await?;
        insert_session(&pool, manual_review, agent, "review", "failed", Some(manual_task), Some(parent), None, Some(&workspace), Some(serde_json::json!({ "manual": true }))).await?;
        let manual_run = insert_run(&pool, manual_review, agent, Some(manual_task), "review", "failed", None).await?;
        store.retry_run(manual_run, user).await?;
        let manual_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(manual_task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(manual_status, "done");

        let missing_task = Uuid::new_v4();
        let missing_session = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'failed', NULL, 'boom')")
            .bind(missing_task)
            .execute(&pool)
            .await?;
        insert_session(&pool, missing_session, agent, "task", "failed", Some(missing_task), None, None, None, None).await?;
        let missing_run = insert_run(&pool, missing_session, agent, Some(missing_task), "task", "failed", None).await?;
        assert!(matches!(store.retry_run(missing_run, user).await, Err(RetryRunError::Conflict(_))));
        sqlx::query("UPDATE agent_sessions SET working_dir = ' ' WHERE id = $1")
            .bind(missing_session)
            .execute(&pool)
            .await?;
        assert!(matches!(store.retry_run(missing_run, user).await, Err(RetryRunError::Conflict(_))));

        let worker_only_task = Uuid::new_v4();
        let worker_only_session = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'failed', NULL, 'boom')")
            .bind(worker_only_task)
            .execute(&pool)
            .await?;
        insert_session(
            &pool,
            worker_only_session,
            agent,
            "task",
            "failed",
            Some(worker_only_task),
            None,
            None,
            Some("/worker-only/not-mounted-in-api"),
            None,
        )
        .await?;
        let worker_only_run = insert_run(
            &pool,
            worker_only_session,
            agent,
            Some(worker_only_task),
            "task",
            "failed",
            None,
        )
        .await?;
        let visible = store.list_runs_for_session(worker_only_session).await?;
        assert!(visible[0].retryable);
        assert!(store.retry_run(worker_only_run, user).await?.run.is_some());

        let stale_session = Uuid::new_v4();
        insert_session(&pool, stale_session, agent, "aside", "failed", None, None, None, Some(&workspace), None).await?;
        let stale_run = insert_run(&pool, stale_session, agent, None, "aside", "failed", None).await?;
        insert_run(&pool, stale_session, agent, None, "aside", "completed", None).await?;
        assert!(matches!(store.retry_run(stale_run, user).await, Err(RetryRunError::Conflict(_))));

        let main_session = Uuid::new_v4();
        insert_session(&pool, main_session, agent, "main", "active", None, None, Some("main"), Some(&workspace), None).await?;
        let turn_run = insert_run(&pool, main_session, agent, None, "turn", "failed", None).await?;
        let turn_retry = store.retry_run(turn_run, user).await?;
        assert!(turn_retry.run.is_none());
        assert_eq!(turn_retry.retry_mode, "resume");
        let turn_counts: (i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND status = 'pending'),
               (SELECT COUNT(*) FROM session_runs WHERE session_id = $2 AND status = 'queued')",
        )
        .bind(agent)
        .bind(main_session)
        .fetch_one(&pool)
        .await?;
        assert_eq!(turn_counts, (1, 0));
        assert!(matches!(store.retry_run(turn_run, user).await, Err(RetryRunError::Conflict(_))));

        let shared_task = Uuid::new_v4();
        let completed_session = Uuid::new_v4();
        let failed_review = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks VALUES ($1, 'done', NULL, NULL)")
            .bind(shared_task)
            .execute(&pool)
            .await?;
        insert_session(&pool, completed_session, agent, "task", "completed", Some(shared_task), None, Some("task"), Some(&workspace), None).await?;
        insert_session(&pool, failed_review, agent, "review", "failed", Some(shared_task), Some(completed_session), Some("review"), Some(&workspace), Some(serde_json::json!({ "manual": true }))).await?;
        let followup_run = insert_run(&pool, completed_session, agent, Some(shared_task), "followup", "failed", None).await?;
        let concurrent_review = insert_run(&pool, failed_review, agent, Some(shared_task), "review", "failed", None).await?;
        let left = tokio::spawn({
            let store = store.clone();
            async move { store.retry_run(followup_run, user).await }
        });
        let right = tokio::spawn({
            let store = store.clone();
            async move { store.retry_run(concurrent_review, user).await }
        });
        let results = [left.await?, right.await?];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| matches!(result, Err(RetryRunError::Conflict(_)))).count(), 1);

        pool.close().await;
        std::fs::remove_dir_all(&workspace)?;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&admin).await?;
        Ok(())
    }

    #[tokio::test]
    async fn finished_reports_dedupe_against_the_latest_one() -> anyhow::Result<()> {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return Ok(());
        };
        let admin = PgPoolOptions::new().max_connections(1).connect(&database_url).await?;
        let schema = format!("world_finished_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await?;
        let options = PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new().max_connections(2).connect_with(options).await?;
        sqlx::raw_sql(
            "CREATE TABLE world_events (
               seq BIGSERIAL PRIMARY KEY, type TEXT NOT NULL, agent_id UUID, session_id UUID, run_id UUID,
               task_id UUID, actor_user_id UUID, payload JSONB NOT NULL DEFAULT '{}', created_at TIMESTAMPTZ DEFAULT NOW()
             );
             CREATE TABLE agent_tasks (id UUID PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE agent_sessions (id UUID PRIMARY KEY, task_id UUID);",
        )
        .execute(&pool)
        .await?;
        let store = WorldStore::new(pool.clone());
        let session = Uuid::new_v4();
        let quest = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks (id, status) VALUES ($1, 'failed')").bind(quest).execute(&pool).await?;
        sqlx::query("INSERT INTO agent_sessions (id, task_id) VALUES ($1, $2)").bind(session).bind(quest).execute(&pool).await?;
        let report = serde_json::json!({ "summary": "shipped the fix" });
        let insert = |payload: serde_json::Value| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO world_events (type, session_id, payload) VALUES ('task.finished', $1, $2)")
                    .bind(session)
                    .bind(payload)
                    .execute(&pool)
                    .await
            }
        };
        assert!(!store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), None).await?);
        insert(serde_json::json!({ "report": report, "pr_url": "https://example/pr/1", "error": null })).await?;
        assert!(store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), None).await?);
        assert!(!store.finished_report_unchanged(session, &report, Some("https://example/pr/2"), None).await?);
        assert!(!store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), Some("boom")).await?);
        assert!(!store.finished_report_unchanged(session, &serde_json::json!({ "summary": "new work" }), Some("https://example/pr/1"), None).await?);
        insert(serde_json::json!({ "report": report, "pr_url": "https://example/pr/1", "error": "boom" })).await?;
        assert!(store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), Some("boom")).await?);
        assert!(!store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), None).await?);
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert!(
            !store.finished_report_unchanged(session, &report, Some("https://example/pr/1"), Some("boom")).await?,
            "a failure is announced again once the quest was reopened"
        );
        let other_session = Uuid::new_v4();
        assert!(!store.finished_report_unchanged(other_session, &report, Some("https://example/pr/1"), Some("boom")).await?);
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&admin).await?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_cost_totals_follow_every_run_change() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut agents = Vec::new();
        for name in ["a", "b"] {
            agents.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, $2, 'claude', 'sonnet') RETURNING id")
                    .bind(format!("usage-{name}-{tag}"))
                    .bind(name)
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let session: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'sonnet') RETURNING id")
            .bind(agents[0])
            .fetch_one(&pool)
            .await?;
        let mut runs = Vec::new();
        for i in 0..6 {
            runs.push(
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, cost_usd, input_tokens, cached_input_tokens, output_tokens)
                     VALUES ($1, $2, 'turn', 'completed', 'x', $3, $4, 5, 7) RETURNING id",
                )
                .bind(session)
                .bind(agents[0])
                .bind(0.1 * f64::from(i) + 0.013)
                .bind(i64::from(i) * 100)
                .fetch_one(&pool)
                .await?,
            );
        }
        sqlx::query("UPDATE session_runs SET cost_usd = cost_usd + 0.25, output_tokens = output_tokens + 11 WHERE id = ANY($1)")
            .bind(&runs[..3])
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE session_runs SET agent_id = $2 WHERE id = $1").bind(runs[3]).bind(agents[1]).execute(&pool).await?;
        sqlx::query("DELETE FROM session_runs WHERE id = $1").bind(runs[4]).execute(&pool).await?;
        let expected: Vec<(Uuid, f64, i64)> = sqlx::query_as(
            "SELECT agent_id, SUM(cost_usd)::float8, SUM(input_tokens + cached_input_tokens + output_tokens)::int8
             FROM session_runs WHERE agent_id = ANY($1) GROUP BY agent_id",
        )
        .bind(&agents)
        .fetch_all(&pool)
        .await?;
        let totals = store.agent_costs(&agents).await?;
        assert_eq!(totals.len(), expected.len());
        for (agent, cost, tokens) in expected {
            let (got_cost, got_tokens) = totals[&agent];
            assert!((got_cost - cost).abs() < 1e-9, "{got_cost} vs {cost}");
            assert_eq!(got_tokens, tokens);
            assert_eq!(store.agent_cost(agent).await?, (got_cost, got_tokens));
        }
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(&agents).execute(&pool).await?;
        assert!(store.agent_costs(&agents).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn compaction_that_cannot_help_waits_for_growth() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'c', 'claude', 'sonnet') RETURNING id")
            .bind(format!("compact-{tag}"))
            .fetch_one(&pool)
            .await?;
        let session: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'sonnet') RETURNING id")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let run = |prompt: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
                     VALUES ($1, $2, 'turn', 'completed', $3, clock_timestamp()) RETURNING id",
                )
                .bind(session)
                .bind(agent)
                .bind(prompt)
                .fetch_one(&pool)
                .await
            }
        };
        let limit = 120_000;
        let first = run("hello").await?;
        assert!(store.should_compact(session, first, 130_000, limit).await?);
        run(super::COMPACT_PROMPT).await?;
        let after = run("next").await?;
        assert!(!store.should_compact(session, after, 133_000, limit).await?);
        let later = run("later").await?;
        assert!(!store.should_compact(session, later, 140_000, limit).await?);
        assert!(store.should_compact(session, later, 170_000, limit).await?);
        run(super::COMPACT_PROMPT).await?;
        let fixed = run("after a good compaction").await?;
        assert!(!store.should_compact(session, fixed, 40_000, limit).await?);
        let grown = run("grown").await?;
        assert!(store.should_compact(session, grown, 121_000, limit).await?);
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn auto_context_limit_saves_queues_once_and_falls_back_to_reset() -> anyhow::Result<()> {
        use crate::context::{
            AutoAction, AutoUpdate, AUTO_ACTION_INVALID, AUTO_FELL_BACK, AUTO_NOTHING_SENT, AUTO_TOKENS_INVALID,
            AUTO_TOKENS_OVER_MAX, COMPACT_INBOX_KIND,
        };
        use serde_json::{json, Value};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut people = Vec::new();
        for who in ["owner", "other"] {
            people.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email) VALUES ($1) RETURNING id")
                    .bind(format!("auto-{who}-{tag}@example.test"))
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let (owner, other) = (people[0], people[1]);
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'a', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("auto-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 140000) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let events = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>(
                    "SELECT payload FROM world_events WHERE agent_id = $1 AND type = 'agent.context' ORDER BY seq",
                )
                .bind(agent_id)
                .fetch_all(&pool)
                .await
            }
        };
        let run = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
                     VALUES ($1, $2, 'turn', 'completed', 'hello', clock_timestamp()) RETURNING id",
                )
                .bind(session)
                .bind(agent_id)
                .fetch_one(&pool)
                .await
            }
        };
        let inbox = |kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>(
                    "SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending' ORDER BY id",
                )
                .bind(agent_id)
                .bind(kind)
                .fetch_all(&pool)
                .await
            }
        };

        let view = store.context_view(&agent).await?;
        assert_eq!(view.auto_context_tokens, None);
        assert_eq!(view.auto_context_action, "compact");
        assert_eq!(view.max_tokens, Some(200_000));

        let set = json!({ "auto_context_tokens": 150_000 });
        assert!(matches!(store.update_auto_context(&agent, other, &set).await?, AutoUpdate::NotOwner));
        for (body, reason) in [
            (json!({}), AUTO_NOTHING_SENT),
            (json!({ "auto_context_tokens": 9_999 }), AUTO_TOKENS_INVALID),
            (json!({ "auto_context_tokens": 200_000 }), AUTO_TOKENS_OVER_MAX),
            (json!({ "auto_context_action": null }), AUTO_ACTION_INVALID),
            (json!({ "auto_context_action": "clear" }), AUTO_ACTION_INVALID),
        ] {
            match store.update_auto_context(&agent, owner, &body).await? {
                AutoUpdate::Invalid(got) => assert_eq!(got, reason, "{body}"),
                other => panic!("{body} should be refused, got {other:?}"),
            }
        }
        assert!(events().await?.is_empty());
        assert_eq!(store.auto_context(agent_id).await?.tokens, None);

        match store.update_auto_context(&agent, owner, &set).await? {
            AutoUpdate::Saved { view, changed } => {
                assert!(changed);
                assert_eq!(view.auto_context_tokens, Some(150_000));
                assert_eq!(view.auto_context_action, "compact");
            }
            other => panic!("expected a save, got {other:?}"),
        }
        let announced = events().await?;
        assert_eq!(announced.len(), 1);
        assert_eq!(announced[0]["auto_context_tokens"], 150_000);
        assert_eq!(announced[0]["quiet"], true);
        assert!(matches!(
            store.update_auto_context(&agent, owner, &set).await?,
            AutoUpdate::Saved { changed: false, .. }
        ));
        assert_eq!(events().await?.len(), 1);

        let refused = sqlx::query("UPDATE agent_profiles SET auto_context_tokens = 9999 WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await;
        assert!(refused.is_err(), "the database must refuse limits under 10000");

        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None);
        sqlx::query("UPDATE agent_sessions SET context_tokens = 160000 WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, Some(AutoAction::Compact));
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None);
        let compacts = inbox(COMPACT_INBOX_KIND).await?;
        assert_eq!(compacts.len(), 1, "one crossing queues one compaction");
        assert_eq!(compacts[0]["origin"], "auto");
        assert_eq!(compacts[0]["requested_action"], "compact");
        assert_eq!(compacts[0]["action"], "compact");
        assert_eq!(compacts[0]["fallback_reason"], Value::Null);
        let latest = events().await?.pop().expect("event");
        assert_eq!(latest["pending"], "compact");
        assert_eq!(latest["auto"]["action"], "compact");
        let floors: (Option<i32>, Option<i32>) =
            sqlx::query_as("SELECT compact_floor, auto_compact_floor FROM agent_sessions WHERE id = $1")
                .bind(session)
                .fetch_one(&pool)
                .await?;
        assert_eq!(floors.0, None, "the per-agent limit never moves the global compaction floor");

        let auto_floor = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<i32>>("SELECT auto_compact_floor FROM agent_sessions WHERE id = $1")
                    .bind(session)
                    .fetch_one(&pool)
                    .await
            }
        };
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
             VALUES ($1, $2, 'turn', 'completed', $3, clock_timestamp())",
        )
        .bind(session)
        .bind(agent_id)
        .bind(super::COMPACT_PROMPT)
        .execute(&pool)
        .await?;
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None, "a stuck compaction waits");
        assert_eq!(auto_floor().await?, Some(160_000));
        sqlx::query("UPDATE agent_sessions SET context_tokens = 140000 WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None);
        assert_eq!(auto_floor().await?, None, "dropping under the limit clears the floor");
        sqlx::query("UPDATE agent_sessions SET context_tokens = 150000 WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(
            store.auto_context_cleanup(agent_id, run().await?).await?,
            Some(AutoAction::Compact),
            "the next crossing fires at the configured limit again"
        );
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("UPDATE agent_sessions SET auto_compact_floor = 160000 WHERE id = $1").bind(session).execute(&pool).await?;
        assert!(matches!(
            store.update_auto_context(&agent, owner, &json!({ "auto_context_tokens": 155_000 })).await?,
            AutoUpdate::Saved { changed: true, .. }
        ));
        assert_eq!(auto_floor().await?, None, "a new limit clears the floor");
        assert!(matches!(
            store.update_auto_context(&agent, owner, &set).await?,
            AutoUpdate::Saved { changed: true, .. }
        ));
        sqlx::query("UPDATE agent_sessions SET context_tokens = 160000 WHERE id = $1").bind(session).execute(&pool).await?;

        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("UPDATE agent_profiles SET cli = 'cursor', model = 'composer-2.5' WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("UPDATE agent_sessions SET backend = 'cursor', model = 'composer-2.5' WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, Some(AutoAction::Reset));
        let resets = inbox(super::RESET_INBOX_KIND).await?;
        assert_eq!(resets.len(), 1);
        assert_eq!(resets[0]["requested_action"], "compact");
        assert_eq!(resets[0]["action"], "reset");
        assert_eq!(resets[0]["fallback_reason"], AUTO_FELL_BACK);
        let latest = events().await?.pop().expect("event");
        assert_eq!(latest["pending"], "reset");
        assert_eq!(latest["auto"]["fallback_reason"], AUTO_FELL_BACK);
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None);
        assert_eq!(inbox(super::RESET_INBOX_KIND).await?.len(), 1);

        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'test' WHERE id = $1").bind(agent_id).execute(&pool).await?;
        assert_eq!(store.auto_context_cleanup(agent_id, run().await?).await?, None);

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(&people).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn oversized_main_sessions_start_quests_fresh_once() -> anyhow::Result<()> {
        use crate::context::{ForkChoice, COMPACT_INBOX_KIND};
        use crate::run_causes::{diagnose, RunEnd, FRESH_RETRY_CAUSE};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("fork-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Fork', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("fork-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 1000) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("fork-{tag}-notes"), "notes", "Remember the blue widget.")
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let main = store.get_session(main_id).await?.expect("main");
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fork));
        use crate::context::QuestContext;
        assert_eq!(agent.quest_context, "lean", "new and existing agents start lean");
        let level = QuestContext::resolve(None, &agent.quest_context);
        assert_eq!(store.fork_choice(&agent, &main, level).await?, ForkChoice::Lean { used: 1000, level: QuestContext::Lean });
        assert_eq!(store.fork_choice(&agent, &main, QuestContext::Auto).await?, ForkChoice::Fork);
        assert_eq!(
            store.fork_choice(&agent, &main, QuestContext::resolve(Some("full"), &agent.quest_context)).await?,
            ForkChoice::Fork,
            "a quest override wins over the agent setting"
        );
        let updated = store
            .update_agent(agent_id, &AgentUpdate { quest_context: Some("auto".into()), ..Default::default() })
            .await?
            .expect("agent");
        assert_eq!(updated.quest_context, "auto");
        let refused = sqlx::query("UPDATE agent_profiles SET quest_context = 'huge' WHERE id = $1").bind(agent_id).execute(&pool).await;
        assert!(refused.is_err(), "the check constraint refuses unknown levels");

        sqlx::query("UPDATE agent_sessions SET context_tokens = 120000 WHERE id = $1").bind(main_id).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        assert_eq!(
            store.fork_choice(&agent, &main, QuestContext::Auto).await?,
            ForkChoice::Lean { used: 120_000, level: QuestContext::Auto },
            "auto stops forking past half of the 200k window"
        );

        sqlx::query("UPDATE agent_sessions SET context_tokens = 150000 WHERE id = $1").bind(main_id).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        match store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await? {
            ForkChoice::Fresh { used, limit, compacting } => {
                assert_eq!((used, limit), (150_000, 140_000));
                assert!(compacting);
            }
            other => panic!("an oversized main session must not fork: {other:?}"),
        }
        let compacts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending'",
        )
        .bind(agent_id)
        .bind(COMPACT_INBOX_KIND)
        .fetch_one(&pool)
        .await?;
        assert_eq!(compacts, 1);

        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'fork', 'in_progress') RETURNING id",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, cli_session_id)
             VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .bind(task_id)
        .bind(format!("fork-cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        let failed: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, error)
             VALUES ($1, $2, $3, 'task', 'failed', $4, 'Build the blue widget brief.', 'Prompt is too long') RETURNING id",
        )
        .bind(task_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        let end = RunEnd { kind: "task", error: Some("Prompt is too long"), output: "", turns: 0, timed_out: false };
        let diagnosis = diagnose(&end);
        let retry = store.fresh_retry(failed, &diagnosis).await?.expect("one fresh retry");
        let (fork_from, prompt, cause, status): (Option<Uuid>, String, Option<String>, String) =
            sqlx::query_as("SELECT fork_from, prompt, retry_cause, status FROM session_runs WHERE id = $1")
                .bind(retry)
                .fetch_one(&pool)
                .await?;
        assert_eq!(fork_from, None);
        assert_eq!(cause.as_deref(), Some(FRESH_RETRY_CAUSE));
        assert_eq!(status, "queued");
        assert!(prompt.contains("Build the blue widget brief."));
        assert!(prompt.contains("Remember the blue widget."));
        let cli: Option<String> = sqlx::query_scalar("SELECT cli_session_id FROM agent_sessions WHERE id = $1")
            .bind(task_session)
            .fetch_one(&pool)
            .await?;
        assert_eq!(cli, None);

        sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Prompt is too long' WHERE id = $1")
            .bind(retry)
            .execute(&pool)
            .await?;
        assert_eq!(store.fresh_retry(retry, &diagnosis).await?, None);
        assert_eq!(store.fresh_retry(failed, &diagnosis).await?, None);
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(runs, 2);

        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(retry).execute(&pool).await?;
        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2, context_tokens = 190000 WHERE id = $1")
            .bind(task_session)
            .bind(format!("fresh-cli-{tag}"))
            .execute(&pool)
            .await?;
        let followup: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, error)
             VALUES ($1, $2, $3, 'followup', 'failed', '[follow-up from owner]\nFix the red review comment.', 'Prompt is too long')
             RETURNING id",
        )
        .bind(task_session)
        .bind(agent_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        let end = RunEnd { kind: "followup", error: Some("Prompt is too long"), output: "", turns: 0, timed_out: false };
        let diagnosis = diagnose(&end);
        let resumed = store.auto_retry(followup, &diagnosis).await?.expect("one fresh resume");
        let (kind, fork_from, prompt, cause): (String, Option<Uuid>, String, Option<String>) =
            sqlx::query_as("SELECT kind, fork_from, prompt, retry_cause FROM session_runs WHERE id = $1")
                .bind(resumed)
                .fetch_one(&pool)
                .await?;
        assert_eq!((kind.as_str(), fork_from, cause.as_deref()), ("task", None, Some(FRESH_RETRY_CAUSE)));
        assert!(prompt.contains("git log"));
        assert!(prompt.contains("Remember the blue widget."));
        assert!(prompt.contains("Build the blue widget brief."));
        assert!(prompt.contains(crate::context::LATEST_FOLLOWUP));
        assert!(prompt.contains("Fix the red review comment."));
        let cli: Option<String> = sqlx::query_scalar("SELECT cli_session_id FROM agent_sessions WHERE id = $1")
            .bind(task_session)
            .fetch_one(&pool)
            .await?;
        assert_eq!(cli, None);
        sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Prompt is too long' WHERE id = $1")
            .bind(resumed)
            .execute(&pool)
            .await?;
        assert_eq!(store.auto_retry(resumed, &diagnosis).await?, None);
        assert_eq!(store.auto_retry(followup, &diagnosis).await?, None);
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(runs, 4);

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task_id).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("fork-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn auto_quests_that_fork_and_fail_turn_one_retry_fresh_once() -> anyhow::Result<()> {
        use crate::context::{ForkChoice, QuestContext};
        use crate::run_causes::{diagnose, RunEnd, FRESH_RETRY_CAUSE};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("auto-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, quest_context)
             VALUES ($1, 'Auto', 'claude', 'sonnet', $2, 'auto') RETURNING id",
        )
        .bind(format!("auto-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 60000) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("auto-{tag}-notes"), "notes", "Remember the green gear.")
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let main = store.get_session(main_id).await?.expect("main");
        let level = QuestContext::resolve(None, &agent.quest_context);
        assert_eq!(level, QuestContext::Auto);
        assert_eq!(store.fork_choice(&agent, &main, level).await?, ForkChoice::Fork, "auto forks under half the window");

        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'auto', 'in_progress') RETURNING id",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, cli_session_id)
             VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .bind(task_id)
        .bind(format!("auto-cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        let end = RunEnd { kind: "task", error: Some("Prompt is too long"), output: "", turns: 1, timed_out: false };
        let diagnosis = diagnose(&end);
        assert!(diagnosis.fork_too_long());
        let failed: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, error, cause, diagnosis)
             VALUES ($1, $2, $3, 'task', 'failed', $4, 'Fit the green gear brief.', 'Prompt is too long', 'cli_error', $5)
             RETURNING id",
        )
        .bind(task_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .bind(serde_json::to_value(&diagnosis)?)
        .fetch_one(&pool)
        .await?;

        let retry = store.fresh_retry(failed, &diagnosis).await?.expect("one fresh retry");
        let (fork_from, prompt, cause): (Option<Uuid>, String, Option<String>) =
            sqlx::query_as("SELECT fork_from, prompt, retry_cause FROM session_runs WHERE id = $1")
                .bind(retry)
                .fetch_one(&pool)
                .await?;
        assert_eq!(fork_from, None, "the retry never forks the main session again");
        assert_eq!(cause.as_deref(), Some(FRESH_RETRY_CAUSE));
        assert!(prompt.contains("Fit the green gear brief."));
        assert!(prompt.contains("Remember the green gear."));
        assert_eq!(store.fresh_retry(failed, &diagnosis).await?, None);
        sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Prompt is too long' WHERE id = $1")
            .bind(retry)
            .execute(&pool)
            .await?;
        assert_eq!(store.fresh_retry(retry, &diagnosis).await?, None, "a fresh retry is never retried again");

        assert!(
            !matches!(store.fork_choice(&agent, &main, level).await?, ForkChoice::Fork),
            "after a failed fork, the next auto quest starts without the main conversation"
        );
        assert!(matches!(store.fork_choice(&agent, &main, QuestContext::Lean).await?, ForkChoice::Lean { .. }));

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task_id).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("auto-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn forks_count_their_overhead_learn_from_failures_and_survive_resets() -> anyhow::Result<()> {
        use crate::context::ForkChoice;
        use crate::run_causes::FRESH_RETRY_CAUSE;
        use crate::world::COMPACT_PROMPT;
        use serde_json::json;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("overhead-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Tara', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("overhead-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 105000) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("overhead-{tag}-notes"), "notes", "Keep the green ledger.")
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let clear_compacts = || async {
            sqlx::query("DELETE FROM agent_inbox WHERE agent_id = $1").bind(agent_id).execute(&pool).await
        };
        let queued_compacts = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'context.compact'")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
        };

        let main = store.get_session(main_id).await?.expect("main");
        match store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await? {
            ForkChoice::Fresh { used, limit, compacting } => {
                assert_eq!((used, limit), (105_000, 140_000));
                assert!(!compacting, "only the fork overhead crosses the line, so the main session is not compacted");
            }
            other => panic!("a fork adds its own overhead, so 105k of main context must start fresh: {other:?}"),
        }
        assert_eq!(queued_compacts().await?, 0);

        sqlx::query("UPDATE agent_sessions SET context_tokens = 150000 WHERE id = $1").bind(main_id).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fresh { compacting: true, .. }));
        assert_eq!(queued_compacts().await?, 1);
        clear_compacts().await?;
        sqlx::query("UPDATE agent_sessions SET compact_floor = 150000 WHERE id = $1").bind(main_id).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fresh { compacting: false, .. }));
        assert_eq!(queued_compacts().await?, 0);
        sqlx::query("UPDATE agent_sessions SET compact_floor = NULL WHERE id = $1").bind(main_id).execute(&pool).await?;
        clear_compacts().await?;
        sqlx::query("UPDATE agent_sessions SET context_tokens = 60000 WHERE id = $1").bind(main_id).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fork));

        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'ledger', 'in_progress') RETURNING id",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, cli_session_id)
             VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .bind(task_id)
        .bind(format!("fork-cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        let failed: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, error, started_at, finished_at)
             VALUES ($1, $2, $3, 'task', 'failed', $4, 'Balance the green ledger.', 'Prompt is too long', NOW(), NOW()) RETURNING id",
        )
        .bind(task_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_run_events (run_id, kind, body) VALUES ($1, 'text', $2), ($1, 'text', 'Prompt is too long')",
        )
        .bind(failed)
        .bind(format!("Session started: fork-cli-{tag}"))
        .execute(&pool)
        .await?;
        let diagnosis = store.record_run_end(failed, "task", Some("Prompt is too long"), "", false).await?;
        assert_eq!(diagnosis.turns, 1);
        assert!(diagnosis.fork_too_long());
        let retry = store.auto_retry(failed, &diagnosis).await?.expect("one fresh retry for a runner run");
        let cause: Option<String> = sqlx::query_scalar("SELECT retry_cause FROM session_runs WHERE id = $1")
            .bind(retry)
            .fetch_one(&pool)
            .await?;
        assert_eq!(cause.as_deref(), Some(FRESH_RETRY_CAUSE));
        let retrying: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE type = 'run.retrying' AND task_id = $1 AND payload->>'fresh' = 'true'",
        )
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(retrying, 1);
        assert_eq!(store.auto_retry(failed, &diagnosis).await?, None);
        sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Prompt is too long' WHERE id = $1")
            .bind(retry)
            .execute(&pool)
            .await?;
        let again = store.record_run_end(retry, "task", Some("Prompt is too long"), "", false).await?;
        assert_eq!(store.auto_retry(retry, &again).await?, None);
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(runs, 2);

        let main = store.get_session(main_id).await?.expect("main");
        assert!(store.fork_failed_since_compact(&main).await?);
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fresh { used: 60_000, .. }));
        clear_compacts().await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, error, created_at)
             VALUES ($1, $2, 'turn', 'failed', $3, 'Prompt is too long', NOW() + INTERVAL '500 milliseconds')",
        )
        .bind(main_id)
        .bind(agent_id)
        .bind(COMPACT_PROMPT)
        .execute(&pool)
        .await?;
        assert!(store.fork_failed_since_compact(&main).await?, "a failed /compact keeps the failed fork in mind");
        clear_compacts().await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
             VALUES ($1, $2, 'turn', 'completed', $3, NOW() + INTERVAL '1 second')",
        )
        .bind(main_id)
        .bind(agent_id)
        .bind(COMPACT_PROMPT)
        .execute(&pool)
        .await?;
        assert!(!store.fork_failed_since_compact(&main).await?);
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fork));
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, cause, diagnosis, created_at)
             VALUES ($1, $2, $3, 'task', 'failed', $4, 'Grow the green ledger.', 'cli_error',
                     '{\"failure\": \"context_too_long\", \"turns\": 5}'::jsonb, NOW() + INTERVAL '2 seconds')",
        )
        .bind(task_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .execute(&pool)
        .await?;
        assert!(!store.fork_failed_since_compact(&main).await?, "a quest that outgrew the window later says nothing about the fork");
        assert!(matches!(store.fork_choice(&agent, &main, crate::context::QuestContext::Full).await?, ForkChoice::Fork));

        let queued_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, usage_baseline)
             VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, '{\"cost_usd\": 4}'::jsonb) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        let queued: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, not_before)
             VALUES ($1, $2, $3, 'task', 'queued', $4, 'Audit the green ledger.', NOW() + INTERVAL '1 day') RETURNING id",
        )
        .bind(queued_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        let codex_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id)
             VALUES ($1, 'task', 'codex', 'gpt-5.5', $2, $3) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        let codex_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, not_before)
             VALUES ($1, $2, $3, 'task', 'queued', $4, 'Port the green ledger.', NOW() + INTERVAL '1 day') RETURNING id",
        )
        .bind(codex_session)
        .bind(agent_id)
        .bind(task_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        let aside_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id)
             VALUES ($1, 'aside', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(agent_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        let aside_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, fork_from, prompt, not_before)
             VALUES ($1, $2, 'aside', 'queued', $3, 'Which ledger is green?', NOW() + INTERVAL '1 day') RETURNING id",
        )
        .bind(aside_session)
        .bind(agent_id)
        .bind(main_id)
        .fetch_one(&pool)
        .await?;
        let fresh = store.restart_main_session(agent_id, "claude", "sonnet", "Keep the green ledger.").await?;
        assert_ne!(fresh.id, main_id);
        let (fork_from, prompt): (Option<Uuid>, String) =
            sqlx::query_as("SELECT fork_from, prompt FROM session_runs WHERE id = $1").bind(queued).fetch_one(&pool).await?;
        assert_eq!(fork_from, None);
        assert!(prompt.starts_with("[fresh session] You are Tara."));
        assert!(prompt.contains("Keep the green ledger."));
        assert!(prompt.ends_with("Audit the green ledger."));
        assert!(prompt.contains("fresh claude session"));
        let (fork_from, prompt): (Option<Uuid>, String) =
            sqlx::query_as("SELECT fork_from, prompt FROM session_runs WHERE id = $1").bind(codex_run).fetch_one(&pool).await?;
        assert_eq!(fork_from, None);
        assert!(prompt.contains("fresh codex session"), "the intro names the quest's own CLI");
        assert!(prompt.ends_with("Port the green ledger."));
        let (fork_from, prompt): (Option<Uuid>, String) =
            sqlx::query_as("SELECT fork_from, prompt FROM session_runs WHERE id = $1").bind(aside_run).fetch_one(&pool).await?;
        assert_eq!(fork_from, Some(main_id), "asides keep forking the old main conversation");
        assert_eq!(prompt, "Which ledger is green?");
        let baseline: Value = sqlx::query_scalar("SELECT usage_baseline FROM agent_sessions WHERE id = $1")
            .bind(queued_session)
            .fetch_one(&pool)
            .await?;
        assert_eq!(baseline, json!({}));

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task_id).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("overhead-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn message_pages_and_channel_scope_match_the_full_queries() -> anyhow::Result<()> {
        use super::WorldEvent;
        use serde_json::json;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("pages-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let mut agents = Vec::new();
        for name in ["a", "b"] {
            agents.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id")
                    .bind(format!("pages-{name}-{tag}"))
                    .bind(name)
                    .bind(owner)
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let (a, b) = (agents[0], agents[1]);
        let rows: [(Uuid, &str, Option<Uuid>, i32); 9] = [
            (a, "user", None, 1),
            (a, "agent", Some(a), 2),
            (b, "agent", Some(a), 3),
            (b, "agent", Some(b), 4),
            (a, "user", None, 5),
            (b, "user", None, 6),
            (b, "agent", Some(a), 7),
            (a, "agent", Some(b), 8),
            (a, "user", None, 9),
        ];
        for (agent_id, kind, author, minute) in rows {
            sqlx::query(
                "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, author_agent_id, body, created_at)
                 VALUES ($1, $2, $3, $4, $5, NOW() - make_interval(mins => 100 - $6))",
            )
            .bind(agent_id)
            .bind(kind)
            .bind((kind == "user").then_some(owner))
            .bind(author)
            .bind(format!("m{minute}"))
            .bind(minute)
            .execute(&pool)
            .await?;
        }
        let mid: DateTime<Utc> = sqlx::query_scalar("SELECT created_at FROM agent_messages WHERE agent_id = $1 AND body = 'm5'")
            .bind(a)
            .fetch_one(&pool)
            .await?;
        for (limit, after, before) in [(3, None, None), (10, None, None), (2, None, Some(mid)), (10, Some(mid), None), (2, Some(mid), None)] {
            let order = if after.is_some() { "ASC" } else { "DESC" };
            let mut expected: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT m.body FROM agent_messages m
                 WHERE (m.agent_id = $1 OR (m.author_kind = 'agent' AND m.author_agent_id = $1 AND m.agent_id <> $1))
                   AND ($3::timestamptz IS NULL OR m.created_at >= $3 OR m.edited_at >= $3)
                   AND ($4::timestamptz IS NULL OR m.created_at < $4)
                 ORDER BY m.created_at {order} LIMIT $2"
            ))
            .bind(a)
            .bind(limit)
            .bind(after)
            .bind(before)
            .fetch_all(&pool)
            .await?;
            if after.is_none() {
                expected.reverse();
            }
            let page = store.list_messages_page(a, owner, limit, after, before).await?;
            assert_eq!(page.iter().map(|m| m.body.clone()).collect::<Vec<_>>(), expected, "limit {limit} after {after:?} before {before:?}");
        }
        assert_eq!(
            store.list_messages(a, owner, 10).await?.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            ["m1", "m2", "m3", "m5", "m7", "m8", "m9"]
        );

        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(a).fetch_one(&pool).await?;
        let mut channels = Vec::new();
        for kind in ["channel", "dm", "dm"] {
            channels.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO channels (name, kind, org_id) VALUES ($1, $2, $3) RETURNING id")
                    .bind(format!("{kind}-{}", Uuid::new_v4().simple()))
                    .bind(kind)
                    .bind(org)
                    .fetch_one(&pool)
                    .await?,
            );
        }
        sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, 'user', $2)")
            .bind(channels[1])
            .bind(owner)
            .execute(&pool)
            .await?;
        let event = |channel: Option<Uuid>| WorldEvent {
            seq: 0,
            event_type: "channel.message".into(),
            agent_id: None,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: None,
            payload: channel.map_or(json!({}), |id| json!({ "channel_id": id })),
            created_at: Utc::now(),
            org_id: None,
        };
        let events: Vec<WorldEvent> = vec![event(Some(channels[0])), event(Some(channels[1])), event(Some(channels[2])), event(None)];
        let full = store.hidden_channels(Some(owner), None).await?;
        let scoped = store.hidden_channels_among(Some(owner), None, &events).await?;
        assert_eq!(scoped, [channels[2]].into_iter().collect());
        assert!(events.iter().all(|e| e.in_channel(&full) == e.in_channel(&scoped)));
        assert!(store.hidden_channels_among(Some(owner), None, &[event(None)]).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn owner_asked_now_needs_the_owner_speaking() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("owner-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Ask', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("ask-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        for kind in ["slack.notification", "task.assigned"] {
            let seq: i64 = sqlx::query_scalar("INSERT INTO world_events (type, agent_id, actor_user_id, payload) VALUES ($1, $2, $3, '{}') RETURNING seq")
                .bind(kind)
                .bind(agent)
                .bind(owner)
                .fetch_one(&pool)
                .await?;
            sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload, status) VALUES ($1, $2, $3, '{}', 'processing')")
                .bind(agent)
                .bind(kind)
                .bind(seq)
                .execute(&pool)
                .await?;
        }
        let stamped: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_inbox WHERE agent_id = $1 AND payload->>'from_owner' = 'true'")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(stamped, 2);
        assert!(!store.owner_asked_now(agent).await?);
        let seq: i64 = sqlx::query_scalar("INSERT INTO world_events (type, agent_id, actor_user_id, payload) VALUES ('message.created', $1, $2, '{}') RETURNING seq")
            .bind(agent)
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload, status) VALUES ($1, 'message', $2, '{}', 'processing')")
            .bind(agent)
            .bind(seq)
            .execute(&pool)
            .await?;
        assert!(!store.owner_asked_now(agent).await?, "outside content in the same turn keeps the owner gate shut");
        assert!(store.outside_led_now(agent).await?);
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1 AND kind = 'slack.notification'").bind(agent).execute(&pool).await?;
        assert!(store.owner_asked_now(agent).await?);
        assert!(!store.outside_led_now(agent).await?);
        sqlx::query("UPDATE agent_inbox SET payload = payload || '{\"shell_share\": true}' WHERE agent_id = $1 AND kind = 'message'").bind(agent).execute(&pool).await?;
        assert!(!store.owner_asked_now(agent).await?, "command output shared back is not the owner asking");

        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        let pending = |kind: &'static str, by_owner: bool| {
            let pool = pool.clone();
            async move {
                let seq: i64 = sqlx::query_scalar("INSERT INTO world_events (type, agent_id, actor_user_id, payload) VALUES ($1, $2, $3, '{}') RETURNING seq")
                    .bind(kind)
                    .bind(agent)
                    .bind(by_owner.then_some(owner))
                    .fetch_one(&pool)
                    .await?;
                sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload) VALUES ($1, $2, $3, '{}')").bind(agent).bind(kind).bind(seq).execute(&pool).await?;
                anyhow::Ok(())
            }
        };
        pending("feed.item", false).await?;
        pending("task.finished", false).await?;
        pending("message", true).await?;
        pending("github.comment", true).await?;
        let first: Vec<String> = store.take_pending_inbox(agent, 20).await?.into_iter().map(|i| i.kind).collect();
        assert_eq!(first, vec!["task.finished", "message"], "outside content waits while the owner is speaking");
        assert!(store.owner_asked_now(agent).await?);
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1 AND status = 'processing'").bind(agent).execute(&pool).await?;
        let second: Vec<String> = store.take_pending_inbox(agent, 20).await?.into_iter().map(|i| i.kind).collect();
        assert_eq!(second, vec!["feed.item", "github.comment"], "and gets a turn of its own right after");
        assert!(store.outside_led_now(agent).await? && !store.owner_asked_now(agent).await?);
        Ok(())
    }

    #[tokio::test]
    async fn tidy_drops_only_empty_failed_turns_and_runs_page() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Fea', 'codex', 'gpt') RETURNING id")
            .bind(format!("fea-{tag}"))
            .fetch_one(&pool)
            .await?;
        let mode: String = sqlx::query_scalar("SELECT approval_mode FROM agent_profiles WHERE id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(mode, "auto");
        let main: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'codex', 'gpt', 'active') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        for i in 0..70 {
            sqlx::query(
                "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, error, created_at)
                 VALUES ($1, $2, 'turn', 'failed', '[event] run.failed', 'Failed to spawn codex', NOW() - make_interval(mins => 200 - $3))",
            )
            .bind(main)
            .bind(agent)
            .bind(i)
            .execute(&pool)
            .await?;
        }
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, error, input_tokens, created_at)
             VALUES ($1, $2, 'turn', 'failed', 'real try', 'boom', 500, NOW() - INTERVAL '100 minutes')",
        )
        .bind(main)
        .bind(agent)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output, created_at)
             VALUES ($1, $2, 'turn', 'completed', 'hello', 'hi', NOW() - INTERVAL '1 minute')",
        )
        .bind(main)
        .bind(agent)
        .execute(&pool)
        .await?;
        let page = store.list_runs_page(main, None, 60, false).await?;
        assert_eq!(page.len(), 60);
        assert_eq!(page.last().expect("newest").run.prompt, "hello");
        let earlier = store.list_runs_page(main, Some(page[0].run.created_at), 60, false).await?;
        assert_eq!(earlier.len(), 12);
        assert!(earlier.last().expect("run").run.created_at < page[0].run.created_at);
        assert_eq!(store.tidy_failed_turns(Some(agent), 24).await?, 0);
        assert_eq!(store.tidy_failed_turns(Some(agent), 0).await?, 70);
        let left: Vec<String> = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE session_id = $1 ORDER BY created_at")
            .bind(main)
            .fetch_all(&pool)
            .await?;
        assert_eq!(left, vec!["real try".to_string(), "hello".to_string()]);
        Ok(())
    }

    #[tokio::test]
    async fn cancel_task_and_delete_task_clean_up_quests() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, status, status_detail) VALUES ($1, 'Pri', 'claude', 'haiku', $2, 'waiting', 'Waiting for answers') RETURNING id")
            .bind(format!("pri-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'slack triage', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO task_milestones (task_id, position, title, kind, status) VALUES ($1, 0, 'merge', 'merge', 'active')")
            .bind(task)
            .execute(&pool)
            .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id, plan_state)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', 'active', $2, 'clarifying') RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let clarification: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Which account?', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "clarification": { "task_id": task, "session_id": session } }))
        .fetch_one(&pool)
        .await?;
        store
            .emit(
                NewEvent::new("attention.requested")
                    .agent(agent)
                    .session(session)
                    .task(Some(task))
                    .payload(json!({ "text": "Which account?", "message_id": clarification, "clarification": true })),
                None,
            )
            .await?;
        let running: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, started_at)
             VALUES ($1, $2, $3, 'task', 'running', 'do it', NOW()) RETURNING id",
        )
        .bind(session)
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let queued: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, 'followup', 'queued', 'more') RETURNING id",
        )
        .bind(session)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let prefix = &task.to_string()[..8];
        let found = store.find_tasks_by_prefix(prefix).await?;
        assert!(found.iter().any(|t| t.id == task));

        let cancelled = store.cancel_task(task, "Pri", Some("stuck")).await?;
        assert!(cancelled.was_open);
        assert_eq!(cancelled.dropped, 1);
        assert_eq!(cancelled.running, vec![running]);
        assert_eq!(cancelled.archived, 1);
        let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
        assert_eq!(status, "cancelled");
        assert_eq!(store.get_run(queued).await?.expect("queued").status, "cancelled");
        let closed = store.get_session(session).await?.expect("session");
        assert_eq!(closed.status, "archived");
        assert_eq!(closed.plan_state, None);
        let settled: (Value, Option<Uuid>) = sqlx::query_as(
            "SELECT meta, author_user_id FROM agent_messages WHERE agent_id = $1 AND meta->>'answers' = $2",
        )
        .bind(agent)
        .bind(clarification.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(settled.0["auto"], json!(true));
        assert_eq!(settled.1, Some(user));
        let close_event: Value = sqlx::query_scalar(
            "SELECT payload FROM world_events WHERE type = 'message.created' AND payload->>'answer_card' = $1",
        )
        .bind(clarification.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(close_event["quiet"], json!(true));
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "idle");
        let open_ask: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_notifications WHERE kind = 'ask' AND ref = $1 AND cleared_at IS NULL")
            .bind(clarification.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(open_ask, 0);
        let milestone: String = sqlx::query_scalar("SELECT status FROM task_milestones WHERE task_id = $1").bind(task).fetch_one(&pool).await?;
        assert_eq!(milestone, "skipped");
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.assigned' AND status = 'pending'",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(pending, 0);
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE type = 'task.cancelled' AND task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(events, 1);
        assert_eq!(store.request_stop(running, "Pri").await?, Some(false));

        let again = store.cancel_task(task, "Pri", None).await?;
        assert!(!again.was_open);
        assert_eq!(again.dropped, 0);

        let child: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id) VALUES ($1, 'child', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let child_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-2', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(child)
        .fetch_one(&pool)
        .await?;
        let child_queued: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'task', 'queued', 'go') RETURNING id",
        )
        .bind(child_session)
        .bind(agent)
        .bind(child)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_sessions SET plan_state = 'clarifying' WHERE id = $1")
            .bind(child_session)
            .execute(&pool)
            .await?;
        let child_clarification: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Which region?', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "clarification": { "task_id": child, "session_id": child_session } }))
        .fetch_one(&pool)
        .await?;
        let refused = store.delete_task(task, "Pri", None).await.expect_err("open subtask");
        assert!(refused.to_string().contains("open subtasks"));
        assert_eq!(store.get_run(child_queued).await?.expect("still queued").status, "queued");
        let moved = store
            .edit_quest(child, &QuestEdit { status: Some("review".into()), assignee: None }, user, "Ana")
            .await?;
        assert_eq!(moved.status, "review");
        assert_eq!(store.get_session(child_session).await?.expect("child session").plan_state, None);
        let child_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(child_clarification.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(child_answers, 1);
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(child).execute(&pool).await?;

        let deleted = store.delete_task(task, "Pri", Some("clutter")).await?;
        assert_eq!(deleted.subtasks, 1);
        assert_eq!(deleted.dropped, 1);
        assert_eq!(deleted.running, vec![running]);
        assert_eq!(deleted.archived, 1);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_tasks WHERE id = $1 OR id = $2")
            .bind(task)
            .bind(child)
            .fetch_one(&pool)
            .await?;
        assert_eq!(left, 0);
        let milestones: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_milestones WHERE task_id = $1").bind(task).fetch_one(&pool).await?;
        assert_eq!(milestones, 0);
        for id in [session, child_session] {
            let s = store.get_session(id).await?.expect("session kept");
            assert_eq!(s.status, "archived");
            assert_eq!(s.task_id, None);
        }
        let child_run = store.get_run(child_queued).await?.expect("run kept");
        assert_eq!(child_run.status, "cancelled");
        assert_eq!(child_run.task_id, None);
        let dangling: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM agent_sessions WHERE task_id IN ($1, $2))
                  + (SELECT COUNT(*) FROM session_runs WHERE task_id IN ($1, $2))
                  + (SELECT COUNT(*) FROM world_events WHERE task_id IN ($1, $2))",
        )
        .bind(task)
        .bind(child)
        .fetch_one(&pool)
        .await?;
        assert_eq!(dangling, 0);
        let deleted_event: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(deleted_event, 1);
        assert!(store.delete_task(task, "Pri", None).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn limit_errors_defer_turns_without_burning_attempts() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Lim', 'claude', 'sonnet') RETURNING id")
            .bind(format!("lim-{tag}"))
            .fetch_one(&pool)
            .await?;
        let mut ids = Vec::new();
        for _ in 0..2 {
            ids.push(
                sqlx::query_scalar::<_, i64>("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', '{}') RETURNING id")
                    .bind(agent)
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let processing = "UPDATE agent_inbox SET status = 'processing', attempts = 3 WHERE id = ANY($1)";
        sqlx::query(processing).bind(&ids).execute(&pool).await?;
        let reset = Utc::now() + chrono::Duration::minutes(90);
        store
            .requeue_inbox(&ids[..1], &format!("Claude Code returned error: Claude AI usage limit reached|{}", reset.timestamp()))
            .await?;
        store.requeue_inbox(&ids[1..], "Claude Code exited with code 1: boom").await?;
        let rows: Vec<(i64, String, i32, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT id, status, attempts, retry_at FROM agent_inbox WHERE id = ANY($1) ORDER BY id")
                .bind(&ids)
                .fetch_all(&pool)
                .await?;
        assert_eq!(rows[0].1, "pending", "a usage limit never fails the message");
        assert_eq!(rows[0].2, 2, "the limited attempt is given back");
        let wait = rows[0].3.expect("retry_at") - reset;
        assert!(wait.num_seconds() >= 0 && wait.num_seconds() < 60, "waits for the reset, got {wait}");
        assert_eq!(rows[1].1, "failed", "ordinary errors still give up after three attempts");
        sqlx::query("UPDATE agent_inbox SET status = 'processing', attempts = 1, retry_at = NULL WHERE id = $1").bind(ids[1]).execute(&pool).await?;
        store.requeue_inbox(&ids[1..], "Claude Code exited with code 1: boom").await?;
        let backoff: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT retry_at FROM agent_inbox WHERE id = $1").bind(ids[1]).fetch_one(&pool).await?;
        assert!(backoff.expect("default backoff") < Utc::now() + chrono::Duration::seconds(90), "the default 60 s backoff still applies");
        Ok(())
    }

    #[tokio::test]
    async fn daily_budget_warns_pauses_and_releases() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, daily_budget_usd) VALUES ($1, 'Bud', 'claude', 'sonnet', 1.0) RETURNING id",
        )
        .bind(format!("bud-{tag}"))
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'claude', 'sonnet', 'active') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let finish = |cost: f64| {
            let pool = pool.clone();
            async move {
                let run: Uuid = sqlx::query_scalar(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'running', 'hi') RETURNING id",
                )
                .bind(session)
                .bind(agent)
                .fetch_one(&pool)
                .await?;
                sqlx::query("UPDATE session_runs SET status = 'completed', cost_usd = $2, finished_at = NOW() WHERE id = $1")
                    .bind(run)
                    .bind(cost)
                    .execute(&pool)
                    .await?;
                anyhow::Ok(())
            }
        };
        let budget_events = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>("SELECT payload FROM world_events WHERE agent_id = $1 AND type = 'agent.budget' ORDER BY seq")
                    .bind(agent)
                    .fetch_all(&pool)
                    .await
            }
        };
        finish(0.5).await?;
        assert!(budget_events().await?.is_empty());
        finish(0.35).await?;
        finish(0.05).await?;
        let events = budget_events().await?;
        assert_eq!(events.len(), 1, "one warning per day");
        assert_eq!(events[0]["paused"], false);
        assert!(store.get_agent(agent).await?.unwrap().paused_reason.is_none());
        finish(0.2).await?;
        let events = budget_events().await?;
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["paused"], true);
        let paused = store.get_agent(agent).await?.unwrap().paused_reason.unwrap();
        assert!(paused.starts_with("Daily budget of $1.00 reached ($1.10"), "{paused}");
        assert!(!paused.contains('\u{2014}'));
        assert!(store.release_budget_pauses(Some(agent)).await?.is_empty(), "still over budget today");
        let raise = AgentUpdate { daily_budget_usd: Some(serde_json::json!(5)), ..Default::default() };
        assert_eq!(store.update_agent(agent, &raise).await?.unwrap().daily_budget_usd, Some(5.0));
        assert_eq!(store.release_budget_pauses(Some(agent)).await?, vec![agent], "raising the budget resumes the agent");
        assert!(store.get_agent(agent).await?.unwrap().paused_reason.is_none());
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Daily budget of $1.00 reached', budget_paused_on = CURRENT_DATE - 1, daily_budget_usd = 1 WHERE id = $1")
            .bind(agent)
            .execute(&pool)
            .await?;
        assert_eq!(store.release_budget_pauses(None).await?, vec![agent], "a new day resumes the agent");
        let clear = AgentUpdate { daily_budget_usd: Some(Value::Null), ..Default::default() };
        assert_eq!(store.update_agent(agent, &clear).await?.unwrap().daily_budget_usd, None);
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Paused by Ana' WHERE id = $1").bind(agent).execute(&pool).await?;
        assert!(store.release_budget_pauses(None).await?.is_empty(), "manual pauses are left alone");
        Ok(())
    }

    #[tokio::test]
    async fn usage_is_never_counted_twice() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Twice') RETURNING id")
            .bind(format!("twice-{tag}"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, 'Twice', 'codex', 'gpt-5.5', $2) RETURNING id")
            .bind(format!("twice-{tag}"))
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let session = |role: &'static str, backend: &'static str, cli: Option<&'static str>, baseline: Value| {
            let pool = pool.clone();
            async move {
                anyhow::Ok(
                    sqlx::query_scalar::<_, Uuid>(
                        "INSERT INTO agent_sessions (agent_id, role, backend, model, status, cli_session_id, usage_baseline)
                         VALUES ($1, $2, $3, 'gpt-5.5', 'active', $4, $5) RETURNING id",
                    )
                    .bind(agent)
                    .bind(role)
                    .bind(backend)
                    .bind(cli)
                    .bind(baseline)
                    .fetch_one(&pool)
                    .await?,
                )
            }
        };
        let baseline_of = |id: Uuid| {
            let pool = pool.clone();
            async move { anyhow::Ok(sqlx::query_scalar::<_, Value>("SELECT usage_baseline FROM agent_sessions WHERE id = $1").bind(id).fetch_one(&pool).await?) }
        };

        let main = session("main", "codex", Some("main-thread"), serde_json::json!({ "input_tokens": 1000, "cached_input_tokens": 0, "output_tokens": 10 })).await?;
        let task = session("task", "codex", None, serde_json::json!({ "input_tokens": 400, "output_tokens": 4 })).await?;
        let other_cli = session("task", "claude", None, serde_json::json!({})).await?;
        let started = session("task", "codex", Some("own-thread"), serde_json::json!({ "input_tokens": 7 })).await?;
        assert_eq!(store.take_fork_baseline(task, main).await?, Some(baseline_of(main).await?), "a fork starts from the parent as it is when the fork runs");
        assert_eq!(store.take_fork_baseline(other_cli, main).await?, None, "another CLI starts its own count");
        assert_eq!(store.take_fork_baseline(started, main).await?, None, "a session that already runs keeps its own count");
        assert_eq!(baseline_of(started).await?, serde_json::json!({ "input_tokens": 7 }));

        let failed = |session: Uuid| {
            let pool = pool.clone();
            async move {
                anyhow::Ok(
                    sqlx::query_scalar::<_, Uuid>("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'followup', 'running', 'go') RETURNING id")
                        .bind(session)
                        .bind(agent)
                        .fetch_one(&pool)
                        .await?,
                )
            }
        };
        let thread = session("task", "codex", Some("thread"), serde_json::json!({ "input_tokens": 100, "cached_input_tokens": 50, "output_tokens": 5, "cost_usd": 0.0 })).await?;
        let run = failed(thread).await?;
        store.record_live_usage(run, 0.2, 40, 10, 3).await?;
        let stop = |cost: f64, input: i64, output: i64| super::RunOutcome { status: "failed", output: None, error: Some("stopped"), cost_usd: cost, input_tokens: input, cached_input_tokens: 0, output_tokens: output };
        assert!(store.finish_run(run, stop(0.0, 0, 0)).await?);
        assert_eq!(
            baseline_of(thread).await?,
            serde_json::json!({ "input_tokens": 140, "cached_input_tokens": 60, "output_tokens": 8, "cost_usd": 0.0 }),
            "the next run of the thread does not count what the stopped run already recorded"
        );
        assert!(!store.finish_run(run, stop(0.0, 55, 3)).await?);
        assert_eq!(baseline_of(thread).await?["input_tokens"], 155, "a late larger figure adds only the difference");

        let claude = session("task", "claude", Some("claude-session"), serde_json::json!({ "cost_usd": 3.0 })).await?;
        let run = failed(claude).await?;
        store.record_live_usage(run, 0.4, 900, 0, 30).await?;
        assert!(store.finish_run(run, stop(0.0, 0, 0)).await?);
        assert_eq!(baseline_of(claude).await?, serde_json::json!({ "cost_usd": 3.0 }), "Claude keeps a killed run out of its own total, so nothing is folded in");
        Ok(())
    }

    #[test]
    fn a_reconnect_replays_a_small_gap_and_resyncs_a_stale_one() {
        let gap = |head, missed| ReplayGap { head, missed };
        assert_eq!(gap(100, 3).resume_from(97), Resume::Replay(97));
        assert_eq!(gap(100, 0).resume_from(100), Resume::Replay(100));
        assert_eq!(gap(100, REPLAY_MAX_EVENTS).resume_from(1), Resume::Replay(1));
        assert_eq!(gap(100_000, REPLAY_MAX_EVENTS + 1).resume_from(1), Resume::Resync(100_000));
        assert_eq!(gap(100, 0).resume_from(500), Resume::Resync(100), "an id from the future means the log was reset");
        assert_eq!(gap(100, 0).resume_from(-1), Resume::Resync(100));
    }

    #[tokio::test]
    async fn replay_gap_counts_only_the_orgs_own_missed_events() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut orgs = Vec::new();
        for name in ["a", "b"] {
            orgs.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO orgs (slug, name) VALUES ($1, 'Replay') RETURNING id")
                    .bind(format!("replay-{name}-{}", &tag[..16]))
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let mut seqs = Vec::new();
        for (i, org) in [orgs[0], orgs[1], orgs[0], orgs[0]].into_iter().enumerate() {
            seqs.push(
                sqlx::query_scalar::<_, i64>("INSERT INTO world_events (type, payload, org_id) VALUES ('test.replay', $1, $2) RETURNING seq")
                    .bind(serde_json::json!({ "i": i }))
                    .bind(org)
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let gap = store.replay_gap(orgs[0], seqs[0], 10).await?;
        assert_eq!(gap.missed, 2);
        assert_eq!(gap.head, seqs[3]);
        assert_eq!(store.replay_gap(orgs[1], 0, 10).await?.head, seqs[1], "the head never reaches past the org's own events");
        assert_eq!(gap.resume_from(seqs[0]), Resume::Replay(seqs[0]));
        let events = store.events_since_in(orgs[0], seqs[0], 10).await?;
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![seqs[2], seqs[3]], "replay never includes another org's events");
        assert_eq!(store.replay_gap(orgs[0], seqs[0], 1).await?.missed, 2, "the count stops one past the cap");
        assert_eq!(store.replay_gap(orgs[0], seqs[0], 1).await?.resume_from(seqs[0]), Resume::Replay(seqs[0]));
        assert_eq!(store.replay_gap(orgs[0], gap.head + 1000, 10).await?.resume_from(gap.head + 1000), Resume::Resync(gap.head));
        sqlx::query("DELETE FROM world_events WHERE seq = ANY($1)").bind(&seqs).execute(&pool).await?;
        sqlx::query("DELETE FROM orgs WHERE id = ANY($1)").bind(&orgs).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn budgets_count_live_spend_and_stop_running_work() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Meter') RETURNING id")
            .bind(format!("meter-{tag}"))
            .fetch_one(&pool)
            .await?;
        let new_agent = |name: &'static str, cli: &'static str| {
            let (pool, tag) = (pool.clone(), tag.clone());
            async move {
                let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, $2, $3, 'composer-2.5', $4) RETURNING id")
                    .bind(format!("{}-{tag}", name.to_lowercase()))
                    .bind(name)
                    .bind(cli)
                    .bind(org)
                    .fetch_one(&pool)
                    .await?;
                let session: Uuid = sqlx::query_scalar(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'task', $2, 'composer-2.5', 'active') RETURNING id",
                )
                .bind(agent)
                .bind(cli)
                .fetch_one(&pool)
                .await?;
                anyhow::Ok((agent, session))
            }
        };
        let running = |agent: Uuid, session: Uuid| {
            let pool = pool.clone();
            async move {
                anyhow::Ok(
                    sqlx::query_scalar::<_, Uuid>(
                        "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'task', 'running', 'go') RETURNING id",
                    )
                    .bind(session)
                    .bind(agent)
                    .fetch_one(&pool)
                    .await?,
                )
            }
        };

        let (cursor, cursor_session) = new_agent("Cur", "cursor").await?;
        assert!(!store.metered(cursor).await?, "no budget means no metering");
        let tokens = AgentUpdate { daily_token_budget: Some(serde_json::json!(1000)), ..Default::default() };
        assert_eq!(store.update_agent(cursor, &tokens).await?.unwrap().daily_token_budget, Some(1000));
        assert!(store.metered(cursor).await?);
        let first = running(cursor, cursor_session).await?;
        let other = running(cursor, cursor_session).await?;
        assert!(store.record_live_usage(first, 0.0, 300, 50_000, 200).await?.is_empty(), "cached reads do not count");
        assert!(store.get_agent(cursor).await?.unwrap().paused_reason.is_none());
        let stopped = store.record_live_usage(first, 0.0, 600, 50_000, 500).await?;
        let paused = store.get_agent(cursor).await?.unwrap().paused_reason.unwrap();
        assert!(paused.starts_with("Daily budget of 1K tokens reached (1.1K tokens used today)"), "{paused}");
        let mut runs = stopped.runs.clone();
        runs.sort();
        let mut expected = vec![first, other];
        expected.sort();
        assert_eq!(runs, expected, "running work of the paused agent is stopped");
        assert_eq!(store.stop_requested(other).await?.as_deref(), Some(crate::withdraw::STOPPED_BY));
        assert!(store.spend_blocker(cursor).await?.is_some());
        assert!(store.record_live_usage(first, 0.0, 600, 50_000, 500).await?.is_empty(), "no change, nothing to do");

        let finished = store
            .finish_run(first, super::RunOutcome { status: "failed", output: None, error: Some("stopped"), cost_usd: 0.0, input_tokens: 0, cached_input_tokens: 0, output_tokens: 0 })
            .await?;
        assert!(finished);
        let kept: (i64, i64) = sqlx::query_as("SELECT input_tokens, output_tokens FROM session_runs WHERE id = $1").bind(first).fetch_one(&pool).await?;
        assert_eq!(kept, (600, 500), "a failed run keeps the spend it streamed");

        assert!(store.release_budget_pauses(Some(cursor)).await?.is_empty(), "still over the token budget");
        let raise = AgentUpdate { daily_token_budget: Some(serde_json::json!(5000)), ..Default::default() };
        store.update_agent(cursor, &raise).await?;
        assert_eq!(store.release_budget_pauses(Some(cursor)).await?, vec![cursor]);

        let (codex, codex_session) = new_agent("Cod", "codex").await?;
        let money = AgentUpdate { daily_budget_usd: Some(serde_json::json!(1)), ..Default::default() };
        store.update_agent(codex, &money).await?;
        let run = running(codex, codex_session).await?;
        assert!(store.record_live_usage(run, 0.6, 10, 0, 10).await?.is_empty());
        store
            .finish_run(run, super::RunOutcome { status: "completed", output: Some("ok"), error: None, cost_usd: 0.5, input_tokens: 12, cached_input_tokens: 0, output_tokens: 11 })
            .await?;
        let exact: f64 = sqlx::query_scalar("SELECT cost_usd FROM session_runs WHERE id = $1").bind(run).fetch_one(&pool).await?;
        assert!((exact - 0.5).abs() < 1e-9, "a completed run records what the CLI reported, got {exact}");
        let run = running(codex, codex_session).await?;
        store.record_live_usage(run, 0.7, 10, 0, 10).await?;
        let paused = store.get_agent(codex).await?.unwrap().paused_reason.unwrap();
        assert!(paused.starts_with("Daily budget of $1.00 reached ($1.20"), "{paused}");
        assert_eq!(store.stop_requested(run).await?.as_deref(), Some(crate::withdraw::STOPPED_BY));

        Ok(())
    }

    #[tokio::test]
    async fn rejected_model_key_pauses_once_and_resumes_on_key_update() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("key-{tag}@example.com"))
            .fetch_one(&pool)
            .await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Kim', 'omp', 'openrouter/kimi', $2) RETURNING id",
        )
        .bind(format!("key-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let agent = store.get_agent(id).await?.unwrap();
        let cards = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>("SELECT payload FROM world_events WHERE agent_id = $1 AND type = 'attention.requested'")
                    .bind(id)
                    .fetch_all(&pool)
                    .await
            }
        };
        assert!(!store.pause_for_rejected_key(&agent, "API Error: 503 Service Unavailable").await?);
        assert!(store.get_agent(id).await?.unwrap().paused_reason.is_none(), "transient errors keep retrying");
        let rejected = "omp model call failed: 401 status code (no body)";
        assert!(store.pause_for_rejected_key(&agent, rejected).await?);
        assert!(!store.pause_for_rejected_key(&agent, rejected).await?);
        assert!(!store.pause_for_rejected_key(&agent, rejected).await?);
        let cards = cards().await?;
        assert_eq!(cards.len(), 1, "one card while paused");
        assert_eq!(cards[0]["key"], "OPENROUTER_API_KEY");
        assert_eq!(cards[0]["reason"], super::KEY_REJECTED_REASON);
        let reason = store.get_agent(id).await?.unwrap().paused_reason.unwrap();
        assert_eq!(reason, "Model key rejected: OPENROUTER_API_KEY");
        assert!(store.release_key_pauses(owner, None, Some("FAL_KEY")).await?.is_empty(), "other keys leave it paused");
        assert_eq!(store.release_key_pauses(owner, None, Some("OPENROUTER_API_KEY")).await?, vec![id]);
        assert!(store.get_agent(id).await?.unwrap().paused_reason.is_none());
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Paused by Ana' WHERE id = $1").bind(id).execute(&pool).await?;
        assert!(!store.pause_for_rejected_key(&agent, rejected).await?, "a manual pause is kept");
        assert!(store.release_key_pauses(owner, Some(id), None).await?.is_empty(), "key updates leave manual pauses alone");
        Ok(())
    }

    #[tokio::test]
    async fn every_retry_path_keeps_what_the_run_was_asked() -> anyhow::Result<()> {
        use crate::run_causes::{diagnose, RunEnd};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Tara', 'write') RETURNING id")
            .bind(format!("tara-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Mender', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("mender-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id) VALUES ($1, 'main', 'claude', 'sonnet', $2) RETURNING id")
            .bind(agent)
            .bind(format!("main-{tag}"))
            .fetch_one(&pool)
            .await?;
        let quest = |status: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_tasks (user_id, title, description, status, assigned_agent_id) VALUES ($1, 'Fix the drawer', 'Close it on Esc', $2, $3) RETURNING id",
            )
            .bind(user)
            .bind(status)
            .bind(agent)
            .fetch_one(&pool)
        };
        let session = |role: &'static str, task: Option<Uuid>, cli: Option<String>, status: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id, working_dir, parent_session_id)
                 VALUES ($1, $2, 'claude', 'sonnet', $3, $4, $5, '/quests/drawer', $6) RETURNING id",
            )
            .bind(agent)
            .bind(role)
            .bind(cli)
            .bind(status)
            .bind(task)
            .bind(main)
            .fetch_one(&pool)
        };
        let run = |session: Uuid, task: Option<Uuid>, kind: &'static str, status: &'static str, fork: Option<Uuid>, request: Value| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt, request, started_at, finished_at)
                 VALUES ($1, $2, $3, $4, $5, $6, 'the full runtime prompt', $7, NOW() - INTERVAL '3 seconds', NOW()) RETURNING id",
            )
            .bind(session)
            .bind(agent)
            .bind(task)
            .bind(kind)
            .bind(status)
            .bind(fork)
            .bind(request)
            .fetch_one(&pool)
        };
        let asked = |id: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_scalar::<_, Option<Value>>("SELECT request FROM session_runs WHERE id = $1").bind(id).fetch_one(&pool).await }
        };
        let task_ask = json!({ "text": "Close it on Esc", "brief": "Start with the key handler" });
        let followup_ask = json!({ "from": "Tara", "text": "Add a test for Esc" });
        let with_retry = |ask: &Value, n: u32| {
            let mut ask = ask.clone();
            ask["retry"] = json!(n);
            Some(ask)
        };

        let task = quest("failed").await?;
        let failed_session = session("task", Some(task), None, "failed").await?;
        let failed = run(failed_session, Some(task), "task", "failed", None, task_ask.clone()).await?;
        let retried = store.retry_run(failed, user).await.map_err(|e| anyhow::anyhow!("{e:?}"))?.run.expect("a retried run");
        assert_eq!(retried.request, with_retry(&task_ask, 1), "Retry keeps the description and brief");
        sqlx::query("UPDATE session_runs SET status = 'failed', finished_at = NOW() WHERE id = $1").bind(retried.id).execute(&pool).await?;
        sqlx::query("UPDATE agent_sessions SET status = 'failed' WHERE id = $1").bind(failed_session).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1").bind(task).execute(&pool).await?;
        let twice = store.retry_run(retried.id, user).await.map_err(|e| anyhow::anyhow!("{e:?}"))?.run.expect("a second retry");
        assert_eq!(twice.request, with_retry(&task_ask, 2), "retries count up");

        let task = quest("in_progress").await?;
        let fresh_session = session("task", Some(task), None, "active").await?;
        let never = run(fresh_session, Some(task), "task", "completed", None, task_ask.clone()).await?;
        let diagnosis = store.record_run_end(never, "task", None, "", false).await?;
        let again = store.auto_retry(never, &diagnosis).await?.expect("a run that never started is retried");
        assert_eq!(asked(again).await?, with_retry(&task_ask, 1), "auto retry keeps the description and brief");

        let task = quest("in_progress").await?;
        let live = session("task", Some(task), Some(format!("cli-{tag}")), "active").await?;
        let cut = run(live, Some(task), "followup", "failed", None, followup_ask.clone()).await?;
        store.insert_run_event(cut, "tool", Some("Bash"), "cargo test").await?;
        store.insert_run_event(cut, "text", None, "running the tests").await?;
        let network = store.record_run_end(cut, "followup", Some("stream disconnected before completion"), "", false).await?;
        let resumed = store.auto_retry(cut, &network).await?.expect("a cut follow-up is resumed");
        let (kind, prompt): (String, String) = sqlx::query_as("SELECT kind, prompt FROM session_runs WHERE id = $1").bind(resumed).fetch_one(&pool).await?;
        assert_eq!(kind, "followup");
        assert_ne!(prompt, "the full runtime prompt", "the resume sends its own prompt");
        assert_eq!(asked(resumed).await?, with_retry(&followup_ask, 1), "the history card still shows what was asked");

        let task = quest("in_progress").await?;
        let forked = session("task", Some(task), Some(format!("fork-{tag}")), "active").await?;
        let too_long = diagnose(&RunEnd { kind: "task", error: Some("Prompt is too long"), output: "", turns: 1, timed_out: false });
        let overflow = run(forked, Some(task), "task", "failed", Some(main), task_ask.clone()).await?;
        sqlx::query("UPDATE session_runs SET cause = 'cli_error', diagnosis = $2 WHERE id = $1")
            .bind(overflow)
            .bind(serde_json::to_value(&too_long)?)
            .execute(&pool)
            .await?;
        let fresh = store.fresh_retry(overflow, &too_long).await?.expect("a fork that was too long starts fresh");
        assert_eq!(asked(fresh).await?, with_retry(&task_ask, 1), "a fresh start keeps the description and brief");

        let aside_ask = json!({ "from": "Mei Chen", "text": "How far along?" });
        let aside = session("aside", None, None, "active").await?;
        let paused = run(aside, None, "aside", "failed", Some(main), aside_ask.clone()).await?;
        assert!(store.resume_after_update(paused).await?);
        let picked: Value = sqlx::query_scalar("SELECT request FROM session_runs WHERE session_id = $1 AND id <> $2 ORDER BY created_at DESC LIMIT 1")
            .bind(aside)
            .bind(paused)
            .fetch_one(&pool)
            .await?;
        assert_eq!(picked, aside_ask, "picking up after an update is not a retry and keeps the request as it was");
        Ok(())
    }

    #[tokio::test]
    async fn run_causes_retry_once_and_only_nudge_real_work() -> anyhow::Result<()> {
        use crate::run_causes::Cause;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Patch', 'claude', 'sonnet') RETURNING id")
            .bind(format!("patch-{tag}"))
            .fetch_one(&pool)
            .await?;
        let quest = |title: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, $2, 'in_progress', $3) RETURNING id",
            )
            .bind(user)
            .bind(title)
            .bind(agent)
            .fetch_one(&pool)
        };
        let session_for = |task: Uuid, cli: Option<&'static str>| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
                 VALUES ($1, 'task', 'claude', 'sonnet', $2, 'active', $3) RETURNING id",
            )
            .bind(agent)
            .bind(cli)
            .bind(task)
            .fetch_one(&pool)
        };
        let finished_run = |session: Uuid, task: Uuid, kind: &'static str, status: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, started_at, finished_at)
                 VALUES ($1, $2, $3, $4, $5, 'do the loka check', NOW() - INTERVAL '3 seconds', NOW()) RETURNING id",
            )
            .bind(session)
            .bind(agent)
            .bind(task)
            .bind(kind)
            .bind(status)
            .fetch_one(&pool)
        };

        let task = quest("loka check").await?;
        let session = session_for(task, None).await?;
        let first = finished_run(session, task, "task", "completed").await?;
        let diagnosis = store.record_run_end(first, "task", None, "", false).await?;
        assert_eq!(diagnosis.cause, Cause::NeverStarted);
        assert!(!diagnosis.auto_retried);
        assert!(diagnosis.duration_secs >= 2);
        let stored: Option<String> = sqlx::query_scalar("SELECT cause FROM session_runs WHERE id = $1").bind(first).fetch_one(&pool).await?;
        assert_eq!(stored.as_deref(), Some("never_started"));
        assert_eq!(store.nudge_missing(first, session, Required::Report).await?, Nudge::Exhausted, "a session that never started has nothing to nudge");

        let retry = store.auto_retry(first, &diagnosis).await?.expect("first never_started run is retried");
        let (kind, prompt, retry_cause, delayed): (String, String, Option<String>, bool) = sqlx::query_as(
            "SELECT kind, prompt, retry_cause, not_before > NOW() FROM session_runs WHERE id = $1",
        )
        .bind(retry)
        .fetch_one(&pool)
        .await?;
        assert_eq!((kind.as_str(), prompt.as_str(), retry_cause.as_deref(), delayed), ("task", "do the loka check", Some("never_started"), true));
        assert!(
            store.claim_queued_run("test").await?.is_none_or(|run| run.id != retry),
            "the retry waits for its backoff"
        );
        assert!(matches!(store.resume_task_session(agent, task, "Fea", Some("go")).await?, TaskResume::Busy { session_id: id, .. } if id == session));
        let retrying: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE type = 'run.retrying' AND task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(retrying, 1);

        sqlx::query("UPDATE session_runs SET status = 'failed', finished_at = NOW() WHERE id = $1").bind(retry).execute(&pool).await?;
        let again = store.record_run_end(retry, "task", Some("Claude Code exited with code 1: spawn ENOENT"), "", false).await?;
        assert_eq!(again.cause, Cause::NeverStarted);
        assert!(again.auto_retried, "the second failure knows the Loka already retried");
        assert!(store.auto_retry(retry, &again).await?.is_none(), "one auto retry per quest per cause");
        assert_eq!(store.latest_diagnosis(session).await?.map(|d| d.cause), Some(Cause::NeverStarted));

        let task = quest("network blip").await?;
        let session = session_for(task, Some("claude-session")).await?;
        let cut = finished_run(session, task, "task", "failed").await?;
        store.insert_run_event(cut, "tool", Some("Bash"), "cargo test").await?;
        store.insert_run_event(cut, "text", None, "running the tests").await?;
        let network = store.record_run_end(cut, "task", Some("stream disconnected before completion"), "", false).await?;
        assert_eq!((network.cause, network.failure.as_deref(), network.turns), (Cause::CliError, Some("network"), 2));
        assert!(network.tail.contains("running the tests") && network.tail.ends_with("stream disconnected before completion"));
        let resumed = store.auto_retry(cut, &network).await?.expect("transient error is resumed");
        let (kind, prompt): (String, String) = sqlx::query_as("SELECT kind, prompt FROM session_runs WHERE id = $1").bind(resumed).fetch_one(&pool).await?;
        assert_eq!((kind.as_str(), prompt.as_str()), ("followup", crate::run_causes::RESUME_AFTER_ERROR));

        let task = quest("mid update").await?;
        let session = session_for(task, Some("claude-session")).await?;
        let mut paused = finished_run(session, task, "task", "failed").await?;
        store.insert_run_event(paused, "tool", Some("Bash"), "cargo build").await?;
        for round in 0..2 {
            let update = store.record_run_end(paused, "task", Some(crate::run_causes::UPDATE_PAUSE), "", false).await?;
            assert_eq!(update.failure.as_deref(), Some("update"));
            let next = store.auto_retry(paused, &update).await?.expect("every update pause resumes");
            let (kind, prompt, cause, waits): (String, String, Option<String>, bool) =
                sqlx::query_as("SELECT kind, prompt, retry_cause, not_before > NOW() FROM session_runs WHERE id = $1").bind(next).fetch_one(&pool).await?;
            assert_eq!((kind.as_str(), prompt.as_str(), cause.as_deref(), waits), ("followup", crate::run_causes::RESUME_AFTER_UPDATE, Some("update"), false), "round {round}");
            sqlx::query("UPDATE session_runs SET status = 'failed', finished_at = NOW() WHERE id = $1").bind(next).execute(&pool).await?;
            store.insert_run_event(next, "tool", Some("Bash"), "cargo test").await?;
            paused = next;
        }
        let network = store.record_run_end(paused, "task", Some("stream disconnected before completion"), "", false).await?;
        assert!(store.auto_retry(paused, &network).await?.is_some(), "updates never use up the hiccup retry");

        let direct: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status) VALUES ($1, 'direct', 'claude', 'sonnet', 'folder-session', 'active') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let cut: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at, finished_at) VALUES ($1, $2, 'direct', 'failed', 'fix the build', NOW(), NOW()) RETURNING id",
        )
        .bind(direct)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert!(store.resume_after_update(cut).await?);
        let (kind, prompt): (String, String) =
            sqlx::query_as("SELECT kind, prompt FROM session_runs WHERE session_id = $1 AND status = 'queued'").bind(direct).fetch_one(&pool).await?;
        assert_eq!((kind.as_str(), prompt.as_str()), ("direct", crate::run_causes::RESUME_DIRECT_AFTER_UPDATE));

        let task = quest("bad login").await?;
        let session = session_for(task, Some("claude-session")).await?;
        let auth = finished_run(session, task, "task", "failed").await?;
        let rejected = store.record_run_end(auth, "task", Some("Invalid API key · Please run /login"), "", false).await?;
        assert!(store.auto_retry(auth, &rejected).await?.is_none(), "auth errors need a person");

        let task = quest("silent finish").await?;
        let session = session_for(task, Some("claude-session")).await?;
        let worked = finished_run(session, task, "task", "completed").await?;
        store.insert_run_event(worked, "tool", Some("Edit"), "src/lib.rs").await?;
        let quiet = store.record_run_end(worked, "task", None, "All done, tests pass.", false).await?;
        assert_eq!(quiet.cause, Cause::NoReport);
        assert!(store.auto_retry(worked, &quiet).await?.is_none(), "no_report is nudged, never auto retried");
        let Nudge::Queued(first_reminder) = store.nudge_missing(worked, session, Required::Report).await? else {
            panic!("the first miss is reminded");
        };
        assert_eq!(store.nudge_missing(worked, session, Required::Report).await?, Nudge::Open, "the queued nudge decides, nothing is settled meanwhile");
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(first_reminder).execute(&pool).await?;
        let Nudge::Queued(nudge) = store.nudge_missing(first_reminder, session, Required::Report).await? else {
            panic!("a reminder that still has no report is reminded once more");
        };
        let prompts: Vec<String> = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = ANY($1) ORDER BY created_at, id")
            .bind(vec![first_reminder, nudge])
            .fetch_all(&pool)
            .await?;
        assert_eq!(prompts, [super::REPORT_NUDGE, super::REPORT_NUDGE_LAST], "the second reminder asks for nothing but the report");
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(nudge).execute(&pool).await?;
        assert_eq!(store.nudge_missing(nudge, session, Required::Report).await?, Nudge::Exhausted, "two reminders without a report end the chase");
        let failed = store.fail_missing(nudge, Required::Report, "Still all done.").await?;
        assert_eq!((failed.cause, failed.reason.as_str()), (Cause::NoReport, crate::run_causes::MISSED_REPORT));
        let (status, error, cause): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, error, cause FROM session_runs WHERE id = $1").bind(nudge).fetch_one(&pool).await?;
        assert_eq!((status.as_str(), error.as_deref(), cause.as_deref()), ("failed", Some(crate::run_causes::MISSED_REPORT), Some("no_report")));
        let latest = store.latest_diagnosis(session).await?.expect("last nudge is diagnosed");
        assert_eq!((latest.cause, latest.tail.as_str()), (Cause::NoReport, "Still all done."));
        Ok(())
    }

    #[tokio::test]
    async fn a_review_without_a_verdict_is_resumed_once_then_failed_with_its_cause() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Rev', 'codex', 'gpt') RETURNING id")
            .bind(format!("rev-{tag}"))
            .fetch_one(&pool)
            .await?;
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'review me', 'done', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let parent = Uuid::new_v4();
        insert_session(&pool, parent, agent, "task", "completed", Some(task), None, Some("author"), None, None).await?;

        let fresh = Uuid::new_v4();
        insert_session(&pool, fresh, agent, "review", "completed", Some(task), Some(parent), None, None, None).await?;
        let never = insert_run(&pool, fresh, agent, Some(task), "review", "completed", None).await?;
        assert_eq!(store.nudge_missing(never, fresh, Required::Verdict).await?, Nudge::Exhausted, "a review that never started has nothing to resume");

        let review = Uuid::new_v4();
        insert_session(&pool, review, agent, "review", "completed", Some(task), Some(parent), Some("reviewer-1"), None, Some(serde_json::json!({ "manual": true }))).await?;
        let first = insert_run(&pool, review, agent, Some(task), "review", "completed", None).await?;
        let Nudge::Queued(one) = store.nudge_missing(first, review, Required::Verdict).await? else {
            panic!("the first miss is nudged once");
        };
        assert_eq!(store.nudge_missing(first, review, Required::Verdict).await?, Nudge::Open, "one resume at a time");
        let (kind, status, prompt, session_status): (String, String, String, String) = sqlx::query_as(
            "SELECT r.kind, r.status, r.prompt, s.status FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id WHERE r.id = $1",
        )
        .bind(one)
        .fetch_one(&pool)
        .await?;
        assert_eq!((kind.as_str(), status.as_str(), prompt.as_str(), session_status.as_str()), ("review", "queued", VERDICT_NUDGE, "active"));
        assert_ne!(one, first);
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(one).execute(&pool).await?;
        assert_eq!(store.nudge_missing(one, review, Required::Verdict).await?, Nudge::Exhausted, "a nudge that still has no verdict ends the review");
        let failed = store.fail_missing(one, Required::Verdict, "Waiting on the test run.").await?;
        assert_eq!((failed.cause, failed.reason.as_str()), (crate::run_causes::Cause::NoVerdict, crate::run_causes::MISSED_VERDICT));
        let (status, cause): (String, Option<String>) = sqlx::query_as("SELECT status, cause FROM session_runs WHERE id = $1").bind(one).fetch_one(&pool).await?;
        assert_eq!((status.as_str(), cause.as_deref()), ("failed", Some("no_verdict")), "a review with no verdict is never counted as completed");
        assert_eq!(store.latest_diagnosis(parent).await?.map(|d| d.cause), Some(crate::run_causes::Cause::NoVerdict));

        let auto = Uuid::new_v4();
        insert_session(&pool, auto, agent, "review", "completed", Some(task), Some(parent), Some("reviewer-2"), None, None).await?;
        let ran = insert_run(&pool, auto, agent, Some(task), "review", "completed", None).await?;
        let Nudge::Queued(nudge) = store.nudge_missing(ran, auto, Required::Verdict).await? else {
            panic!("automatic reviews are nudged too");
        };
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(nudge).execute(&pool).await?;
        assert_eq!(store.nudge_missing(nudge, auto, Required::Verdict).await?, Nudge::Exhausted);
        let later = insert_run(&pool, auto, agent, Some(task), "review", "completed", None).await?;
        assert!(matches!(store.nudge_missing(later, auto, Required::Verdict).await?, Nudge::Queued(_)), "a later miss gets its own nudge");
        Ok(())
    }

    #[tokio::test]
    async fn every_run_with_a_contract_emits_exactly_one_terminal_event() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Ends', 'claude', 'opus') RETURNING id")
            .bind(format!("ends-{tag}"))
            .fetch_one(&pool)
            .await?;
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ends-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'end once', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let ends = |run: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, Value)>("SELECT type, payload FROM world_events WHERE run_id = $1 AND type = ANY($2) ORDER BY seq")
                    .bind(run)
                    .bind(&super::RUN_END_EVENTS[..])
                    .fetch_all(&pool)
                    .await
            }
        };
        let parent = Uuid::new_v4();
        insert_session(&pool, parent, agent, "task", "active", Some(task), None, Some("author"), None, None).await?;
        for (kind, required) in [("task", Required::Report), ("followup", Required::Report), ("review", Required::Verdict)] {
            let session = match required {
                Required::Report => parent,
                Required::Verdict => {
                    let review = Uuid::new_v4();
                    insert_session(&pool, review, agent, "review", "active", Some(task), Some(parent), Some(&format!("rev-{kind}")), None, None).await?;
                    review
                }
            };
            let names = |events: &[(String, Value)]| events.iter().map(|(event, _)| event.clone()).collect::<Vec<_>>();
            let first = insert_run(&pool, session, agent, Some(task), kind, "completed", None).await?;
            sqlx::query("UPDATE session_runs SET cost_usd = 0.25 WHERE id = $1").bind(first).execute(&pool).await?;
            let Nudge::Queued(nudge) = store.settle_missing(first, session, required, "Waiting on the test run.").await? else {
                panic!("{kind}: the first miss is nudged");
            };
            let events = ends(first).await?;
            assert_eq!(names(&events), ["run.completed"], "{kind}: a nudged run completed, and says so the moment the nudge is queued");
            assert_eq!(events[0].1["cost_usd"], 0.25, "{kind}");
            assert!(!store.announce_completed(first).await?, "{kind}: the handler's fallback announce is a no-op");

            sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(nudge).execute(&pool).await?;
            let nudge = if required == Required::Report {
                let Nudge::Queued(last) = store.settle_missing(nudge, session, required, "Still waiting.").await? else {
                    panic!("{kind}: a report is reminded twice");
                };
                assert_eq!(names(&ends(nudge).await?), ["run.completed"], "{kind}: the first reminder completed, and says so");
                sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(last).execute(&pool).await?;
                last
            } else {
                nudge
            };
            assert_eq!(store.settle_missing(nudge, session, required, "Still waiting.").await?, Nudge::Exhausted, "{kind}");
            assert!(!store.announce_completed(nudge).await?, "{kind}: a failed run is never announced as completed");
            let events = ends(nudge).await?;
            assert_eq!(names(&events), ["run.failed"], "{kind}: one terminal event, and it is the final one");
            let payload = &events[0].1;
            assert_eq!(payload["cause"], required.cause().as_str(), "{kind}");
            let nudge_kind = if required == Required::Report { "followup" } else { "review" };
            assert_eq!((payload["run_kind"].as_str(), payload["quiet"].as_bool()), (Some(nudge_kind), Some(true)), "{kind}: the quest's own finish wakes the agent");
            let status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1").bind(nudge).fetch_one(&pool).await?;
            assert_eq!(status, "failed", "{kind}");

            let met = insert_run(&pool, session, agent, Some(task), kind, "completed", None).await?;
            assert!(store.announce_completed(met).await?);
            assert!(!store.announce_completed(met).await?, "{kind}: announcing again after the handler is a no-op");
            assert_eq!(names(&ends(met).await?), ["run.completed"], "{kind}");
            sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE session_id = $1 AND status = 'queued'").bind(session).execute(&pool).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn report_nudges_and_task_resume_never_duplicate_work() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Fea', 'codex', 'gpt') RETURNING id")
            .bind(format!("fea-{tag}"))
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'fix bmc', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert!(matches!(store.resume_task_session(agent, task, "Fea", Some("go")).await?, TaskResume::Fresh));
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'codex', 'gpt', 'thread-1', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let task_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, started_at)
             VALUES ($1, $2, $3, 'task', 'running', 'do it', NOW()) RETURNING id",
        )
        .bind(session)
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert!(matches!(store.resume_task_session(agent, task, "Fea", Some("go")).await?, TaskResume::Busy { session_id: id, .. } if id == session));
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1")
            .bind(task_run)
            .execute(&pool)
            .await?;
        let Nudge::Queued(first) = store.nudge_missing(task_run, session, Required::Report).await? else {
            panic!("the first miss is reminded");
        };
        assert!(matches!(store.resume_task_session(agent, task, "Fea", Some("go")).await?, TaskResume::Busy { .. }));
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1")
            .bind(first)
            .execute(&pool)
            .await?;
        let Nudge::Queued(second) = store.nudge_missing(first, session, Required::Report).await? else {
            panic!("the second reminder follows the first");
        };
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1")
            .bind(second)
            .execute(&pool)
            .await?;
        assert_eq!(store.nudge_missing(second, session, Required::Report).await?, Nudge::Exhausted);
        let human: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, started_at, finished_at)
             VALUES ($1, $2, $3, 'followup', 'completed', 'please finish', NOW(), NOW()) RETURNING id",
        )
        .bind(session)
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert!(matches!(store.nudge_missing(human, session, Required::Report).await?, Nudge::Queued(_)), "a later miss gets its own reminders");
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE session_id = $1 AND status = 'queued'")
            .bind(session)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'failed' WHERE id = $1")
            .bind(session)
            .execute(&pool)
            .await?;
        let TaskResume::Resumed { session_id, run_id } = store.resume_task_session(agent, task, "Fea", Some("check the lock release")).await? else {
            panic!("expected the failed session to be resumed");
        };
        assert_eq!(session_id, session);
        let (kind, status, saved): (String, String, String) =
            sqlx::query_as("SELECT kind, status, prompt FROM session_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!((kind.as_str(), status.as_str()), ("followup", "queued"));
        assert!(saved.contains("check the lock release"));
        let asked = store.get_run(run_id).await?.expect("run").request;
        assert_eq!(asked, Some(serde_json::json!({ "from": "Fea", "text": "check the lock release" })));
        assert_eq!(store.get_session(session).await?.expect("session").status, "active");
        assert!(matches!(store.resume_task_session(agent, task, "Fea", Some("again")).await?, TaskResume::Busy { .. }));
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(sessions, 1);
        Ok(())
    }

    #[tokio::test]
    async fn steer_queue_delivers_direct_messages_into_running_folder_work() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Dee', 'claude', 'haiku') RETURNING id")
            .bind(format!("dee-{tag}"))
            .fetch_one(&pool)
            .await?;
        let session_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir, status, title)
             VALUES ($1, 'direct', 'claude', 'haiku', $2, 'active', 'site') RETURNING id",
        )
        .bind(agent)
        .bind(super::direct_key(Uuid::new_v4()))
        .fetch_one(&pool)
        .await?;
        let session = store.get_session(session_id).await?.expect("session");
        let first = store.start_followup(&session, "look at the header", "Ana", false).await?;
        assert_eq!(first.kind, "direct");
        sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1")
            .bind(first.id)
            .execute(&pool)
            .await?;
        assert!(store.pending_steers(session.id).await?.iter().all(|s| s.id != first.id));
        let second = store.start_followup(&session, "make it blue too", "Ana", true).await?;
        let steers = store.pending_steers(session.id).await?;
        assert_eq!(steers.len(), 1);
        assert_eq!(steers[0].id, second.id);
        assert!(steers[0].interrupt);
        assert!(steers[0].text.starts_with("make it blue too"));
        assert!(steers[0].text.contains("arrived while you were working"));
        assert!(!steers[0].text.contains("report block"));
        assert!(store.mark_steered(second.id, first.id).await?);
        assert!(store.pending_steers(session.id).await?.is_empty());
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(sessions, 1);
        Ok(())
    }

    #[tokio::test]
    async fn steer_queue_delivers_followups_into_running_work() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Kim', 'claude', 'haiku') RETURNING id")
            .bind(format!("kim-{tag}"))
            .fetch_one(&pool)
            .await?;
        let session_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', 'active') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let running: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at)
             VALUES ($1, $2, 'task', 'running', 'do it', NOW()) RETURNING id",
        )
        .bind(session_id)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let session = store.get_session(session_id).await?.expect("session");
        let followup = store.start_followup(&session, "stop polling dcgm", "Ana", true).await?;
        let steers = store.pending_steers(session_id).await?;
        assert_eq!(steers.len(), 1);
        assert!(steers[0].interrupt);
        assert!(steers[0].text.contains("stop polling dcgm"));
        assert!(steers[0].text.contains("arrived while you were working"));
        assert!(!steers[0].text.contains(super::FOLLOWUP_ASK));
        assert!(store.mark_steered(followup.id, running).await?);
        assert!(!store.mark_steered(followup.id, running).await?);
        assert!(store.pending_steers(session_id).await?.is_empty());
        let views = store.list_runs_for_session(session_id).await?;
        let view = views.iter().find(|v| v.run.id == followup.id).expect("followup view");
        assert_eq!(view.steered_into, Some(running));
        assert_eq!(view.run.status, "completed");
        assert_eq!(store.request_stop(running, "Ana").await?, Some(false));
        assert_eq!(store.stop_requested(running).await?.as_deref(), Some("Ana"));
        assert!(!store.stop_runner_run(running, "Stopped by Ana").await?);
        assert!(store.complete_task_run(session_id, chrono::Utc::now() - chrono::Duration::hours(1)).await?);

        let message: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'typo') RETURNING id",
        )
        .bind(agent)
        .bind(user)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', $2)")
            .bind(agent)
            .bind(serde_json::json!({ "message_id": message, "body": "typo" }))
            .execute(&pool)
            .await?;
        let payload = serde_json::json!({ "message_id": message, "body": "fixed" });
        let meta = serde_json::json!({ "attachments": [] });
        let replaced = store.replace_pending_message(agent, message, user, "fixed", &meta, &payload).await?;
        assert_eq!(replaced.map(|m| m.body), Some("fixed".to_string()));
        let queued: serde_json::Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(queued["body"], "fixed");
        sqlx::query("UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        assert!(store.replace_pending_message(agent, message, user, "again", &meta, &payload).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn followups_are_refused_only_while_a_review_is_really_running() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Rev', 'codex', 'gpt') RETURNING id")
            .bind(format!("rev-{tag}"))
            .fetch_one(&pool)
            .await?;
        for (review_status, refused) in [
            ("queued", true),
            ("running", true),
            ("completed", false),
            ("failed", false),
            ("timed_out", false),
            ("cancelled", false),
        ] {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'fix bmc', 'in_progress', $2) RETURNING id",
            )
            .bind(user)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
            let parent = Uuid::new_v4();
            insert_session(&pool, parent, agent, "task", "completed", Some(task), None, Some("cli-task"), None, None).await?;
            let review = Uuid::new_v4();
            insert_session(&pool, review, agent, "review", "active", Some(task), Some(parent), Some("cli-review"), None, None).await?;
            insert_run(&pool, review, agent, Some(task), "review", review_status, None).await?;
            let session = store.get_session(parent).await?.expect("task session");
            let sent = store.start_followup(&session, "one more thing", "Ana", false).await;
            if refused {
                let error = sent.expect_err(review_status).to_string();
                assert!(error.contains("being reviewed") && error.contains("stop the review run"), "{review_status}: {error}");
            } else {
                let run = sent.map_err(|e| anyhow::anyhow!("{review_status}: {e}"))?;
                assert_eq!(run.kind, "followup", "{review_status}");
                assert_eq!(run.status, "queued", "{review_status}");
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn followups_wait_for_a_publish_and_for_racing_attempts() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Pub', 'codex', 'gpt') RETURNING id")
            .bind(format!("pub-{tag}"))
            .fetch_one(&pool)
            .await?;
        for (publish_status, refused) in [("queued", true), ("running", true), ("completed", false), ("failed", false)] {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'ship it', 'in_progress', $2) RETURNING id",
            )
            .bind(user)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
            let parent = Uuid::new_v4();
            insert_session(&pool, parent, agent, "task", "active", Some(task), None, Some("cli-task"), None, None).await?;
            insert_run(&pool, parent, agent, Some(task), "publish", publish_status, None).await?;
            let session = store.get_session(parent).await?.expect("task session");
            let sent = store.start_followup(&session, "one more thing", "Ana", false).await;
            if refused {
                let error = sent.expect_err(publish_status).to_string();
                assert!(error.contains("being published"), "{publish_status}: {error}");
            } else {
                sent.map_err(|e| anyhow::anyhow!("{publish_status}: {e}"))?;
            }
        }
        for (sibling_state, refused) in [("running", true), ("won", false), ("lost", false), ("failed", false)] {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'race', 'in_progress', $2) RETURNING id",
            )
            .bind(user)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
            let finished = Uuid::new_v4();
            let sibling = Uuid::new_v4();
            insert_session(&pool, finished, agent, "task", "active", Some(task), None, Some("cli-a"), None, None).await?;
            insert_session(&pool, sibling, agent, "task", "active", Some(task), None, Some("cli-b"), None, None).await?;
            sqlx::query("UPDATE agent_sessions SET attempt = 1, attempt_state = 'finished' WHERE id = $1").bind(finished).execute(&pool).await?;
            sqlx::query("UPDATE agent_sessions SET attempt = 2, attempt_state = $2 WHERE id = $1").bind(sibling).bind(sibling_state).execute(&pool).await?;
            let session = store.get_session(finished).await?.expect("attempt session");
            let sent = store.start_followup(&session, "one more thing", "Ana", false).await;
            if refused {
                let error = sent.expect_err(sibling_state).to_string();
                assert!(error.contains("other attempts are still running"), "{sibling_state}: {error}");
            } else {
                sent.map_err(|e| anyhow::anyhow!("{sibling_state}: {e}"))?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_cancelled_review_puts_its_quest_back_in_review() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Cut', 'codex', 'gpt') RETURNING id")
            .bind(format!("cut-{tag}"))
            .fetch_one(&pool)
            .await?;
        let quest = |status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'review me', $3, $2) RETURNING id",
                )
                .bind(user)
                .bind(agent)
                .bind(status)
                .fetch_one(&pool)
                .await
            }
        };
        let cases: [(&str, Option<serde_json::Value>, &str, bool); 7] = [
            ("cancelled", None, "in_progress", true),
            ("cancelled", Some(serde_json::json!({ "manual": true })), "in_progress", false),
            ("cancelled", Some(serde_json::json!({ "compare": [Uuid::new_v4()] })), "in_progress", false),
            ("failed", None, "in_progress", false),
            ("timed_out", None, "in_progress", false),
            ("cancelled", None, "done", false),
            ("cancelled", None, "cancelled", false),
        ];
        let mut expected = Vec::new();
        for (run_status, asked_by, quest_status, moves) in cases {
            let task = quest(quest_status).await?;
            let parent = Uuid::new_v4();
            let review = Uuid::new_v4();
            insert_session(&pool, parent, agent, "task", "active", Some(task), None, Some("cli-task"), None, None).await?;
            insert_session(&pool, review, agent, "review", "active", Some(task), Some(parent), Some("cli-review"), None, asked_by).await?;
            let run = insert_run(&pool, review, agent, Some(task), "review", run_status, None).await?;
            sqlx::query("UPDATE session_runs SET error = 'Organization suspended' WHERE id = $1").bind(run).execute(&pool).await?;
            expected.push((task, quest_status, moves));
        }
        let merged = quest("in_progress").await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'done')").bind(merged).execute(&pool).await?;
        let merged_parent = Uuid::new_v4();
        let merged_review = Uuid::new_v4();
        insert_session(&pool, merged_parent, agent, "task", "active", Some(merged), None, Some("cli-task"), None, None).await?;
        insert_session(&pool, merged_review, agent, "review", "active", Some(merged), Some(merged_parent), Some("cli-review"), None, None).await?;
        insert_run(&pool, merged_review, agent, Some(merged), "review", "cancelled", None).await?;
        expected.push((merged, "in_progress", false));
        let busy = quest("in_progress").await?;
        let busy_parent = Uuid::new_v4();
        let busy_review = Uuid::new_v4();
        insert_session(&pool, busy_parent, agent, "task", "active", Some(busy), None, Some("cli-task"), None, None).await?;
        insert_session(&pool, busy_review, agent, "review", "active", Some(busy), Some(busy_parent), Some("cli-review"), None, None).await?;
        insert_run(&pool, busy_review, agent, Some(busy), "review", "cancelled", None).await?;
        insert_run(&pool, busy_parent, agent, Some(busy), "followup", "queued", None).await?;
        expected.push((busy, "in_progress", false));

        let settled = store.settle_cancelled_reviews().await?;
        for (task, before, moves) in expected {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!(status, if moves { "review" } else { before }, "{before}");
            assert_eq!(settled.contains(&task), moves, "{before}");
            let events: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE task_id = $1 AND type = 'review.finished'")
                .bind(task)
                .fetch_all(&pool)
                .await?;
            assert_eq!(events.len(), usize::from(moves), "{before}");
            if moves {
                assert_eq!(events[0]["cancelled"], true);
                assert_eq!(events[0]["review"]["verdict"], "review cancelled");
                assert_eq!(events[0]["review"]["summary"], "Organization suspended");
                let woken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'review.finished' AND payload->>'task_id' = $2")
                    .bind(agent)
                    .bind(task.to_string())
                    .fetch_one(&pool)
                    .await?;
                assert_eq!(woken, 1);
            }
        }
        assert!(store.settle_cancelled_reviews().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn quests_with_a_merged_pr_settle_as_done_unless_work_is_running() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("merged-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Mer', 'codex', 'gpt') RETURNING id")
            .bind(format!("mer-{tag}"))
            .fetch_one(&pool)
            .await?;
        let cases = [
            ("review", Some("https://github.com/o/r/pull/1"), "done", None, "done"),
            ("in_progress", Some("https://github.com/o/r/pull/2"), "done", None, "done"),
            ("review", Some("https://github.com/o/r/pull/3"), "active", None, "review"),
            ("review", None, "done", None, "review"),
            ("failed", Some("https://github.com/o/r/pull/5"), "done", None, "failed"),
            ("cancelled", Some("https://github.com/o/r/pull/6"), "done", None, "cancelled"),
            ("in_progress", Some("https://github.com/o/r/pull/7"), "done", Some("running"), "in_progress"),
            ("review", Some("https://github.com/o/r/pull/8"), "done", Some("completed"), "done"),
        ];
        let mut tasks = Vec::new();
        for (status, pr, merge, run, expected) in cases {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url) VALUES ($1, 'ship it', $2, $3, $4) RETURNING id",
            )
            .bind(user)
            .bind(status)
            .bind(agent)
            .bind(pr)
            .fetch_one(&pool)
            .await?;
            sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(task).execute(&pool).await?;
            sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', $2)").bind(task).bind(merge).execute(&pool).await?;
            sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'tests', 'work', 'pending')").bind(task).execute(&pool).await?;
            if let Some(run) = run {
                let session = Uuid::new_v4();
                insert_session(&pool, session, agent, "task", "active", Some(task), None, Some("cli-task"), None, None).await?;
                insert_run(&pool, session, agent, Some(task), "followup", run, None).await?;
            }
            tasks.push((task, expected));
        }
        let settled = store.settle_merged_quests().await?;
        for (task, expected) in tasks {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!(status, expected);
            assert_eq!(settled.contains(&task), expected == "done");
            let open_work: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_milestones WHERE task_id = $1 AND kind = 'work' AND status = 'pending'")
                .bind(task)
                .fetch_one(&pool)
                .await?;
            assert_eq!(open_work, i64::from(!settled.contains(&task)));
        }
        assert!(store.settle_merged_quests().await?.iter().all(|id| !settled.contains(id)));
        Ok(())
    }

    #[tokio::test]
    async fn agents_and_calls_read_activity_without_quiet_notes() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Feed', 'claude', 'sonnet') RETURNING id")
            .bind(format!("feed-{tag}"))
            .fetch_one(&pool)
            .await?;
        for (kind, quiet) in [("task.finished", false), ("task.changed", true), ("run.retrying", true), ("message.created", false)] {
            let payload = if quiet { json!({ "quiet": true, "tag": tag }) } else { json!({ "tag": tag }) };
            store.emit(NewEvent::new(kind).agent(agent).payload(payload), None).await?;
        }
        let kinds = |events: Vec<super::WorldEvent>| events.into_iter().map(|e| e.event_type).collect::<Vec<_>>();
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &[], 10).await?), vec!["task.finished", "message.created"]);
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &["task.".to_string()], 10).await?), vec!["task.finished"]);
        assert_eq!(store.recent_events(Some(agent), 10).await?.len(), 4, "the raw reader behind the live stream still sees everything");
        Ok(())
    }

    #[tokio::test]
    async fn every_change_a_quest_card_shows_announces_itself() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("status-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Pace', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("pace-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Stuck drawer', 'failed', $2) RETURNING id")
            .bind(user)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let changes = || {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Option<Uuid>, Value)>("SELECT agent_id, payload FROM world_events WHERE task_id = $1 AND type = 'task.changed' ORDER BY seq")
                    .bind(quest)
                    .fetch_all(&pool)
                    .await
            }
        };
        let card = |status: &str, changed: &str| json!({ "task_id": quest, "status": status, "title": "Stuck drawer", "changed": changed, "quiet": true });
        assert_eq!(changes().await?, vec![(Some(agent), card("failed", "created"))], "a new quest shows up on boards");

        let session = Uuid::new_v4();
        insert_session(&pool, session, agent, "task", "failed", Some(quest), None, Some("cli-status"), None, None).await?;
        let failed = store.get_session(session).await?.expect("session");
        store.start_followup(&failed, "Try the drawer again", "Ana", false).await?;
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card("in_progress", "status"))), "a follow-up that reopens a failed quest tells every open card");

        let seen = changes().await?.len();
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress', title = 'Stuck drawer' WHERE id = $1").bind(quest).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET error_message = 'noise' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert_eq!(changes().await?.len(), seen, "writes that change nothing a card shows say nothing");

        sqlx::query("UPDATE agent_tasks SET pr_url = 'https://github.com/acme/app/pull/7' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert_eq!(changes().await?.last().map(|c| c.1["changed"].clone()), Some(json!("details")), "a PR link on the card");

        sqlx::query("UPDATE agent_sessions SET plan_state = 'awaiting' WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(changes().await?.last().map(|c| c.1["changed"].clone()), Some(json!("plan")), "Needs you appears");
        let before = changes().await?.len();
        sqlx::query("UPDATE agent_sessions SET status = 'archived' WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1, "archiving the waiting session clears Needs you");

        let milestone: Uuid = sqlx::query_scalar("INSERT INTO task_milestones (task_id, title) VALUES ($1, 'Write the fix') RETURNING id")
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        assert_eq!(changes().await?.last().map(|c| c.1["changed"].clone()), Some(json!("milestones")));
        let before = changes().await?.len();
        sqlx::query("UPDATE task_milestones SET next_check_at = NOW(), data = '{\"sha\": \"abc\"}' WHERE id = $1").bind(milestone).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before, "merge watch bookkeeping is not a card change");
        sqlx::query("UPDATE task_milestones SET status = 'done' WHERE id = $1").bind(milestone).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1, "a ticked milestone is");

        let before = changes().await?.len();
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1").bind(quest).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title) VALUES ($1, 'Review'), ($1, 'Ship')").bind(quest).execute(&mut *tx).await?;
        tx.commit().await?;
        assert_eq!(changes().await?.len(), before + 1, "one change per quest per transaction");

        let bell: i64 = sqlx::query_scalar("SELECT count(*) FROM world_notifications n JOIN world_events e ON e.seq = n.event_seq WHERE e.task_id = $1 AND e.type = 'task.changed'")
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        assert_eq!(bell, 0, "card changes are quiet");
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_work_runs_fail_their_quest_so_it_can_be_resumed() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let new_agent = |name: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Cut', 'codex', 'gpt') RETURNING id")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
            }
        };
        let agent = new_agent(format!("cut-{tag}")).await?;
        let other = new_agent(format!("old-{tag}")).await?;
        let quest = |status: &'static str, assignee: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'stuck', $3, $2) RETURNING id",
                )
                .bind(user)
                .bind(assignee)
                .bind(status)
                .fetch_one(&pool)
                .await
            }
        };
        struct Case {
            name: &'static str,
            kind: &'static str,
            asked_by: Option<serde_json::Value>,
            quest_status: &'static str,
            run_status: &'static str,
            handed_off: bool,
            session_status: &'static str,
            later: Option<&'static str>,
            moves: bool,
        }
        let compare = Some(serde_json::json!({ "compare": [Uuid::new_v4()] }));
        let case = |name, kind| Case {
            name,
            kind,
            asked_by: None,
            quest_status: "in_progress",
            run_status: "cancelled",
            handed_off: false,
            session_status: "active",
            later: None,
            moves: true,
        };
        let cases = [
            case("task", "task"),
            case("followup", "followup"),
            case("publish", "publish"),
            Case { asked_by: compare, ..case("compare review", "review") },
            Case { moves: false, ..case("automatic review", "review") },
            Case { asked_by: Some(serde_json::json!({ "manual": true })), moves: false, ..case("manual review", "review") },
            Case { run_status: "failed", moves: false, ..case("failed run", "task") },
            Case { run_status: "timed_out", moves: false, ..case("timed out run", "followup") },
            Case { quest_status: "done", moves: false, ..case("done quest", "task") },
            Case { quest_status: "cancelled", moves: false, ..case("cancelled quest", "task") },
            Case { later: Some("queued"), moves: false, ..case("newer queued run", "task") },
            Case { later: Some("running"), moves: false, ..case("newer running run", "task") },
            Case { later: Some("completed"), moves: false, ..case("newer finished run", "task") },
            Case { handed_off: true, moves: false, ..case("handed off", "task") },
            Case { session_status: "archived", moves: false, ..case("archived session", "task") },
        ];
        let mut expected = Vec::new();
        for (index, case) in cases.into_iter().enumerate() {
            let agent = new_agent(format!("case-{index}-{tag}")).await?;
            let task = quest(case.quest_status, agent).await?;
            let session = Uuid::new_v4();
            let parent = Uuid::new_v4();
            let is_review = case.kind == "review";
            insert_session(&pool, parent, agent, "task", "active", Some(task), None, Some("cli-task"), None, None).await?;
            insert_session(
                &pool,
                session,
                agent,
                if is_review { "review" } else { "task" },
                case.session_status,
                Some(task),
                is_review.then_some(parent),
                Some("cli-run"),
                None,
                case.asked_by.clone(),
            )
            .await?;
            let run = insert_run(&pool, session, if case.handed_off { other } else { agent }, Some(task), case.kind, case.run_status, None).await?;
            sqlx::query("UPDATE session_runs SET error = 'Organization suspended', created_at = NOW() - INTERVAL '1 minute' WHERE id = $1")
                .bind(run)
                .execute(&pool)
                .await?;
            if let Some(later) = case.later {
                insert_run(&pool, parent, agent, Some(task), "followup", later, None).await?;
            }
            expected.push((task, agent, case.name, case.quest_status, case.moves, run));
        }
        let racing_other = quest("in_progress", agent).await?;
        let racing_session = Uuid::new_v4();
        let racing_sibling = Uuid::new_v4();
        insert_session(&pool, racing_session, agent, "task", "active", Some(racing_other), None, Some("cli-a"), None, None).await?;
        insert_session(&pool, racing_sibling, agent, "task", "active", Some(racing_other), None, Some("cli-b"), None, None).await?;
        insert_run(&pool, racing_session, agent, Some(racing_other), "task", "cancelled", None).await?;
        insert_run(&pool, racing_sibling, agent, Some(racing_other), "task", "running", None).await?;
        expected.push((racing_other, agent, "sibling attempt still running", "in_progress", false, Uuid::nil()));

        let settled = store.settle_cancelled_work().await?;
        for (task, agent, name, before, moves, run) in expected.clone() {
            let (status, error): (String, Option<String>) = sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
                .bind(task)
                .fetch_one(&pool)
                .await?;
            assert_eq!(status, if moves { "failed" } else { before }, "{name}");
            assert_eq!(settled.contains(&task), moves, "{name}");
            let events: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE task_id = $1 AND type = 'run.failed'")
                .bind(task)
                .fetch_all(&pool)
                .await?;
            assert_eq!(events.len(), usize::from(moves), "{name}");
            let announced: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE task_id = $1 AND type = 'task.changed' AND payload->>'changed' = 'status'")
                .bind(task)
                .fetch_all(&pool)
                .await?;
            assert_eq!(
                announced,
                if moves { vec![serde_json::json!({ "task_id": task, "status": "failed", "title": "stuck", "changed": "status", "quiet": true })] } else { Vec::new() },
                "{name}: a quest that turns failed tells every open card"
            );
            if moves {
                assert!(error.as_deref().is_some_and(|e| e.contains("was cancelled") && e.contains("Organization suspended")), "{name}");
                assert_eq!(events[0]["cause"], "cancelled", "{name}");
                assert_eq!(events[0]["task_id"], task.to_string(), "{name}");
                let cause: Option<String> = sqlx::query_scalar("SELECT cause FROM session_runs WHERE id = $1").bind(run).fetch_one(&pool).await?;
                assert_eq!(cause.as_deref(), Some("cancelled"), "{name}");
                let woken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'run.failed' AND payload->>'task_id' = $2")
                    .bind(agent)
                    .bind(task.to_string())
                    .fetch_one(&pool)
                    .await?;
                assert_eq!(woken, 1, "{name}");
            }
        }
        let again = store.settle_cancelled_work().await?;
        assert!(again.is_empty(), "{again:?}");
        let (resume_task, resume_agent) = expected.iter().find(|row| row.4).map(|row| (row.0, row.1)).expect("a quest moved");
        let resumed = store.resume_task_session(resume_agent, resume_task, "Fea", Some("pick it back up")).await?;
        assert!(matches!(resumed, TaskResume::Resumed { .. } | TaskResume::Fresh));
        Ok(())
    }

    #[tokio::test]
    async fn a_quest_counts_as_merged_only_with_a_pr_and_a_done_merge() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let pr = "https://github.com/acme/app/pull/7";
        let cases = [
            ("pr and done quest", "done", Some(pr), None, true),
            ("pr and merged milestone", "review", Some(pr), Some("done"), true),
            ("pr and active milestone", "review", Some(pr), Some("active"), false),
            ("pr and failed milestone", "review", Some(pr), Some("failed"), false),
            ("pr and no milestone", "in_progress", Some(pr), None, false),
            ("done without a pr", "done", None, None, false),
            ("merged milestone without a pr", "review", None, Some("done"), false),
            ("cancelled quest", "cancelled", Some(pr), Some("done"), false),
        ];
        for (name, status, pr_url, milestone, merged) in cases {
            let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status, pr_url) VALUES ($1, 'quest', $2, $3) RETURNING id")
                .bind(user)
                .bind(status)
                .bind(pr_url)
                .fetch_one(&pool)
                .await?;
            if let Some(milestone) = milestone {
                sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', $2)")
                    .bind(task)
                    .bind(milestone)
                    .execute(&pool)
                    .await?;
            }
            let found = store.quest_merged_pr(task).await?;
            assert_eq!(found.is_some(), merged, "{name}");
            if merged {
                assert_eq!(found.as_deref(), Some(pr), "{name}");
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_session(
        pool: &sqlx::PgPool,
        id: Uuid,
        agent: Uuid,
        role: &str,
        status: &str,
        task: Option<Uuid>,
        parent: Option<Uuid>,
        cli_session_id: Option<&str>,
        working_dir: Option<&str>,
        asked_by: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO agent_sessions
               (id, agent_id, role, backend, model, cli_session_id, parent_session_id, task_id, working_dir, status, asked_by)
             VALUES ($1, $2, $3, 'codex', 'test', $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(agent)
        .bind(role)
        .bind(cli_session_id)
        .bind(parent)
        .bind(task)
        .bind(working_dir)
        .bind(status)
        .bind(asked_by)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_run(
        pool: &sqlx::PgPool,
        session: Uuid,
        agent: Uuid,
        task: Option<Uuid>,
        kind: &str,
        status: &str,
        fork_from: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        Ok(sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt)
             VALUES ($1, $2, $3, $4, $5, $6, 'original prompt') RETURNING id",
        )
        .bind(session)
        .bind(agent)
        .bind(task)
        .bind(kind)
        .bind(status)
        .bind(fork_from)
        .fetch_one(pool)
        .await?)
    }

    #[tokio::test]
    async fn handle_renames_take_over_and_keep_aliases() -> anyhow::Result<()> {
        use super::{HandleRename, RenameHandleError};
        let Ok(url) = std::env::var("HANDLE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mk = |prefix: &str| format!("{prefix}-{tag}");
        let agent_a: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Ha', 'claude', 'haiku') RETURNING id")
            .bind(mk("ha"))
            .fetch_one(&pool)
            .await?;
        let agent_b: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Bee', 'claude', 'haiku') RETURNING id")
            .bind(mk("hb"))
            .fetch_one(&pool)
            .await?;
        for raw in ["--bad--", "-x", "x-", "", &"a".repeat(65)] {
            let raw: &str = raw;
            assert!(matches!(store.rename_handle(agent_a, raw).await, Err(RenameHandleError::Invalid)), "expected Invalid for {raw:?}");
        }
        assert!(matches!(store.rename_handle(agent_a, &mk("ha")).await, Ok(HandleRename::Unchanged)));
        assert!(matches!(store.rename_handle(agent_a, &mk("hb")).await, Err(RenameHandleError::Taken(_))));
        let renamed = store.rename_handle(agent_a, &mk("HR")).await?;
        assert!(matches!(&renamed, HandleRename::Renamed { from, to } if from == &mk("ha") && to == &mk("hr")));
        let by_alias = store.find_agent(&mk("ha")).await?.expect("old handle resolves through the alias");
        assert_eq!(by_alias.id, agent_a);
        assert_eq!(by_alias.slug, mk("hr"));
        let by_new = store.find_agent(&mk("hr")).await?.expect("new handle resolves");
        assert_eq!(by_new.id, agent_a);
        assert!(matches!(store.rename_handle(agent_b, &mk("hr")).await, Err(RenameHandleError::Taken(_))));
        assert!(matches!(store.rename_handle(agent_b, &mk("ha")).await, Err(RenameHandleError::Taken(_))), "another agent's alias must block the takeover");
        let back = store.rename_handle(agent_a, &mk("ha")).await?;
        assert!(matches!(&back, HandleRename::Renamed { from, to } if from == &mk("hr") && to == &mk("ha")));
        let reclaimed: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_slug_aliases WHERE lower(slug) = lower($1) AND agent_id = $2)")
            .bind(mk("ha"))
            .bind(agent_a)
            .fetch_one(&pool)
            .await?;
        assert!(!reclaimed, "reclaiming the old handle drops its own alias row");
        assert_eq!(store.find_agent(&mk("hr")).await?.expect("previous handle now resolves through its alias").id, agent_a);
        let conflict = store.find_agent(&mk("hb")).await?.expect("agent b");
        sqlx::query("INSERT INTO agent_slug_aliases (slug, agent_id) VALUES ($1, $2)")
            .bind(mk("hb"))
            .bind(agent_a)
            .execute(&pool)
            .await?;
        assert_eq!(store.find_agent(&mk("hb")).await?.expect("lookup works").id, conflict.id, "a live slug always beats an alias");
        let dashless = format!("hb{tag}");
        match store.rename_handle(agent_a, &dashless).await {
            Err(RenameHandleError::Taken(h)) => assert_eq!(RenameHandleError::Taken(h).to_string(), format!("@{dashless} is taken")),
            other => panic!("dash variants share the mention key, got {other:?}"),
        }
        assert!(matches!(store.rename_handle(agent_a, "here").await, Err(RenameHandleError::Taken(_))));
        let person: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, $2) RETURNING id")
            .bind(format!("person-{tag}@test.dev"))
            .bind(format!("Pat {tag}"))
            .fetch_one(&pool)
            .await?;
        assert!(matches!(store.rename_handle(agent_a, &format!("pat-{tag}")).await, Err(RenameHandleError::Taken(_))), "a person's name is in the mention namespace");
        assert!(matches!(store.rename_handle(agent_a, &format!("person-{tag}")).await, Err(RenameHandleError::Taken(_))), "so is the email local part");
        let raw = sqlx::query("UPDATE agent_profiles SET slug = $2 WHERE id = $1").bind(agent_b).bind(mk("HR")).execute(&pool).await;
        assert!(raw.is_err_and(|e| e.to_string().contains("is taken")), "SQL rejects an alias takeover even without the Rust check");
        let race = mk("race");
        let (first, second) = tokio::join!(store.rename_handle(agent_a, &race), store.rename_handle(agent_b, &race));
        assert_eq!(
            [first.is_ok(), second.is_ok()].iter().filter(|ok| **ok).count(),
            1,
            "only one concurrent rename wins: {first:?} {second:?}"
        );
        assert!(matches!(first, Ok(_) | Err(RenameHandleError::Taken(_))) && matches!(second, Ok(_) | Err(RenameHandleError::Taken(_))));
        sqlx::query("DELETE FROM users WHERE id = $1").bind(person).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id IN ($1, $2)").bind(agent_a).bind(agent_b).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_person_name_resolves_to_their_main_agent() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("HANDLE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let slug = format!("prya-{tag}");
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Mainly', 'claude', 'haiku') RETURNING id")
            .bind(&slug)
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Side', 'claude', 'haiku') RETURNING id")
            .bind(format!("side-{tag}"))
            .fetch_one(&pool)
            .await?;
        let person: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, $2) RETURNING id")
            .bind(format!("abhi-{tag}@test.dev"))
            .bind(format!("Abhi {tag}"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, person, Some(main)).await?;
        let by_person = store.find_agent(&format!("abhi {tag}")).await?.expect("person name resolves to the main agent");
        assert_eq!(by_person.id, main);
        assert_eq!(store.find_agent(&format!("@Abhi {tag}")).await?.expect("with an at sign").id, main);
        assert_eq!(store.find_agent(&slug).await?.expect("handle still resolves").id, main);
        assert_eq!(store.find_agent(&format!("@{slug}")).await?.expect("handle with an at sign").id, main);
        assert_eq!(store.find_agent(&format!("@{}", slug.to_uppercase())).await?.expect("at sign and case").id, main);
        let twin: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, $2) RETURNING id")
            .bind(format!("abhi-twin-{tag}@test.dev"))
            .bind(format!("ABHI {tag}"))
            .fetch_one(&pool)
            .await?;
        let twin_main: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Twin', 'claude', 'haiku') RETURNING id")
            .bind(format!("twin-{tag}"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, twin, Some(twin_main)).await?;
        assert!(store.find_agent(&format!("abhi {tag}")).await?.is_none(), "two people with the same name resolve to nobody");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(twin).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(twin_main).execute(&pool).await?;
        assert_eq!(store.find_agent(&format!("abhi {tag}")).await?.expect("unique again").id, main);
        assert_eq!(store.find_agent("Side").await?.map(|a| a.id), Some(other));
        assert!(store.find_agent(&format!("nobody-{tag}")).await?.is_none());
        sqlx::query("DELETE FROM users WHERE id = $1").bind(person).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id IN ($1, $2)").bind(main).bind(other).execute(&pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    #[tokio::test]
    async fn heartbeat_is_one_per_agent_and_skips_while_pending() -> Result<()> {
        let Ok(url) = std::env::var("HEARTBEAT_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("hb-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Beat', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("beat-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let first = store.create_agent_schedule(user, agent, "Heartbeat", "- check A", Some("0 * * * *"), None, "heartbeat").await?;
        let second = store.create_agent_schedule(user, agent, "Heartbeat", "- check B", Some("*/30 * * * *"), None, "heartbeat").await?;
        assert_eq!(first.id, second.id);
        assert_eq!(second.prompt, "- check B");
        assert!(second.is_heartbeat());
        store.create_agent_schedule(user, agent, "Standup", "say hi", Some("0 9 * * *"), None, "routine").await?;
        store.create_agent_schedule(user, agent, "Standup 2", "say hi", Some("0 9 * * *"), None, "routine").await?;
        let list = store.list_agent_schedules(agent).await?;
        assert_eq!(list.iter().filter(|s| s.is_heartbeat()).count(), 1);
        assert_eq!(list.len(), 3);

        assert!(store.fire_schedule(&second).await?);
        assert!(!store.fire_schedule(&second).await?);
        let routine = list.iter().find(|s| s.name == "Standup").unwrap();
        assert!(store.fire_schedule(routine).await?);
        assert!(!store.fire_schedule(routine).await?);
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1 AND payload->>'schedule_id' = $2")
            .bind(agent)
            .bind(routine.id.to_string())
            .execute(&pool)
            .await?;
        let (heartbeat, quiet): (Option<bool>, Option<bool>) = sqlx::query_as(
            "SELECT (payload->>'heartbeat')::bool, (payload->>'quiet')::bool FROM agent_inbox WHERE agent_id = $1 AND status = 'pending'",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!((heartbeat, quiet), (Some(true), Some(true)));
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        assert!(store.fire_schedule(&second).await?);

        assert!(store.update_agent_schedule(agent, second.id, Some("- check C"), None).await?);
        let after = store.list_agent_schedules(agent).await?.into_iter().find(|s| s.id == second.id).unwrap();
        assert_eq!(after.prompt, "- check C");
        assert_eq!(after.cron_expression.as_deref(), Some("*/30 * * * *"));
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn follow_up_schedules_end_with_their_quest_or_expiry() -> Result<()> {
        let Ok(url) = std::env::var("HEARTBEAT_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("fu-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Lead', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("lead-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Hotfix', 'in_progress') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let tied = store.create_follow_up_schedule(user, agent, "Hotfix check", "check the hotfix", "*/10 * * * *", Some(quest), None).await?;
        assert_eq!(tied.task_id, Some(quest));
        let hours = (tied.expires_at.unwrap() - Utc::now()).num_minutes();
        assert!((FOLLOW_UP_HOURS_DEFAULT * 60 - 2..=FOLLOW_UP_HOURS_DEFAULT * 60).contains(&hours), "{hours}");
        let lone = store.create_follow_up_schedule(user, agent, "Short check", "look again", "*/15 * * * *", None, Some(1000)).await?;
        assert!(lone.expires_at.unwrap() < Utc::now() + chrono::Duration::hours(FOLLOW_UP_HOURS_MAX) + chrono::Duration::minutes(1));
        store.create_agent_schedule(user, agent, "Standup", "say hi", Some("0 9 * * *"), None, "routine").await?;
        assert_eq!(store.list_agent_schedules(agent).await?.len(), 3);

        sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert_eq!(store.list_agent_schedules(agent).await?.len(), 3);
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(quest).execute(&pool).await?;
        let names: Vec<String> = store.list_agent_schedules(agent).await?.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Short check".to_string(), "Standup".to_string()]);

        sqlx::query("UPDATE schedules SET expires_at = NOW() - INTERVAL '1 minute' WHERE id = $1").bind(lone.id).execute(&pool).await?;
        assert_eq!(store.list_agent_schedules(agent).await?.len(), 1);
        store.drop_expired_schedules().await?;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedules WHERE agent_id = $1").bind(agent).fetch_one(&pool).await?;
        assert_eq!(left, 1);
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(quest).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn partition_reset_items_splits_and_keeps_order() {
        let inbox = |id: i64, kind: &str| InboxItem {
            id,
            agent_id: Uuid::new_v4(),
            kind: kind.to_string(),
            event_seq: None,
            payload: json!({}),
            status: "pending".to_string(),
            attempts: 0,
            created_at: Utc::now(),
        };
        let (reset, rest) = partition_reset_items(vec![
            inbox(1, "message"),
            inbox(2, RESET_INBOX_KIND),
            inbox(3, "task.assigned"),
            inbox(4, RESET_INBOX_KIND),
        ]);
        assert_eq!(reset.iter().map(|i| i.id).collect::<Vec<_>>(), [2, 4]);
        assert_eq!(rest.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 3]);
    }

    #[tokio::test]
    async fn context_reset_flow() -> anyhow::Result<()> {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return Ok(());
        };
        let admin = PgPoolOptions::new().max_connections(1).connect(&database_url).await?;
        let schema = format!("world_reset_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await?;
        let options = PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new().max_connections(8).connect_with(options).await?;
        sqlx::raw_sql(
            "CREATE TABLE agent_profiles (id UUID PRIMARY KEY, owner_user_id UUID);
             CREATE TABLE agent_inbox (
               id BIGSERIAL PRIMARY KEY, agent_id UUID NOT NULL, kind TEXT NOT NULL, event_seq BIGINT,
               payload JSONB NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'pending', attempts INTEGER DEFAULT 0,
               guest BOOLEAN NOT NULL DEFAULT false, retry_at TIMESTAMPTZ, done_at TIMESTAMPTZ, error TEXT,
               created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
             );
             CREATE TABLE agent_sessions (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), agent_id UUID NOT NULL, role TEXT NOT NULL, backend TEXT NOT NULL,
               model TEXT NOT NULL, cli_session_id TEXT, parent_session_id UUID, task_id UUID,
               working_dir TEXT, status TEXT NOT NULL DEFAULT 'active', usage_baseline JSONB NOT NULL DEFAULT '{}',
               context_tokens BIGINT NOT NULL DEFAULT 0, review_rounds INTEGER NOT NULL DEFAULT 0,
               summary TEXT, rules_hash TEXT, asked_by JSONB, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
               updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
             );
             CREATE UNIQUE INDEX one_active_main ON agent_sessions (agent_id) WHERE role = 'main' AND status = 'active';
             CREATE TABLE world_skills (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), name TEXT NOT NULL, description TEXT NOT NULL,
               body TEXT NOT NULL, source_url TEXT, scope TEXT NOT NULL, enabled BOOLEAN NOT NULL DEFAULT true,
               created_by UUID, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
             );
             CREATE TABLE agent_skills (agent_id UUID NOT NULL, skill_id UUID NOT NULL, PRIMARY KEY (agent_id, skill_id));
             CREATE TABLE agent_messages (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), agent_id UUID NOT NULL, session_id UUID,
               run_id UUID, author_kind TEXT NOT NULL, author_user_id UUID, author_agent_id UUID,
               body TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), edited_at TIMESTAMPTZ,
               meta JSONB NOT NULL DEFAULT '{}', reply_to UUID
             );
             CREATE TABLE session_runs (
               id UUID PRIMARY KEY DEFAULT gen_random_uuid(), session_id UUID NOT NULL, agent_id UUID NOT NULL,
               task_id UUID, kind TEXT NOT NULL, status TEXT NOT NULL, fork_from UUID, prompt TEXT NOT NULL,
               output TEXT, error TEXT, timeout_secs INTEGER NOT NULL DEFAULT 3600,
               cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0, input_tokens BIGINT NOT NULL DEFAULT 0,
               cached_input_tokens BIGINT NOT NULL DEFAULT 0, output_tokens BIGINT NOT NULL DEFAULT 0,
               claimed_by TEXT, heartbeat_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
               started_at TIMESTAMPTZ, finished_at TIMESTAMPTZ, request JSONB
             );
             CREATE TABLE runner_run_inbox (run_id UUID NOT NULL, inbox_id BIGINT NOT NULL, PRIMARY KEY (run_id, inbox_id));",
        )
        .execute(&pool)
        .await?;
        let agent = Uuid::new_v4();
        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_profiles (id, owner_user_id) VALUES ($1, $2)").bind(agent).bind(owner).execute(&pool).await?;
        let store = WorldStore::new(pool.clone());

        assert!(store.request_context_reset(agent, json!({"origin": "test"})).await?);
        assert!(!store.request_context_reset(agent, json!({"origin": "test"})).await?);

        let items = store.take_pending_inbox(agent, 20).await?;
        assert_eq!(items.len(), 1);
        assert!(is_reset_item(&items[0]));
        assert_eq!(items[0].attempts, 1);
        store.repend_inbox(&[items[0].id]).await?;
        let again = store.take_pending_inbox(agent, 20).await?;
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].attempts, 1);
        store.finish_inbox(&[again[0].id], "done", None).await?;
        assert!(store.request_context_reset(agent, json!({"origin": "test"})).await?);
        let pending: Vec<i64> = store.take_pending_inbox(agent, 20).await?.iter().map(|i| i.id).collect();
        store.finish_inbox(&pending, "done", None).await?;

        assert!(!store.notes_updated_since(agent, Some(owner), Utc::now() - chrono::Duration::minutes(30)).await?);
        let skill = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO world_skills (name, description, body, scope, created_by, updated_at)
             VALUES ('atlas-notes', 'notes', 'old', 'agent', $1, NOW() - INTERVAL '1 hour') RETURNING id",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO agent_skills (agent_id, skill_id) VALUES ($1, $2)")
            .bind(agent)
            .bind(skill)
            .execute(&pool)
            .await?;
        assert!(!store.notes_updated_since(agent, Some(owner), Utc::now() - chrono::Duration::minutes(30)).await?);
        sqlx::query("UPDATE world_skills SET updated_at = NOW(), body = 'fresh' WHERE id = $1")
            .bind(skill)
            .execute(&pool)
            .await?;
        assert!(store.notes_updated_since(agent, Some(owner), Utc::now() - chrono::Duration::minutes(30)).await?);
        assert!(!store.notes_updated_since(agent, Some(owner), Utc::now() + chrono::Duration::seconds(30)).await?);

        let old_main = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions (id, agent_id, role, backend, model, cli_session_id, working_dir, status)
             VALUES ($1, $2, 'main', 'claude', 'sonnet', 'cli-old', 'agents/x', 'active')",
        )
        .bind(old_main)
        .bind(agent)
        .execute(&pool)
        .await?;
        let fresh = store.restart_main_session(agent, "claude", "sonnet", "the notes body").await?;
        assert_eq!(fresh.summary.as_deref(), Some("the notes body"));
        assert_eq!(fresh.cli_session_id, None);
        assert_eq!(fresh.working_dir.as_deref(), Some("agents/x"));
        let (old_status, active_count): (String, i64) = sqlx::query_as(
            "SELECT (SELECT status FROM agent_sessions WHERE id = $1), (SELECT COUNT(*) FROM agent_sessions WHERE agent_id = $2 AND role = 'main' AND status = 'active')",
        )
        .bind(old_main)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(old_status, "archived");
        assert_eq!(active_count, 1);

        let stale_item = sqlx::query_scalar::<_, i64>(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts) VALUES ($1, 'message', '{}', 'processing', 1) RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let stale_run = Uuid::new_v4();
        sqlx::query("INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt) VALUES ($1, $2, $3, 'turn', 'queued', 'hi')")
            .bind(stale_run)
            .bind(old_main)
            .bind(agent)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2)")
            .bind(stale_run)
            .bind(stale_item)
            .execute(&pool)
            .await?;
        store.cancel_queued_turns(old_main).await?;
        let (run_status, item_status, item_attempts): (String, String, i32) = sqlx::query_as(
            "SELECT (SELECT status FROM session_runs WHERE id = $1), (SELECT status FROM agent_inbox WHERE id = $2), (SELECT attempts FROM agent_inbox WHERE id = $2)",
        )
        .bind(stale_run)
        .bind(stale_item)
        .fetch_one(&pool)
        .await?;
        assert_eq!(run_status, "cancelled");
        assert_eq!((item_status.as_str(), item_attempts), ("pending", 0));

        let message = |body: &'static str, session: Uuid| NewMessage {
            agent_id: agent,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: None,
            body,
            session_id: Some(session),
            run_id: None,
            meta: serde_json::Value::Null,
        };
        store.insert_message(message("before one", old_main)).await?;
        store
            .insert_message(NewMessage {
                agent_id: agent,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: None,
                body: "Context reset",
                session_id: Some(fresh.id),
                run_id: None,
                meta: json!({ "kind": "reset" }),
            })
            .await?;
        store.insert_message(message("after reset", fresh.id)).await?;
        let listed = store.list_messages(agent, owner, 10).await?;
        assert_eq!(listed.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), ["before one", "Context reset", "after reset"]);
        assert_eq!(listed[1].meta.get("kind").and_then(Value::as_str), Some("reset"));
        Ok(())
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;

    fn entry(definition: Value) -> McpServerEntry {
        McpServerEntry { id: Uuid::nil(), name: "x".into(), definition, scope: "agent".into(), enabled: true, created_at: Utc::now(), duplicate_of: None }
    }

    #[test]
    fn masks_secrets_in_args_and_urls() {
        let masked = entry(json!({
            "command": "npx",
            "args": ["-y", "@scope/server", "--token=abc123", "lin_api_9f8e7d6c5b4a3f2e1d0c", "${secret:KEY}", "/usr/local/bin/tool"],
        }))
        .masked();
        assert_eq!(masked.definition["args"], json!(["-y", "@scope/server", "--token=***", "***", "${secret:KEY}", "/usr/local/bin/tool"]));
        let masked = entry(json!({"type": "http", "url": "https://user:pw@mcp.example.com/sse?api_key=s3cr3t&mode=x"})).masked();
        assert_eq!(masked.definition["url"], "https://***@mcp.example.com/sse?api_key=***&mode=***");
        let plain = entry(json!({"type": "http", "url": "https://mcp.example.com/sse"})).masked();
        assert_eq!(plain.definition["url"], "https://mcp.example.com/sse");
    }

    #[test]
    fn only_reference_based_servers_are_keyless() {
        assert!(keyless_definition(&json!({"command": "slack", "env": {"TOKEN": "${secret:SLACK_TOKEN}"}})));
        assert!(keyless_definition(&json!({"command": "npx", "args": ["-y", "@scope/server"]})));
        assert!(!keyless_definition(&json!({"command": "slack", "env": {"TOKEN": "xoxb-123"}})));
        assert!(!keyless_definition(&json!({"command": "npx", "args": ["--token=abc123"]})));
        assert!(!keyless_definition(&json!({"type": "http", "url": "https://x.dev/sse?key=abc"})));
        assert!(!keyless_definition(&json!({"command": "x", "env": {"T": "pre-${secret:A}"}})));
    }

    #[test]
    fn finds_events_in_hidden_channels() {
        let dm = Uuid::new_v4();
        let event = WorldEvent {
            seq: 1,
            org_id: None,
            event_type: "channel.message".into(),
            agent_id: None,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: None,
            payload: json!({ "channel_id": dm }),
            created_at: Utc::now(),
        };
        assert!(event.in_channel(&HashSet::from([dm])));
        assert!(!event.in_channel(&HashSet::new()));
    }
}

#[cfg(test)]
mod bell_tests {
    use super::*;

    #[tokio::test]
    async fn bell_holds_what_needs_you_and_clears_itself() -> Result<()> {
        let Ok(url) = std::env::var("BELL_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("bell-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bell', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("bell-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(agent).fetch_one(&pool).await?;
        let ask = Uuid::new_v4();
        store
            .emit(NewEvent::new("attention.requested").agent(agent).payload(serde_json::json!({ "text": "pick", "message_id": ask })), None)
            .await?;
        store
            .emit(
                NewEvent::new("message.created").agent(agent).payload(serde_json::json!({ "author_kind": "agent", "body": "done" })),
                None,
            )
            .await?;
        store
            .emit(
                NewEvent::new("message.created").agent(agent).payload(serde_json::json!({ "author_kind": "agent", "body": "quiet", "quiet": true })),
                None,
            )
            .await?;
        store
            .emit(NewEvent::new("task.finished").agent(agent).payload(serde_json::json!({ "status": "done" })), None)
            .await?;
        store
            .emit(NewEvent::new("task.finished").agent(agent).payload(serde_json::json!({ "status": "failed", "error": "boom" })), None)
            .await?;
        let bell = store.notifications(user, org, 60, false).await?;
        let mut kinds: Vec<&str> = bell.items.iter().map(|i| i.kind.as_str()).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, vec!["ask", "message", "quest"]);
        assert_eq!(bell.unread, 3);
        assert_eq!(bell.by_agent.get(&agent), Some(&3));
        assert_eq!(bell.events.len(), 3);

        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta) VALUES ($1, 'user', $2, 'A', $3)")
            .bind(agent)
            .bind(user)
            .bind(serde_json::json!({ "answers": ask }))
            .execute(&pool)
            .await?;
        store.mark_agent_thread_seen(user, agent).await?;
        let bell = store.notifications(user, org, 60, false).await?;
        assert_eq!(bell.items.iter().map(|i| i.kind.as_str()).collect::<Vec<_>>(), vec!["quest"]);
        assert_eq!(store.clear_notifications(user, org, BellClear::Items(vec![bell.items[0].id])).await?, 1);
        assert_eq!(store.notifications(user, org, 60, false).await?.unread, 0);
        assert_eq!(store.notifications(user, org, 60, true).await?.items.len(), 3);

        let channel = store.create_channel(org, &format!("bell-{tag}"), None, user).await?;
        store
            .emit(
                NewEvent::new("channel.message").payload(serde_json::json!({
                    "channel_id": channel.id, "message_id": 7, "mentions_users": [user], "author": "Sam", "body": "hey"
                })),
                None,
            )
            .await?;
        assert_eq!(store.notifications(user, org, 60, false).await?.items[0].kind, "mention");
        store.mark_channel_read(user, channel.id, 6).await?;
        assert_eq!(store.notifications(user, org, 60, false).await?.unread, 1);
        store.mark_channel_read(user, channel.id, 7).await?;
        assert_eq!(store.notifications(user, org, 60, false).await?.unread, 0);
        sqlx::query("DELETE FROM channels WHERE id = $1").bind(channel.id).execute(&pool).await?;

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }
}
