use std::{collections::{HashMap, HashSet}, error::Error as StdError, fmt};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    git_names::TrackedPr,
    milestones::{pr_observation_order, pr_observation_time, PrObservationOrder},
    run_causes::Required,
};

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
    pub answered: bool,
}

impl EventWake {
    pub fn from_payload(payload: &str) -> Self {
        let value = serde_json::from_str::<Value>(payload).unwrap_or(Value::Null);
        EventWake {
            seq: value.get("seq").and_then(Value::as_i64).unwrap_or(0),
            org: value.get("org_id").and_then(Value::as_str).and_then(|org| org.parse().ok()),
            answered: value.get("type").and_then(Value::as_str) == Some("session.answered"),
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
pub const PR_CLOSED_ERROR: &str = "The pull request was closed without merging";

#[derive(Debug, Clone)]
pub struct ClosedPullRequest<'a> {
    pub url: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    pub number: i32,
    pub merged: bool,
    pub closed_at: Option<DateTime<Utc>>,
}

struct PullRequestResult<'a> {
    url: &'a str,
    merged: bool,
    observed_at: Option<DateTime<Utc>>,
    order: PullRequestOrder,
}

#[derive(Clone, Copy)]
enum PullRequestOrder {
    Legacy,
    Ordered,
    Runner,
}

struct WebhookPullRequest<'a> {
    repo: &'a str,
    branch: &'a str,
    number: i32,
}

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
    #[sqlx(default)]
    #[serde(default = "default_asking")]
    pub asking: String,
    #[sqlx(default)]
    #[serde(default)]
    pub crew_variants: Value,
    pub is_main: bool,
    pub jobs: Value,
}

fn default_quest_context() -> String {
    crate::context::QuestContext::default().as_str().to_string()
}

fn default_asking() -> String {
    crate::quest_questions::Asking::Off.as_str().to_string()
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
    pub context_sent: Option<Value>,
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
pub const ROOM_ANSWER_WORKSPACE: &str = "room-answers";

pub fn toolless_aside(asked_by: Option<&Value>) -> bool {
    ["guest", "lean"]
        .iter()
        .any(|flag| asked_by.and_then(|a| a.get(*flag)).and_then(Value::as_bool) == Some(true))
}

pub fn room_answer_aside(asked_by: Option<&Value>) -> bool {
    asked_by.and_then(|asked| asked.get("room_chat")).and_then(Value::as_bool) == Some(true)
}

pub fn room_answer_workspace(session: Uuid) -> String {
    format!("{ROOM_ANSWER_WORKSPACE}/{session}")
}

pub fn room_answer_workspace_key(key: &str) -> bool {
    let mut parts = key.split('/');
    parts.next() == Some(ROOM_ANSWER_WORKSPACE)
        && parts.next().is_some_and(|session| session.parse::<Uuid>().is_ok())
        && parts.next().is_none()
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

const WRITER_KINDS_SQL: &str = "('task', 'followup', 'publish')";

fn shares_folder_sql(held: &str, session: &str) -> String {
    format!(
        "{held}.role = 'task' AND {held}.working_dir = {session}.working_dir AND {held}.runner_id IS NOT DISTINCT FROM {session}.runner_id"
    )
}

fn writes_folder_sql(run: &str, session: &str) -> String {
    format!("({run}.kind IN {WRITER_KINDS_SQL} AND {session}.role = 'task' AND NULLIF(BTRIM({session}.working_dir), '') IS NOT NULL)")
}

fn leased_sql(run: &str) -> String {
    format!("EXISTS (SELECT 1 FROM runner_leases held_lease WHERE held_lease.run_id = {run}.id AND held_lease.status IN ('active', 'finishing'))")
}

pub fn folder_free_sql(run: &str, session: &str) -> String {
    format!(
        "(NOT {writes} OR NOT EXISTS (
            SELECT 1 FROM agent_sessions held
            WHERE {shares} AND (
              EXISTS (
                SELECT 1 FROM session_runs writer
                WHERE writer.session_id = held.id AND writer.kind IN {WRITER_KINDS_SQL}
                  AND (writer.status = 'running' OR {writer_leased}))
              OR ({run}.kind <> 'publish' AND held.id <> {session}.id AND held.status <> 'archived' AND (
                EXISTS (
                  SELECT 1 FROM session_runs pending
                  WHERE pending.session_id = held.id AND pending.kind = 'publish' AND pending.status = 'queued')
                OR EXISTS (
                  SELECT 1 FROM agent_sessions reviewing JOIN session_runs review ON review.session_id = reviewing.id
                  WHERE reviewing.parent_session_id = held.id AND reviewing.role = 'review' AND review.kind = 'review'
                    AND reviewing.asked_by->>'manual' IS DISTINCT FROM 'true'
                    AND (review.status IN ('queued', 'running') OR {review_leased}))
                OR EXISTS (
                  SELECT 1 FROM session_runs carrying
                  WHERE carrying.session_id = held.id AND carrying.carries_on IS NOT NULL))))))",
        writes = writes_folder_sql(run, session),
        shares = shares_folder_sql("held", session),
        writer_leased = leased_sql("writer"),
        review_leased = leased_sql("review"),
    )
}

fn folder_lock_sql(runner: &str, folder: &str) -> String {
    format!("hashtextextended('loka-folder:' || COALESCE({runner}::text, '') || ':' || {folder}, 0)")
}

pub fn withdrawn_note(by: &str) -> String {
    format!("Withdrawn by {by} before it started. Do not restart this work unless they ask.")
}

pub struct FolderHold {
    conn: sqlx::PgConnection,
    runner: Option<Uuid>,
    folder: String,
}

impl FolderHold {
    pub async fn release(mut self) -> Result<()> {
        sqlx::query(&format!("SELECT pg_advisory_unlock({}), pg_notify('session_runs', '')", folder_lock_sql("$1::uuid", "$2::text")))
            .persistent(false)
            .bind(self.runner)
            .bind(&self.folder)
            .execute(&mut self.conn)
            .await?;
        sqlx::Connection::close(self.conn).await?;
        Ok(())
    }
}

pub struct CarryOn {
    conn: sqlx::PgConnection,
    withdrawn: Uuid,
    prior: Uuid,
}

impl CarryOn {
    pub fn prior(&self) -> Uuid {
        self.prior
    }

    pub async fn complete(mut self) -> Result<()> {
        let mut tx = <sqlx::PgConnection as sqlx::Connection>::begin(&mut self.conn).await?;
        let runner = sqlx::query_scalar::<_, Option<Uuid>>(
            "UPDATE session_runs w SET carries_on = NULL
             FROM agent_sessions s
             WHERE w.id = $1 AND w.carries_on = $2 AND s.id = w.session_id
             RETURNING s.runner_id",
        )
        .bind(self.withdrawn)
        .bind(self.prior)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(runner) = runner {
            sqlx::query("SELECT pg_notify('session_runs', $1)")
                .bind(self.withdrawn.to_string())
                .execute(&mut *tx)
                .await?;
            if let Some(runner) = runner {
                sqlx::query("SELECT pg_notify('runner_jobs', $1)")
                    .bind(runner.to_string())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        sqlx::Connection::close(self.conn).await?;
        Ok(())
    }

    pub async fn release(self) -> Result<()> {
        sqlx::Connection::close(self.conn).await?;
        Ok(())
    }
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
    Parked,
    Busy { session_id: Uuid, kind: String, status: String },
    AwaitingPlan(Uuid),
    AwaitingClarification,
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
            TaskResume::AwaitingClarification => Some(format!("this quest {}", crate::quests::CLARIFICATION_WAITING)),
            TaskResume::Fresh | TaskResume::Parked | TaskResume::Resumed { .. } => None,
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
         WHERE ({target}) AND NOT (status = 'done' AND payload ? 'clarification_continuation')"
    )
}

pub async fn display_name_on<'e>(executor: impl sqlx::PgExecutor<'e>, user_id: Uuid) -> Result<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>("SELECT COALESCE(name, email) FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(executor)
        .await?
        .flatten())
}

pub const QUEST_FORKS: &[&str] = &["task", "followup", "review"];

pub const EVERY_FORK: &[&str] = &["task", "followup", "review", "aside", "quick"];

const QUEUED_FORK_SQL: &str = "r.status = 'queued' AND r.kind = ANY($2)
     AND (r.fork_from = $1
          OR (r.kind = 'quick' AND r.fork_from IS NULL AND q.role = 'quick' AND q.parent_session_id = $1 AND q.cli_session_id IS NULL))";

pub async fn unfork_queued_runs(tx: &mut Transaction<'_, Postgres>, main: Uuid, kinds: &[&str], memories: &[(String, String)]) -> Result<()> {
    for (memory, fork_backend) in memories {
        sqlx::query(&format!(
            "WITH moved AS (
               UPDATE session_runs r SET fork_from = NULL, prompt = $3 || E'\\n\\n' || r.prompt
               FROM agent_sessions q
               WHERE q.id = r.session_id AND q.backend = $4 AND {QUEUED_FORK_SQL}
               RETURNING r.session_id
             )
             UPDATE agent_sessions SET usage_baseline = '{{}}'::jsonb
             WHERE id IN (SELECT session_id FROM moved) AND cli_session_id IS NULL"
        ))
        .bind(main)
        .bind(kinds)
        .bind(memory)
        .bind(fork_backend)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
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
    let run = sqlx::query_as::<_, SessionRun>(
        "INSERT INTO session_runs
           (session_id, agent_id, task_id, kind, status, fork_from, prompt, timeout_secs, activity, retry_cause, not_before, nudge, request)
         SELECT session_id, agent_id, task_id, COALESCE($2, kind), 'queued', $3, $4, timeout_secs, $5, $6, $7, nudge,
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
    .await?;
    sqlx::query("UPDATE session_runs SET retried_as = $2 WHERE id = $1")
        .bind(source)
        .bind(run.id)
        .execute(&mut **tx)
        .await?;
    Ok(run)
}

pub fn resume_prompt(from: &str, brief: Option<&str>) -> String {
    let brief = brief.map(str::trim).filter(|b| !b.is_empty());
    format!(
        "[follow-up from {from}]\nPick this quest back up in this same session and quest folder. Check what is already done (diff, commits, branch, PR) before changing anything, and keep what works.{}\n\nWhen it is finished, or you are blocked, end with the full report block as the task prompt describes.",
        brief.map(|b| format!("\n\n{b}")).unwrap_or_default()
    )
}

pub fn main_clarification_task(origin: &Value) -> Option<Uuid> {
    origin.get("task_id").and_then(Value::as_str).and_then(|task| task.parse().ok())
}

pub fn main_clarification_context_task(origin: &Value) -> Option<Uuid> {
    origin.get("context_task_id").and_then(Value::as_str).and_then(|task| task.parse().ok())
}

pub fn main_clarification_resumes(origin: &Value) -> bool {
    origin.get("resume").and_then(Value::as_bool) == Some(true)
}

fn main_clarification_source(origin: &Value) -> Option<i64> {
    origin.get("source_inbox_id").and_then(Value::as_i64)
}

fn clarification_channel_route(route: &Value) -> Option<(Uuid, i64, i64)> {
    let channel_id = route.get("channel_id")?.as_str()?.parse().ok()?;
    let message_id = route.get("message_id")?.as_i64().filter(|id| *id > 0)?;
    let root_id = route.get("root_id")?.as_i64().filter(|id| *id > 0)?;
    Some((channel_id, message_id, root_id))
}

fn clarification_channel_from_mention(payload: &Value) -> Option<(Uuid, i64, i64)> {
    let channel_id = payload.get("channel_id")?.as_str()?.parse().ok()?;
    let message_id = payload.get("message_id")?.as_i64().filter(|id| *id > 0)?;
    let root_id = payload.get("reply_to").and_then(Value::as_i64).unwrap_or(message_id);
    (root_id > 0).then_some((channel_id, message_id, root_id))
}

pub async fn clarification_channel_allowed(
    conn: &mut sqlx::PgConnection,
    agent_id: Uuid,
    route: &Value,
) -> std::result::Result<bool, sqlx::Error> {
    let Some((channel_id, message_id, root_id)) = clarification_channel_route(route) else {
        return Ok(false);
    };
    let valid = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
           SELECT 1
           FROM agent_profiles a
           JOIN channels c ON c.id = $2 AND c.org_id = a.org_id
           JOIN org_members om ON om.org_id = c.org_id AND om.user_id = a.owner_user_id
           JOIN channel_messages source
             ON source.channel_id = c.id AND source.id = $3
            AND source.author_kind = 'user' AND source.author_user_id = a.owner_user_id
           JOIN channel_messages root
             ON root.channel_id = c.id AND root.id = $4 AND root.reply_to IS NULL
           WHERE a.id = $1 AND a.owner_user_id IS NOT NULL
             AND COALESCE(source.reply_to, source.id) = root.id
             AND (c.kind = 'channel' OR EXISTS (
               SELECT 1 FROM channel_members member
               WHERE member.channel_id = c.id AND member.member_kind = 'user' AND member.member_id = a.owner_user_id
             ))
         )",
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(message_id)
    .bind(root_id)
    .fetch_one(&mut *conn)
    .await?;
    if !valid {
        return Ok(false);
    }
    crate::channels::agent_reaches_channel_with(conn, channel_id, agent_id, true).await
}

async fn main_clarification_source_valid(
    conn: &mut sqlx::PgConnection,
    origin: &Value,
    agent_id: Uuid,
    task_id: Option<Uuid>,
) -> Result<bool> {
    let Some(source_id) = main_clarification_source(origin) else {
        return Ok(task_id.is_some());
    };
    let source = sqlx::query_as::<_, (String, Value)>(
        "SELECT kind, payload FROM agent_inbox WHERE id = $1 AND agent_id = $2 FOR UPDATE",
    )
    .bind(source_id)
    .bind(agent_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((kind, payload)) = source else {
        return Ok(false);
    };
    let owner = payload.get("from_owner").and_then(Value::as_bool) == Some(true);
    let source_task = payload.get("task_id").and_then(Value::as_str).and_then(|task| task.parse::<Uuid>().ok());
    let source_scope = task_id.or_else(|| main_clarification_context_task(origin));
    Ok(match kind.as_str() {
        "task.assigned" => {
            task_id == source_task
                && task_id.is_some()
                && payload.get("subscribed").and_then(Value::as_bool) != Some(true)
                && payload.get("clarification_continuation").is_none()
        }
        "message" => {
            if let Some(route) = origin.get("clarification_channel") {
                owner
                    && task_id.is_none()
                    && source_task.is_none()
                    && payload.get("clarification_channel") == Some(route)
                    && payload.get("clarification_continuation").is_some()
                    && clarification_handoff(&payload).is_some()
                    && clarification_channel_allowed(conn, agent_id, route).await?
            } else {
                owner && source_scope == source_task
            }
        }
        "channel.mention" => {
            let route = origin.get("clarification_channel");
            owner
                && task_id.is_none()
                && source_task.is_none()
                && route.and_then(clarification_channel_route) == clarification_channel_from_mention(&payload)
                && match route {
                    Some(route) => clarification_channel_allowed(conn, agent_id, route).await?,
                    None => false,
                }
        }
        "onboarding.finished" => owner && task_id.is_none(),
        _ => false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainClarificationPost {
    Posted,
    AlreadyHandled,
    Refused,
}

pub const CLARIFICATION_POST_REFUSED: &str =
    "I could not attach that question because the request changed. Please send the request again.";

pub fn main_clarification_open(status: &str, assigned: Option<Uuid>, agent_id: Uuid, resumed: bool) -> bool {
    assigned == Some(agent_id)
        && (matches!(status, "todo" | "planning" | "in_progress") || (resumed && status != "cancelled"))
}

pub async fn quest_clarification_with(
    conn: &mut sqlx::PgConnection,
    task_id: Uuid,
    agent_id: Uuid,
    owner_user_id: Uuid,
) -> Result<Option<AgentMessage>> {
    Ok(sqlx::query_as::<_, AgentMessage>(&format!(
        "SELECT m.*
         FROM agent_messages m
         JOIN agent_profiles a ON a.id = m.agent_id
         JOIN agent_tasks t ON t.id = $1 AND t.assigned_agent_id = m.agent_id
         LEFT JOIN agent_sessions s
           ON s.id::text = m.meta->'clarification'->>'session_id'
          AND s.agent_id = m.agent_id AND s.task_id = t.id AND s.status <> 'archived'
         WHERE m.agent_id = $2 AND m.author_kind <> 'user' AND a.owner_user_id = $3
           AND {}
           AND COALESCE(m.meta->'clarification'->>'task_id', m.meta->'clarify_request'->>'task_id') = t.id::text
           AND NOT EXISTS (
             SELECT 1 FROM agent_messages answer
             WHERE answer.agent_id = m.agent_id AND answer.meta->>'answers' = m.id::text
           )
           AND NOT EXISTS (
             SELECT 1 FROM agent_inbox i
             WHERE i.agent_id = m.agent_id AND i.kind = 'task.assigned'
               AND i.status IN ('pending', 'processing')
               AND i.payload->>'task_id' = t.id::text
               AND i.payload ? 'clarification_continuation'
               AND (
                 i.payload->'clarification_waiting_for'->>'prerequisite' = 'clarification'
                 AND (s.id IS NULL OR i.payload->'clarification_waiting_for'->>'session_id' = s.id::text)
               ) IS NOT TRUE
           )
           AND (
             (m.meta->'clarification' IS NOT NULL AND s.plan_state = 'clarifying' AND t.status IN ('in_progress', 'failed'))
             OR
             (m.meta->'clarify_request' IS NOT NULL AND (
               t.status IN ('todo', 'planning', 'in_progress')
               OR (m.meta->'clarify_request'->>'resume' = 'true' AND t.status <> 'cancelled')
             ))
           )
         ORDER BY CASE WHEN m.meta->'clarification' IS NOT NULL THEN 0 ELSE 1 END,
                  m.created_at DESC, m.id DESC
         LIMIT 1",
        crate::guests::thread_filter("$2", "$3"),
    ))
    .bind(task_id)
    .bind(agent_id)
    .bind(owner_user_id)
    .fetch_optional(conn)
    .await?)
}

pub async fn clarification_continuation_pending_with(
    conn: &mut sqlx::PgConnection,
    task_id: Uuid,
    agent_id: Uuid,
) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
           SELECT 1 FROM agent_inbox
           WHERE agent_id = $2 AND kind = 'task.assigned'
             AND status IN ('pending', 'processing')
             AND payload->>'task_id' = $1::text
             AND payload ? 'clarification_continuation'
         )",
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_one(conn)
    .await?)
}

pub(crate) async fn transfer_retry_clarification_resources(
    tx: &mut Transaction<'_, Postgres>,
    source_run: Uuid,
    retry_run: Uuid,
) -> Result<()> {
    transfer_clarification_resources(tx, source_run, retry_run, false).await
}

pub(crate) async fn transfer_plan_clarification_resources(
    tx: &mut Transaction<'_, Postgres>,
    source_run: Uuid,
    next_run: Uuid,
) -> Result<()> {
    transfer_clarification_resources(tx, source_run, next_run, true).await
}

async fn transfer_clarification_resources(
    tx: &mut Transaction<'_, Postgres>,
    source_run: Uuid,
    next_run: Uuid,
    same_session: bool,
) -> Result<()> {
    let compatible = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
           SELECT 1 FROM session_runs source
           JOIN session_runs next
             ON next.agent_id = source.agent_id
            AND next.task_id IS NOT DISTINCT FROM source.task_id
            AND (NOT $3 OR next.session_id = source.session_id)
           WHERE source.id = $1 AND next.id = $2
         )",
    )
    .bind(source_run)
    .bind(next_run)
    .bind(same_session)
    .fetch_one(&mut **tx)
    .await?;
    if !compatible {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO runner_run_inbox (run_id, inbox_id)
         SELECT $2, x.inbox_id
         FROM runner_run_inbox x
         JOIN agent_inbox i ON i.id = x.inbox_id
         JOIN session_runs source ON source.id = x.run_id
         WHERE x.run_id = $1 AND i.agent_id = source.agent_id
           AND i.status = 'done' AND i.payload ? 'clarification_continuation'
         ON CONFLICT DO NOTHING",
    )
    .bind(source_run)
    .bind(next_run)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE agent_messages answer SET run_id = $2
         FROM session_runs source
         WHERE source.id = $1 AND answer.run_id = source.id
           AND answer.agent_id = source.agent_id
           AND answer.author_kind = 'user' AND answer.meta ? 'answers'
           AND EXISTS (
             SELECT 1 FROM agent_messages card
             JOIN agent_sessions card_session
               ON card_session.id::text = card.meta->'clarification'->>'session_id'
              AND card_session.agent_id = source.agent_id
              AND card_session.task_id = source.task_id
             WHERE card.id::text = answer.meta->>'answers' AND card.agent_id = answer.agent_id
               AND card.author_kind <> 'user'
               AND card.meta->'clarification'->>'task_id' = source.task_id::text
           )",
    )
    .bind(source_run)
    .bind(next_run)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn clarification_continuation_meta(payload: &Value) -> Option<(Uuid, Uuid, &str, bool)> {
    let card = payload.get("clarification_continuation")?.as_str()?.parse().ok()?;
    let task = payload.get("task_id")?.as_str()?.parse().ok()?;
    let status = payload.get("clarification_status")?.as_str()?;
    let resumed = payload.get("clarification_resume").and_then(Value::as_bool) == Some(true);
    (payload.get("resume").and_then(Value::as_bool) == Some(true)).then_some((card, task, status, resumed))
}

fn clarification_continuation_card(payload: &Value) -> Option<Uuid> {
    payload.get("clarification_continuation")?.as_str()?.parse().ok()
}

fn clarification_handoff(payload: &Value) -> Option<(&str, &str, &str)> {
    let handoff = payload.get("clarification_handoff")?;
    Some((
        handoff.get("request")?.as_str()?,
        handoff.get("question")?.as_str()?,
        handoff.get("answer")?.as_str()?,
    ))
}

pub fn clarification_continuation_unbound(item: &InboxItem) -> bool {
    item.kind == "message"
        && item.payload.get("task_id").is_none_or(Value::is_null)
        && item.payload.get("from_owner").and_then(Value::as_bool) == Some(true)
        && clarification_continuation_card(&item.payload).is_some()
        && clarification_handoff(&item.payload).is_some()
}

pub fn clarification_continuation_brief(item: &InboxItem) -> Option<String> {
    let bound = clarification_continuation_task(item).is_some();
    let unbound = clarification_continuation_unbound(item);
    if !bound && !unbound {
        return None;
    }
    if let Some(handoff) = item.payload.get("clarification_handoff").and_then(Value::as_object) {
        let request = handoff.get("request").and_then(Value::as_str);
        let question = handoff.get("question").and_then(Value::as_str)?;
        let answer = handoff.get("answer").and_then(Value::as_str)?;
        let mut brief = crate::quests::clarification_exchange(request, question, answer, &item.payload);
        if unbound {
            if let Some(note) = clarification_channel_handoff(&item.payload) {
                brief.push_str(&note);
            }
        }
        return Some(brief);
    }
    if !bound {
        return None;
    }
    let brief = item.payload.get("brief").and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())?;
    let resources = crate::quests::clarification_answer_resources(&item.payload);
    Some(format!("{brief}{resources}"))
}

fn clarification_channel_handoff(payload: &Value) -> Option<String> {
    let route = payload.get("clarification_channel")?;
    let channel_id = route.get("channel_id")?.as_str()?.parse::<Uuid>().ok()?;
    let root_id = route.get("root_id")?.as_i64().filter(|id| *id > 0)?;
    Some(format!(
        "\n\n[private channel handoff]\nThe accepted answer and files are private. Put any outward update in the original channel with post_to_channel (channel: {channel_id}, thread: msg:ch{root_id}) without quoting private details."
    ))
}

fn clarification_continuation_unbound_matches(
    item: &InboxItem,
    agent_id: Uuid,
    kind: &str,
    inbox_status: &str,
    payload: &Value,
) -> bool {
    clarification_continuation_unbound(item)
        && agent_id == item.agent_id
        && kind == "message"
        && matches!(inbox_status, "pending" | "processing")
        && payload.get("task_id").is_none_or(Value::is_null)
        && payload.get("from_owner").and_then(Value::as_bool) == Some(true)
        && clarification_continuation_card(payload) == clarification_continuation_card(&item.payload)
        && payload.get("clarification_handoff") == item.payload.get("clarification_handoff")
        && clarification_handoff(payload).is_some()
}

fn clarification_continuation_matches(
    item: &InboxItem,
    task_status: &str,
    assigned: Option<Uuid>,
    agent_id: Uuid,
    kind: &str,
    inbox_status: &str,
    payload: &Value,
) -> bool {
    let Some((card_id, task_id, accepted_status, resumed)) = clarification_continuation_meta(&item.payload) else {
        return false;
    };
    kind == "task.assigned"
        && agent_id == item.agent_id
        && matches!(inbox_status, "pending" | "processing")
        && clarification_continuation_meta(payload).is_some_and(|(current_card, current_task, current_status, current_resumed)| {
            current_card == card_id
                && current_task == task_id
                && current_status == accepted_status
                && current_resumed == resumed
                && current_status == task_status
                && main_clarification_open(task_status, assigned, agent_id, resumed)
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClarificationContinuationKind {
    Bound,
    Unbound,
}

async fn clarification_continuation_current_for_task(
    conn: &mut sqlx::PgConnection,
    item: &InboxItem,
    task_id: Uuid,
    task_status: &str,
    assigned: Option<Uuid>,
) -> Result<Option<ClarificationContinuationKind>> {
    let current = sqlx::query_as::<_, (Uuid, String, String, Value)>(
        "SELECT agent_id, kind, status, payload FROM agent_inbox WHERE id = $1 FOR UPDATE",
    )
    .bind(item.id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((agent_id, kind, inbox_status, payload)) = current.as_ref() else {
        return Ok(None);
    };
    if clarification_continuation_meta(&item.payload).is_some_and(|(_, bound_task, _, _)| bound_task == task_id)
        && clarification_continuation_matches(item, task_status, assigned, *agent_id, kind, inbox_status, payload)
    {
        return Ok(Some(ClarificationContinuationKind::Bound));
    }
    if assigned != Some(item.agent_id)
        || matches!(task_status, "done" | "cancelled")
        || !clarification_continuation_unbound_matches(item, *agent_id, kind, inbox_status, payload)
    {
        return Ok(None);
    }
    Ok(Some(ClarificationContinuationKind::Unbound))
}

async fn link_clarification_continuation(
    conn: &mut sqlx::PgConnection,
    run_id: Uuid,
    item: &InboxItem,
) -> Result<()> {
    sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(run_id)
        .bind(item.id)
        .execute(conn)
        .await?;
    Ok(())
}

async fn finish_clarification_continuation(conn: &mut sqlx::PgConnection, item_id: i64, error: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE agent_inbox SET status = 'done', error = $2, done_at = NOW()
         WHERE id = $1 AND status IN ('pending', 'processing') AND payload ? 'clarification_continuation'",
    )
    .bind(item_id)
    .bind(error)
    .execute(conn)
    .await?;
    Ok(())
}

async fn park_clarification_continuation(
    conn: &mut sqlx::PgConnection,
    item: &InboxItem,
    task_id: Uuid,
    session_id: Uuid,
    prerequisite: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_inbox
         SET status = 'pending', retry_at = 'infinity',
             error = 'Waiting for the quest to continue',
             payload = jsonb_set(payload, '{clarification_waiting_for}', jsonb_build_object(
               'task_id', $2::text, 'session_id', $3::text, 'prerequisite', $4::text))
         WHERE id = $1 AND status = 'processing' AND payload ? 'clarification_continuation'",
    )
    .bind(item.id)
    .bind(task_id)
    .bind(session_id)
    .bind(prerequisite)
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn adopt_saved_clarifications(
    conn: &mut sqlx::PgConnection,
    run_id: Uuid,
    session_id: Uuid,
    agent_id: Uuid,
    task_id: Uuid,
) -> Result<u64> {
    let task = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT status, assigned_agent_id FROM agent_tasks WHERE id = $1",
    )
    .bind(task_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((status, assigned)) = task else {
        return Ok(0);
    };
    let items = sqlx::query_as::<_, InboxItem>(
        "SELECT id, agent_id, kind, event_seq, payload, status, attempts, created_at
         FROM agent_inbox
         WHERE agent_id = $1 AND kind IN ('task.assigned', 'message') AND status IN ('pending', 'processing')
           AND (
             (payload->'clarification_waiting_for'->>'task_id' = $2::text
               AND payload->'clarification_waiting_for'->>'session_id' = $3::text)
             OR (kind = 'task.assigned' AND payload->>'task_id' = $2::text
               AND NOT payload ? 'clarification_waiting_for')
           )
           AND payload ? 'clarification_continuation'
         ORDER BY id
         FOR UPDATE",
    )
    .bind(agent_id)
    .bind(task_id)
    .bind(session_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut adopted = 0;
    for item in items {
        let valid = clarification_continuation_matches(
            &item, &status, assigned, item.agent_id, &item.kind, &item.status, &item.payload,
        ) || assigned == Some(item.agent_id)
            && !matches!(status.as_str(), "done" | "cancelled")
            && clarification_continuation_unbound_matches(
                &item, item.agent_id, &item.kind, &item.status, &item.payload,
            );
        if !valid {
            finish_clarification_continuation(
                &mut *conn,
                item.id,
                Some("Quest changed after the clarification answer"),
            )
            .await?;
            continue;
        }
        let Some(exchange) = clarification_continuation_brief(&item) else {
            finish_clarification_continuation(&mut *conn, item.id, Some("Invalid clarification continuation")).await?;
            continue;
        };
        let updated = sqlx::query(
            "UPDATE session_runs SET prompt = prompt || E'\n\n[clarification answered while this quest was waiting]\n' || $2
             WHERE id = $1 AND session_id = $3 AND agent_id = $4 AND task_id = $5 AND status = 'queued'",
        )
        .bind(run_id)
        .bind(exchange)
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .execute(&mut *conn)
        .await?;
        if updated.rows_affected() == 0 {
            continue;
        }
        link_clarification_continuation(conn, run_id, &item).await?;
        finish_clarification_continuation(conn, item.id, None).await?;
        adopted += 1;
    }
    Ok(adopted)
}

pub async fn adopt_saved_clarifications_into_queued_run(
    conn: &mut sqlx::PgConnection,
    agent_id: Uuid,
    task_id: Uuid,
) -> Result<Option<Uuid>> {
    let queued = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT r.id, r.session_id
         FROM session_runs r
         JOIN agent_sessions s ON s.id = r.session_id
         WHERE r.agent_id = $1 AND r.task_id = $2 AND r.kind = 'followup' AND r.status = 'queued'
           AND s.agent_id = $1 AND s.task_id = $2 AND s.role = 'task' AND s.status <> 'archived'
         ORDER BY r.created_at, r.id
         FOR UPDATE OF r, s LIMIT 1",
    )
    .bind(agent_id)
    .bind(task_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((run_id, session_id)) = queued else {
        return Ok(None);
    };
    Ok((adopt_saved_clarifications(conn, run_id, session_id, agent_id, task_id).await? > 0).then_some(run_id))
}

pub fn clarification_continuation_task(item: &InboxItem) -> Option<Uuid> {
    if item.kind != "task.assigned" {
        return None;
    }
    clarification_continuation_meta(&item.payload).map(|(_, task, _, _)| task)
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
    #[sqlx(default)]
    #[serde(skip)]
    pub quest_scope: Option<Value>,
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
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by_session: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<Value>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes_to: Option<Value>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_status: Option<String>,
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
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Value>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_event: Option<String>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired: Option<Value>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
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

    pub fn is_action(&self) -> bool {
        self.kind == crate::pr_actions::KIND
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
    #[sqlx(default)]
    #[serde(default)]
    pub github_repos: Vec<String>,    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
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
    pub milestones: Vec<crate::milestones::Milestone>,    #[sqlx(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<crate::quest_questions::DecisionSummary>,
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
    #[serde(default)]
    pub asking: Option<String>,
    #[serde(default)]
    pub crew_variants: Option<Vec<Value>>,
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

const AGENT_COLUMNS: &str = "id, slug, name, avatar_emoji, cli, model, system_prompt_addendum, persona, role, look, gear, status, status_detail, owner_user_id, visibility, paused_reason, archived_at, archive_delete_at, tags, expertise, created_at, revoked_powers, approval_mode, reviewer, daily_budget_usd, daily_token_budget, agent_profiles.org_id, agent_profiles.quest_context, agent_profiles.asking, agent_profiles.crew_variants,
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

pub(crate) fn task_time_limit_secs(minutes: Option<i64>) -> Result<Option<i32>> {
    match minutes {
        Some(m) if !(WorldStore::MIN_TIME_LIMIT_MINUTES..=WorldStore::MAX_TIME_LIMIT_MINUTES).contains(&m) => anyhow::bail!(
            "a time limit must be between {} minutes and {} hours",
            WorldStore::MIN_TIME_LIMIT_MINUTES,
            WorldStore::MAX_TIME_LIMIT_MINUTES / 60
        ),
        Some(m) => Ok(Some((m * 60) as i32)),
        None => Ok(None),
    }
}

pub(crate) async fn set_task_time_limit_in(tx: &mut Transaction<'_, Postgres>, task_id: Uuid, secs: Option<i32>) -> Result<()> {
    let found = sqlx::query("UPDATE agent_tasks SET time_limit_secs = $2, updated_at = NOW() WHERE id = $1")
        .bind(task_id)
        .bind(secs)
        .execute(&mut **tx)
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
    .execute(&mut **tx)
    .await?;
    Ok(())
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

async fn create_run_in(conn: &mut sqlx::PgConnection, new: NewRun<'_>) -> Result<SessionRun> {
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
    .fetch_one(conn)
    .await?)
}

fn event_type_patterns(types: &[String]) -> Vec<String> {
    types
        .iter()
        .map(|kind| kind.trim())
        .filter(|kind| !kind.is_empty())
        .map(|kind| match kind.strip_suffix('*') {
            Some(prefix) => format!("{}%", prefix.replace('%', "")),
            None if kind.ends_with('.') => format!("{}%", kind.replace('%', "")),
            None => kind.replace('%', ""),
        })
        .collect()
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

    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile_task_completion(
        &self,
        task_id: Uuid,
        summary: Option<&str>,
        milestone_positions: &[i32],
        milestone_titles: &[String],
        error: Option<&str>,
        pr_url: Option<&str>,
        changes_requested: bool,
    ) -> Result<Option<(&'static str, bool)>> {
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT status, pr_url, merged_pr_url FROM agent_tasks WHERE id = $1 FOR UPDATE",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current_status, tracked_pr_url, merged_pr_url)) = task else {
            tx.rollback().await?;
            return Ok(None);
        };
        let tracked_pr = tracked_pr_url.as_deref().and_then(TrackedPr::parse);
        if let Some(expected_pr_url) = pr_url {
            let Some(expected_pr) = TrackedPr::parse(expected_pr_url) else {
                tx.rollback().await?;
                return Ok(None);
            };
            if !tracked_pr.is_some_and(|tracked| expected_pr.same_identity(tracked)) {
                tx.rollback().await?;
                return Ok(None);
            }
        }
        let merged = error.is_none()
            && tracked_pr
                .zip(merged_pr_url.as_deref().and_then(TrackedPr::parse))
                .is_some_and(|(tracked, merged)| tracked.same_identity(merged));
        let status = crate::milestones::finished_status(error.is_some(), merged, tracked_pr_url.is_some(), changes_requested);
        if current_status == "cancelled" || current_status == "done" && (status != "done" || error.is_some()) {
            tx.rollback().await?;
            return Ok(None);
        }
        if current_status == "done" {
            tx.rollback().await?;
            return Ok(Some((status, false)));
        }
        if let Some(summary) = summary {
            sqlx::query("UPDATE agent_tasks SET result_content = $2 WHERE id = $1 AND result_content IS DISTINCT FROM $2")
                .bind(task_id)
                .bind(summary)
                .execute(&mut *tx)
                .await?;
        }
        crate::milestones::complete_work_in(&mut tx, task_id, milestone_positions, milestone_titles, error.is_none()).await?;
        let watching = if error.is_none() || pr_url.is_some() {
            crate::milestones::arm_merge_in(&mut tx, task_id).await?
        } else {
            false
        };
        sqlx::query(
            "UPDATE agent_tasks SET status = $2, error_message = $3
             WHERE id = $1 AND (status IS DISTINCT FROM $2 OR error_message IS DISTINCT FROM $3)",
        )
        .bind(task_id)
        .bind(status)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((status, watching)))
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
             WHERE id = $2 AND agent_id = $1 AND kind <> 'action'",
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
        if schedule.is_action() {
            return Ok(self.queue_pr_action(schedule).await?.is_some());
        }
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
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at,
                    action, on_event, fired, outcome, run_id FROM schedules
             WHERE agent_id = $1 AND (expires_at IS NULL OR expires_at > NOW()) ORDER BY created_at",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn schedule_by_id(&self, id: Uuid) -> Result<Option<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at,
                    action, on_event, fired, outcome, run_id FROM schedules
             WHERE id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        )
        .bind(id)
        .fetch_optional(&self.pool)
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
                    s.next_run_at, s.enabled, s.kind, s.task_id, s.expires_at, s.action, s.on_event, s.fired, s.outcome, s.run_id
             FROM schedules s JOIN agent_profiles a ON a.id = s.agent_id
             WHERE a.owner_user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
             ORDER BY a.name, s.created_at",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_agent_schedule(&self, agent_id: Uuid, id: Uuid) -> Result<bool> {
        sqlx::query(
            "WITH cancelled AS (
               UPDATE session_runs SET status = 'cancelled', error = 'The pull request action was deleted before it ran', finished_at = NOW()
               WHERE status = 'queued' AND kind = 'action'
                 AND id = (SELECT run_id FROM schedules WHERE id = $2 AND agent_id = $1 AND kind = 'action')
               RETURNING session_id
             )
             UPDATE agent_sessions SET status = 'archived', updated_at = NOW() WHERE id IN (SELECT session_id FROM cancelled)",
        )
        .bind(agent_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
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

    pub async fn set_sent_context(&self, id: Uuid, sent: &crate::context::SentContext) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET context_sent = $2 WHERE id = $1")
            .bind(id)
            .bind(sent.to_value())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn forget_sent_context(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET context_sent = '{}'::jsonb WHERE id = $1")
            .bind(id)
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
        self.open_aside(agent_id, question, asked_by, prompt, guest, choice).await
    }

    pub async fn open_aside(
        &self,
        agent_id: Uuid,
        question: &str,
        asked_by: Value,
        prompt: Option<String>,
        guest: bool,
        choice: &AsideChoice,
    ) -> Result<SessionRun> {
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
        self.start_lean_aside_with(agent_id, asked_by, prompt, false).await
    }

    pub async fn start_room_aside(&self, agent_id: Uuid, asked_by: Value, prompt: &str) -> Result<SessionRun> {
        self.start_lean_aside_with(agent_id, asked_by, prompt, true).await
    }

    async fn start_lean_aside_with(&self, agent_id: Uuid, asked_by: Value, prompt: &str, own_model: bool) -> Result<SessionRun> {
        let mut tx = self.pool.begin().await?;
        let main = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions
             WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR SHARE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("your main agent has not started yet; say hello to it first"))?;
        let (backend, model) = match (own_model, main.backend.as_str()) {
            (true, backend) if TOOLLESS_BACKENDS.contains(&backend) => (main.backend.clone(), main.model.clone()),
            (_, "omp") => ("omp".to_string(), main.model.clone()),
            _ => (GUEST_BACKEND.to_string(), GUEST_MODEL.to_string()),
        };
        let mut asked_by = asked_by;
        if let Some(obj) = asked_by.as_object_mut() {
            obj.insert("lean".into(), Value::Bool(true));
        }
        let session_id = Uuid::new_v4();
        let working_dir = if own_model { Some(room_answer_workspace(session_id)) } else { main.working_dir.clone() };
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (id, agent_id, role, backend, model, parent_session_id, working_dir, usage_baseline, asked_by)
             VALUES ($1, $2, 'aside', $3, $4, $5, $6, '{}'::jsonb, $7)
             RETURNING *",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(main.id)
        .bind(working_dir)
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
               asking = COALESCE($18, asking),
               crew_variants = COALESCE($19, crew_variants),
               updated_at = NOW()
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        let trim = |v: &Option<String>| v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let visibility = trim(&u.visibility);
        let save = || {
            sqlx::query_as::<_, WorldAgent>(&sql)
                .bind(id)
                .bind(trim(&u.name))
                .bind(trim(&u.persona))
                .bind(trim(&u.cli))
                .bind(trim(&u.model))
                .bind(trim(&u.avatar_emoji))
                .bind(&u.look)
                .bind(&u.gear)
                .bind(visibility.clone())
                .bind(u.tags.as_ref().map(|t| crate::expertise::merge_tags(&[], t)))
                .bind(trim(&u.expertise))
                .bind(u.revoked_powers.as_ref().map(|p| crate::powers::normalize(p)))
                .bind(trim(&u.approval_mode))
                .bind(&u.reviewer)
                .bind(&u.daily_budget_usd)
                .bind(&u.daily_token_budget)
                .bind(u.quest_context.as_deref().and_then(|c| crate::context::QuestContext::parse(c).ok()).map(|c| c.as_str()))
                .bind(u.asking.as_deref().and_then(|a| crate::quest_questions::Asking::parse(a).ok()).map(|a| a.as_str()))
                .bind(u.crew_variants.as_deref().and_then(|v| crate::quest_questions::clean_crew_variants(v, None).ok()))
        };
        if visibility.is_none() {
            return Ok(save().fetch_optional(&self.pool).await?);
        }
        let mut tx = self.pool.begin().await?;
        if let Some(org) = sqlx::query_scalar::<_, Uuid>("SELECT org_id FROM agent_profiles WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        {
            crate::orgs::lock_org_parent(&mut tx, org).await?;
        }
        let updated = save().fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
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

    pub async fn session_briefs(&self, agent_id: Option<Uuid>, session_id: Option<Uuid>, limit: i64) -> Result<Vec<SessionBrief>> {
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
               AND ($2::uuid IS NULL OR s.id = $2)
             ORDER BY s.updated_at DESC LIMIT $3",
        )
        .bind(agent_id)
        .bind(session_id)
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
        let locked_task_status = if let Some(task_id) = session.task_id {
            let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1 FOR NO KEY UPDATE")
                .bind(task_id)
                .fetch_one(&mut *tx)
                .await?;
            (session.role == "task").then_some(status)
        } else {
            None
        };
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(session.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let task_id = sqlx::query_scalar::<_, Option<Uuid>>("SELECT task_id FROM agent_sessions WHERE id = $1")
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        if let Some(task_id) = task_id {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_tasks WHERE id = $1 FOR UPDATE")
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?;
        }
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
        let reopened_session = current.0 == "task" && current.2 == "failed" && reopen_failed_session(&mut tx, session.id).await?;
        let reopened = reopened_session && locked_task_status.as_deref() == Some("failed");
        let (plan_state, plan_first, asked_by) = sqlx::query_as::<_, (Option<String>, bool, Option<Value>)>(
            "SELECT s.plan_state, COALESCE(t.plan_first, FALSE), s.asked_by
             FROM agent_sessions s LEFT JOIN agent_tasks t ON t.id = s.task_id WHERE s.id = $1",
        )
        .bind(session.id)
        .fetch_one(&mut *tx)
        .await?;
        let plan_first = plan_first || crate::quests::is_best_of_prework(asked_by.as_ref());
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
            crate::quests::finish_single_prework_in(&mut tx, session.id).await?;
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
        .bind(current.3)
        .bind(prompt)
        .bind(interrupt)
        .bind(crate::run_request::RunRequest::asked(from, Some(text)).value())
        .fetch_one(&mut *tx)
        .await?;
        if current.0 == "task" {
            if let Some(task_id) = current.3 {
                adopt_saved_clarifications(&mut tx, run.id, session.id, session.agent_id, task_id).await?;
            }
        }
        if reopened_session {
            reopen_quest(&mut tx, current.3).await?;
        }
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
        self.nudge_missing_with(run_id, session_id, required, None, false).await
    }

    pub async fn nudge_single_prework(&self, run_id: Uuid, session_id: Uuid, prompt: &str) -> Result<Nudge> {
        self.nudge_missing_with(run_id, session_id, Required::Report, Some(prompt), true).await
    }

    async fn nudge_missing_with(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        required: Required,
        prompt_override: Option<&str>,
        single_prework: bool,
    ) -> Result<Nudge> {
        let (role, kinds, kind, reminders, activity) = match required {
            Required::Report => ("task", &["task", "followup"][..], "followup", &[REPORT_NUDGE, REPORT_NUDGE_LAST][..], "Writing up the report"),
            Required::Verdict => ("review", &["review"][..], "review", &[VERDICT_NUDGE][..], "Waiting for the review verdict"),
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
        if single_prework && !crate::quests::owns_single_prework_in(&mut tx, session_id, run_id).await? {
            return Ok(Nudge::Open);
        }
        let (open, sent) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*) FILTER (WHERE status IN ('queued', 'running') AND id <> $4),
                    COUNT(*) FILTER (WHERE nudge = $3 AND retry_cause IS NULL
                                       AND created_at >= COALESCE((SELECT MAX(created_at) FROM session_runs
                                                                   WHERE session_id = $1 AND kind = ANY($2) AND nudge IS NULL),
                                                                 '-infinity'))
             FROM session_runs WHERE session_id = $1 AND kind = ANY($2)",
        )
        .bind(session_id)
        .bind(kinds)
        .bind(required.nudge())
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
        if open > 0 {
            return Ok(Nudge::Open);
        }
        let Some(reminder) = usize::try_from(sent).ok().and_then(|sent| reminders.get(sent)) else {
            return Ok(Nudge::Exhausted);
        };
        let prompt = prompt_override.unwrap_or(reminder);
        let nudge = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity, nudge)
             VALUES ($1, $2, $3, $4, 'queued', $5, 3600, $6, $7)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(kind)
        .bind(prompt)
        .bind(activity)
        .bind(required.nudge())
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

    pub async fn run_nudge(&self, run_id: Uuid) -> Result<Option<Required>> {
        let nudge = sqlx::query_scalar::<_, Option<String>>("SELECT nudge FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok(match nudge.as_deref() {
            Some("report") => Some(Required::Report),
            Some("verdict") => Some(Required::Verdict),
            _ => None,
        })
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
        fail_completed_run_in(&mut tx, run_id, &kind, required.missed(), &diagnosis).await?;
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
        if diagnosis.overflowed() {
            return self.recover_overflow(run_id, true).await;
        }
        if diagnosis.lost_session() {
            return Ok(match self.start_over_lost_session(run_id).await? {
                crate::context::StartOver::Retried(id) => Some(id),
                _ => None,
            });
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
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1 FOR NO KEY UPDATE")
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
        let session = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<Value>)>(
            "SELECT status, cli_session_id, plan_state, asked_by FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((session_status, cli_session_id, plan_state, asked_by)) = session else {
            return Ok(None);
        };
        if session_status == "archived" || kind == "followup" && cli_session_id.is_none() {
            return Ok(None);
        }
        let resume = diagnosis.cause == crate::run_causes::Cause::CliError
            && kind != "review"
            && cli_session_id.is_some();
        let source_is_task = kind == "task";
        let (kind, prompt, fork_from) = if resume {
            let prompt = if update { crate::run_causes::RESUME_AFTER_UPDATE } else { crate::run_causes::RESUME_AFTER_ERROR };
            ("followup".to_string(), prompt.to_string(), None)
        } else {
            (kind, prompt, fork_from)
        };
        let answer_prework = crate::quests::single_prework_phase(asked_by.as_ref()).is_some()
            && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM agent_messages WHERE run_id = $1 AND meta ? 'answers')")
                .bind(run_id)
                .fetch_one(&mut *tx)
                .await?;
        let retry_prework = crate::quests::single_prework_phase(asked_by.as_ref()).is_some()
            && (source_is_task
                || plan_state.as_deref() == Some("planning")
                || answer_prework
                || crate::quests::single_prework_retry(asked_by.as_ref(), run_id));
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
        if retry_prework {
            sqlx::query(
                "UPDATE agent_sessions
                 SET asked_by = jsonb_set(asked_by, '{before_big_work_retry}', to_jsonb($2::text)), updated_at = NOW()
                 WHERE id = $1 AND asked_by->>'before_big_work_phase' IN ('pending', 'planning')",
            )
            .bind(session_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        transfer_retry_clarification_resources(&mut tx, run_id, id).await?;
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
        self.resume_task_session_with_continuation(agent_id, task_id, from, brief, &prompt, None).await
    }

    pub async fn resume_clarification_task_session(
        &self,
        agent_id: Uuid,
        task_id: Uuid,
        from: &str,
        brief: Option<&str>,
        item: &InboxItem,
    ) -> Result<TaskResume> {
        let prompt = resume_prompt(from, brief);
        self.resume_task_session_with_continuation(agent_id, task_id, from, brief, &prompt, Some(item)).await
    }

    async fn resume_task_session_with_continuation(
        &self,
        agent_id: Uuid,
        task_id: Uuid,
        from: &str,
        brief: Option<&str>,
        prompt: &str,
        continuation: Option<&InboxItem>,
    ) -> Result<TaskResume> {
        let mut tx = self.pool.begin().await?;
        let guarded_continuation = continuation.and_then(clarification_continuation_task) == Some(task_id);
        let mut facts = crate::quests::load_resume_facts(&mut tx, agent_id, task_id, true, guarded_continuation).await?;
        let continuation_kind = if let Some(item) = continuation {
            let assigned = sqlx::query_scalar::<_, Option<Uuid>>("SELECT assigned_agent_id FROM agent_tasks WHERE id = $1")
                .bind(task_id)
                .fetch_one(&mut *tx)
                .await?;
            let current = clarification_continuation_current_for_task(&mut tx, item, task_id, &facts.status, assigned).await?;
            if current.is_none() {
                if !clarification_continuation_unbound(item) {
                    finish_clarification_continuation(
                        &mut tx,
                        item.id,
                        Some("Quest changed after the clarification answer"),
                    )
                    .await?;
                }
                tx.commit().await?;
                anyhow::bail!("quest changed after the clarification answer")
            }
            current
        } else {
            None
        };
        if continuation_kind == Some(ClarificationContinuationKind::Bound)
            && facts.active.as_ref().is_some_and(|active| {
                matches!(active.kind.as_str(), "task" | "followup")
                    && facts.live.as_ref().is_some_and(|session| session.id == active.session_id)
            })
        {
            facts.active = None;
        }
        let (session_id, plan_skipped) = match crate::quests::decide_resume(&facts) {
            crate::quests::ResumePlan::Refused(reason) => anyhow::bail!(reason),
            crate::quests::ResumePlan::Busy(active) => {
                return Ok(TaskResume::Busy { session_id: active.session_id, kind: active.kind, status: active.status })
            }
            crate::quests::ResumePlan::AwaitingPlan(session_id) => {
                if let Some(item) = continuation.filter(|_| continuation_kind.is_some()) {
                    park_clarification_continuation(&mut tx, item, task_id, session_id, "plan").await?;
                    tx.commit().await?;
                    return Ok(TaskResume::Parked);
                }
                return Ok(TaskResume::AwaitingPlan(session_id));
            }
            crate::quests::ResumePlan::AwaitingClarification => {
                if let (Some(item), Some(session)) = (
                    continuation.filter(|_| continuation_kind.is_some()),
                    facts.live.as_ref(),
                ) {
                    park_clarification_continuation(&mut tx, item, task_id, session.id, "clarification").await?;
                    tx.commit().await?;
                    return Ok(TaskResume::Parked);
                }
                return Ok(TaskResume::AwaitingClarification);
            }
            crate::quests::ResumePlan::Fresh => return Ok(TaskResume::Fresh),
            crate::quests::ResumePlan::Resume { session_id, plan_skipped } => (session_id, plan_skipped),
        };
        if plan_skipped {
            sqlx::query("UPDATE agent_sessions SET plan_state = 'approved' WHERE id = $1")
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
            crate::quests::finish_single_prework_in(&mut tx, session_id).await?;
        }
        let prompt = if plan_skipped { format!("{}\n\n{prompt}", crate::quests::PLAN_SKIPPED) } else { prompt.to_string() };
        sqlx::query("UPDATE agent_sessions SET status = 'active', review_rounds = 0, updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE agent_tasks
             SET status = 'in_progress', error_message = CASE WHEN status = 'failed' THEN NULL ELSE error_message END
             WHERE id = $1 AND status NOT IN ('done', 'cancelled')",
        )
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
        let contextual_agents = if facts.status == "done" {
            settle_contextual_task_clarifications(&mut tx, &[task_id], "Quest resumed").await?
        } else {
            Vec::new()
        };
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity, request)
             VALUES ($1, $2, $3, 'followup', 'queued', $4, 3600, 'Picking the quest back up', $5)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(prompt)
        .bind(crate::run_request::RunRequest::asked(from, if continuation.is_some() { None } else { brief }).value())
        .fetch_one(&mut *tx)
        .await?;
        if let Some(item) = continuation {
            link_clarification_continuation(&mut tx, run_id, item).await?;
            sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1")
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
            finish_clarification_continuation(&mut tx, item.id, None).await?;
        }
        adopt_saved_clarifications(&mut tx, run_id, session_id, agent_id, task_id).await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&contextual_agents).await;
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
               AND NOT EXISTS (
                 SELECT 1 FROM runner_run_inbox x JOIN agent_inbox i ON i.id = x.inbox_id
                 WHERE x.run_id = session_runs.id AND i.payload ? 'clarification_continuation'
               )
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
               AND NOT EXISTS (
                 SELECT 1 FROM runner_run_inbox x JOIN agent_inbox i ON i.id = x.inbox_id
                 WHERE x.run_id = f.id AND i.payload ? 'clarification_continuation'
               )
             RETURNING f.id",
        )
        .bind(followup_ids)
        .bind(into_run)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn other_folder_writer(&self, session: &AgentSession) -> Result<Option<String>> {
        if session.role != "task" || session.working_dir.as_deref().is_none_or(|dir| dir.trim().is_empty()) {
            return Ok(None);
        }
        let last = sqlx::query_as::<_, (Uuid, Uuid, String)>(&format!(
            "SELECT writer.id, writer.session_id, writer.kind FROM session_runs writer
             JOIN agent_sessions held ON held.id = writer.session_id
             JOIN agent_sessions s ON s.id = $1
             WHERE {shares} AND writer.kind IN ('task', 'followup') AND writer.started_at IS NOT NULL
             ORDER BY writer.started_at DESC, writer.id DESC LIMIT 1",
            shares = shares_folder_sql("held", "s")
        ))
        .bind(session.id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(last.filter(|(_, writer, _)| *writer != session.id).map(|(run, writer, kind)| {
            let what = if kind == "followup" { "a follow-up" } else { "a task run" };
            format!("{what} ({}) of another session of this quest ({})", &run.to_string()[..8], &writer.to_string()[..8])
        }))
    }

    pub async fn hold_folder(&self, session: &AgentSession) -> Result<Option<FolderHold>> {
        let held = sqlx::query_as::<_, (Option<Uuid>, String)>(
            "SELECT runner_id, working_dir FROM agent_sessions WHERE id = $1 AND role IN ('task', 'review') AND NULLIF(BTRIM(working_dir), '') IS NOT NULL",
        )
        .bind(session.id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((runner, folder)) = held else {
            return Ok(None);
        };
        let mut conn = <sqlx::PgConnection as sqlx::Connection>::connect_with(&self.pool.connect_options()).await?;
        sqlx::query(&format!("SELECT pg_advisory_lock({})", folder_lock_sql("$1::uuid", "$2::text")))
            .persistent(false)
            .bind(runner)
            .bind(&folder)
            .execute(&mut conn)
            .await?;
        Ok(Some(FolderHold { conn, runner, folder }))
    }

    pub async fn withdraw_queued(&self, run_id: Uuid, by: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query_scalar::<_, Uuid>(
            "SELECT t.id FROM session_runs w
             JOIN agent_sessions s ON s.id = w.session_id
             JOIN agent_tasks t ON t.id = COALESCE(w.task_id, s.task_id)
             WHERE w.id = $1 FOR UPDATE OF t",
        )
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        let session = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id FROM session_runs w JOIN agent_sessions s ON s.id = w.session_id WHERE w.id = $1 FOR UPDATE OF s",
        )
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        if session.is_none() {
            return Ok(false);
        }
        let withdrawn = sqlx::query(
            "UPDATE session_runs SET status = 'cancelled', finished_at = NOW(), stop_requested_by = $2, error = $3
             WHERE id = $1 AND status = 'queued' AND kind <> 'turn'",
        )
        .bind(run_id)
        .bind(by)
        .bind(withdrawn_note(by))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !withdrawn {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE session_runs SET carries_on = (
               SELECT prior.id FROM session_runs w
               JOIN agent_sessions s ON s.id = w.session_id AND s.role = 'task' AND s.status <> 'archived'
               JOIN session_runs prior ON prior.session_id = w.session_id AND prior.kind = 'followup'
                 AND prior.status = 'completed' AND prior.finished_at >= w.created_at
               WHERE w.id = $1 AND w.kind = 'followup' AND w.started_at IS NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs live
                   WHERE live.session_id = w.session_id AND live.kind = 'followup' AND live.status IN ('queued', 'running'))
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs later
                   WHERE later.created_at > prior.finished_at AND later.status <> 'cancelled'
                     AND (later.session_id = s.id OR later.session_id IN (SELECT id FROM agent_sessions WHERE parent_session_id = s.id)))
               ORDER BY prior.finished_at DESC LIMIT 1)
             WHERE id = $1",
        )
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        crate::withdraw::announce_stopped(&self.pool, &crate::withdraw::StoppedWork { runs: vec![run_id], ..Default::default() }).await;
        Ok(true)
    }

    pub async fn carry_on_candidates(&self, withdrawn: Option<Uuid>, on_device: bool) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT w.id FROM session_runs w
             JOIN agent_sessions s ON s.id = w.session_id
             WHERE w.carries_on IS NOT NULL AND ($1::uuid IS NULL OR w.id = $1) AND (s.runner_id IS NOT NULL) = $2
             ORDER BY w.finished_at, w.id",
        )
        .bind(withdrawn)
        .bind(on_device)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn claim_carry_on(&self, withdrawn: Uuid, on_device: bool) -> Result<Option<CarryOn>> {
        let mut conn = <sqlx::PgConnection as sqlx::Connection>::connect_with(&self.pool.connect_options()).await?;
        let locked = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock(hashtextextended('loka-carry-on:' || $1::text, 0))")
            .bind(withdrawn)
            .fetch_one(&mut conn)
            .await?;
        if !locked {
            sqlx::Connection::close(conn).await?;
            return Ok(None);
        }
        let prior = sqlx::query_scalar::<_, Uuid>(
            "SELECT w.carries_on FROM session_runs w
             JOIN agent_sessions s ON s.id = w.session_id
             WHERE w.id = $1 AND w.carries_on IS NOT NULL AND (s.runner_id IS NOT NULL) = $2
               AND (NOT $2 OR NOT EXISTS (
                 SELECT 1 FROM runner_leases l WHERE l.run_id = w.carries_on AND l.status IN ('active', 'finishing')))",
        )
        .bind(withdrawn)
        .bind(on_device)
        .fetch_optional(&mut conn)
        .await?;
        let Some(prior) = prior else {
            sqlx::Connection::close(conn).await?;
            return Ok(None);
        };
        Ok(Some(CarryOn { conn, withdrawn, prior }))
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

    pub async fn settle_pr(&self, task_id: Uuid, expected_pr_url: &str, merged: bool) -> Result<bool> {
        self.settle_pr_inner(
            task_id,
            PullRequestResult { url: expected_pr_url, merged, observed_at: None, order: PullRequestOrder::Legacy },
            None,
            false,
            None,
            None,
        )
        .await
    }

    pub async fn settle_pr_observed(
        &self,
        task_id: Uuid,
        expected_pr_url: &str,
        merged: bool,
        observed_at: DateTime<Utc>,
    ) -> Result<bool> {
        self.settle_pr_inner(
            task_id,
            PullRequestResult { url: expected_pr_url, merged, observed_at: Some(observed_at), order: PullRequestOrder::Ordered },
            None,
            false,
            None,
            None,
        )
        .await
    }

    pub async fn settle_pr_from_webhook(
        &self,
        task_id: Uuid,
        pr: ClosedPullRequest<'_>,
    ) -> Result<bool> {
        let Some(identity) = TrackedPr::parse(pr.url) else {
            return Ok(false);
        };
        if pr.branch.is_empty() || !identity.repo().eq_ignore_ascii_case(pr.repo) || identity.number() != pr.number {
            return Ok(false);
        }
        let result = PullRequestResult { url: pr.url, merged: pr.merged, observed_at: pr.closed_at, order: PullRequestOrder::Ordered };
        let webhook = WebhookPullRequest { repo: pr.repo, branch: pr.branch, number: pr.number };
        self.settle_pr_inner(task_id, result, None, false, Some(webhook), None).await
    }

    pub async fn task_failure_is_stale(&self, task_id: Uuid, run_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT CASE
                 WHEN t.status = 'cancelled' THEN TRUE
                 WHEN t.status <> 'done' THEN FALSE
                 ELSE EXISTS (
                   SELECT 1 FROM session_runs r
                   JOIN world_events e ON e.task_id = t.id AND e.created_at >= r.created_at
                   WHERE r.id = $2 AND (
                     e.type = 'task.merged' AND e.payload->>'pr_url' = t.pr_url
                     OR e.type = 'task.finished' AND e.payload->>'status' = 'done'
                     OR e.type = 'task.changed' AND e.payload->>'status' = 'done'
                       AND e.payload->>'changed' IN ('status', 'created')
                   )
                 )
               END
             FROM agent_tasks t WHERE t.id = $1",
        )
        .bind(task_id)
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false))
    }

    pub async fn settle_pr_from_runner(&self, task_id: Uuid, expected_pr_url: &str, merged: bool, runner_id: Uuid) -> Result<bool> {
        self.settle_pr_inner(
            task_id,
            PullRequestResult { url: expected_pr_url, merged, observed_at: None, order: PullRequestOrder::Legacy },
            Some(runner_id),
            false,
            None,
            None,
        )
        .await
    }

    pub async fn settle_pr_from_runner_observed(
        &self,
        task_id: Uuid,
        expected_pr_url: &str,
        merged: bool,
        runner_id: Uuid,
        observed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        self.settle_pr_inner(
            task_id,
            PullRequestResult { url: expected_pr_url, merged, observed_at, order: PullRequestOrder::Runner },
            Some(runner_id),
            false,
            None,
            None,
        )
        .await
    }

    pub async fn settle_merged_followup(
        &self,
        task_id: Uuid,
        session_id: Uuid,
        expected_pr_url: &str,
    ) -> Result<bool> {
        self.settle_pr_inner(
            task_id,
            PullRequestResult { url: expected_pr_url, merged: true, observed_at: None, order: PullRequestOrder::Legacy },
            None,
            false,
            None,
            Some(session_id),
        )
        .await
    }

    async fn settle_pr_inner(
        &self,
        task_id: Uuid,
        result: PullRequestResult<'_>,
        runner_id: Option<Uuid>,
        require_recorded_merge: bool,
        webhook: Option<WebhookPullRequest<'_>>,
        followup_session: Option<Uuid>,
    ) -> Result<bool> {
        let expected_pr_url = result.url;
        let merged = result.merged;
        let Some(result_pr) = TrackedPr::parse(expected_pr_url) else {
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, (String, String, Option<Uuid>, Option<String>, Option<String>)>(
            "SELECT t.title, t.status, t.assigned_agent_id, t.pr_url, t.merged_pr_url
             FROM agent_tasks t
             WHERE t.id = $1 AND ($2::uuid IS NULL OR EXISTS (
               SELECT 1 FROM world_runners w
               JOIN agent_profiles a ON a.id = t.assigned_agent_id
               WHERE w.id = $2 AND w.status = 'online' AND a.owner_user_id = w.owner_user_id
                 AND a.runner_id = w.id AND a.paused_reason IS NULL
             )) AND ($3::text IS NULL
               OR t.pr_url IS NOT NULL AND lower(t.pr_url) = lower($5)
               OR t.pr_url IS NULL AND t.branch = $4
                 AND lower(regexp_replace(regexp_replace(COALESCE(t.repo, ''), '^https?://github\\.com/', ''), '\\.git$', '')) = lower($3))
             FOR UPDATE OF t",
        )
        .bind(task_id)
        .bind(runner_id)
        .bind(webhook.as_ref().map(|pr| pr.repo))
        .bind(webhook.as_ref().map(|pr| pr.branch))
        .bind(webhook.as_ref().map(|_| expected_pr_url))
        .fetch_optional(&mut *tx)
        .await?;
        let Some((title, status, agent_id, pr_url, merged_pr_url)) = task else {
            return Ok(false);
        };
        if status == "cancelled" {
            return Ok(false);
        }
        if let Some(session_id) = followup_session {
            let session = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM agent_sessions
                 WHERE id = $1 AND task_id = $2 AND role = 'task' AND status <> 'archived'
                 FOR UPDATE",
            )
            .bind(session_id)
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?;
            if session.is_none() {
                return Ok(false);
            }
        }
        if let Some(pr) = webhook.as_ref() {
            if pr_url
                .as_deref()
                .is_some_and(|url| TrackedPr::parse(url).is_none_or(|tracked| !tracked.same_identity(result_pr)))
            {
                return Ok(false);
            }
            sqlx::query(
                "UPDATE agent_tasks SET pr_url = COALESCE(pr_url, $2), pr_number = $3
                 WHERE id = $1 AND (pr_url IS NULL OR pr_number IS DISTINCT FROM $3)",
            )
            .bind(task_id)
            .bind(expected_pr_url)
            .bind(pr.number)
            .execute(&mut *tx)
            .await?;
        } else if !pr_url.as_deref().and_then(TrackedPr::parse).is_some_and(|tracked| tracked.same_identity(result_pr)) {
            return Ok(false);
        }
        let expected_pr_url = pr_url.as_deref().unwrap_or(expected_pr_url);
        let tracked_pr = TrackedPr::parse(expected_pr_url).expect("validated pull request");
        let mut observed_at = result.observed_at;
        let runner_result = matches!(result.order, PullRequestOrder::Runner);
        if !merged {
            let ordering = sqlx::query_as::<_, (Value, String, Option<String>, Option<DateTime<Utc>>)>(
                "SELECT data, status, detail,
                        CASE WHEN jsonb_typeof(data->'runner_pr_claimed_at') = 'number'
                     THEN to_timestamp((data->>'runner_pr_claimed_at')::double precision) END
                 FROM task_milestones WHERE task_id = $1 AND kind = 'merge' FOR UPDATE",
            )
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or((Value::Object(Default::default()), String::new(), None, None));
            if runner_result {
                let claim_matches_pr = ordering
                    .0
                    .get("runner_pr_claimed_url")
                    .and_then(Value::as_str)
                    .is_none_or(|url| TrackedPr::parse(url).is_some_and(|claimed| claimed.same_identity(tracked_pr)));
                let matches_claim = match (observed_at.as_ref(), ordering.3.as_ref()) {
                    (Some(observed), Some(claimed))
                        if observed
                            .signed_duration_since(claimed)
                            .num_microseconds()
                            .is_some_and(|difference| difference.abs() <= 1) =>
                    {
                        true
                    }
                    (None, Some(_)) => true,
                    _ => false,
                };
                if !claim_matches_pr || !matches_claim {
                    return Ok(false);
                }
                observed_at = ordering.3;
            }
            if let Some(observed_at) = observed_at {
                match pr_observation_order(&ordering.0, tracked_pr, observed_at) {
                    PrObservationOrder::Older => return Ok(false),
                    PrObservationOrder::Same => {
                        let closed_pr_matches = ordering
                            .0
                            .get("closed_pr_url")
                            .and_then(Value::as_str)
                            .and_then(TrackedPr::parse)
                            .is_some_and(|closed| closed.same_identity(tracked_pr));
                        let closed_at_matches = ordering.0.get("closed_at").and_then(Value::as_f64).is_some_and(|closed_at| {
                            (closed_at - observed_at.timestamp_micros() as f64 / 1_000_000.0).abs() <= 0.000001
                        });
                        if !(status == "failed"
                            && ordering.1 == "failed"
                            && ordering.2.as_deref() == Some("Closed without merging")
                            && closed_pr_matches
                            && closed_at_matches)
                        {
                            return Ok(false);
                        }
                    }
                    PrObservationOrder::Newer => {}
                }
            } else if pr_observation_time(&ordering.0, tracked_pr).is_some() {
                return Ok(false);
            }
        }
        let recorded_merge = merged_pr_url
            .as_deref()
            .and_then(TrackedPr::parse)
            .is_some_and(|recorded| recorded.same_identity(tracked_pr));
        if require_recorded_merge && !recorded_merge {
            return Ok(false);
        }
        if !merged && (status == "done" || recorded_merge) {
            return Ok(false);
        }
        sqlx::query(
            "SELECT p.id FROM agent_tasks c JOIN agent_tasks p ON p.id = c.parent_task_id
             WHERE c.id = $1 FOR NO KEY UPDATE OF p",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let mut close_transition = false;
        if merged {
            sqlx::query(
                "UPDATE agent_tasks SET status = 'done', error_message = NULL, merged_pr_url = $2
                 WHERE id = $1 AND (status <> 'done' OR error_message IS NOT NULL OR merged_pr_url IS DISTINCT FROM $2)",
            )
                .bind(task_id)
                .bind(expected_pr_url)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE task_milestones
                 SET status = CASE WHEN kind = 'merge' THEN 'done' ELSE 'skipped' END,
                     detail = CASE WHEN kind = 'merge' THEN 'Merged' ELSE detail END,
                     done_at = CASE WHEN kind = 'merge' THEN COALESCE(done_at, NOW()) ELSE NULL END,
                     next_check_at = NULL, updated_at = NOW()
                 WHERE task_id = $1
                   AND ((kind = 'merge' AND status <> 'done') OR (kind = 'work' AND status IN ('pending', 'active')))",
            )
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
            if let Some(session_id) = followup_session {
                sqlx::query(
                    "UPDATE session_runs
                     SET status = 'cancelled', error = 'The pull request was already merged.', finished_at = NOW()
                     WHERE session_id = $1 AND kind = 'followup' AND status = 'queued'",
                )
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE agent_sessions SET status = 'completed', updated_at = NOW()
                     WHERE id = $1 AND status <> 'archived'",
                )
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
            }
        } else {
            close_transition = sqlx::query("UPDATE agent_tasks SET status = 'failed', error_message = $2 WHERE id = $1 AND (status <> 'failed' OR error_message IS DISTINCT FROM $2)")
                .bind(task_id)
                .bind(PR_CLOSED_ERROR)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0;
            close_transition |= sqlx::query(
                "UPDATE task_milestones
                 SET status = 'failed', detail = 'Closed without merging',
                     data = jsonb_set(data, '{closed_pr_url}', to_jsonb($2::text))
                       || CASE WHEN $3::timestamptz IS NULL THEN '{}'::jsonb
                         ELSE jsonb_build_object(
                           'closed_at', EXTRACT(EPOCH FROM $3::timestamptz),
                           'recheck_after', EXTRACT(EPOCH FROM $3::timestamptz),
                           'recheck_pr_url', $2::text
                         ) END,
                     done_at = NOW(), next_check_at = NULL, updated_at = NOW()
                 WHERE task_id = $1 AND kind = 'merge'
                   AND (status IN ('pending', 'active')
                     OR status = 'skipped' AND detail = 'No pull request was opened')",
            )
            .bind(task_id)
            .bind(expected_pr_url)
            .bind(observed_at)
            .execute(&mut *tx)
            .await?
            .rows_affected()
                > 0;
            sqlx::query(
                "UPDATE task_milestones
                 SET data = jsonb_set(data, '{closed_pr_url}', to_jsonb($2::text))
                       || jsonb_build_object(
                         'closed_at', EXTRACT(EPOCH FROM $3::timestamptz),
                         'recheck_after', EXTRACT(EPOCH FROM $3::timestamptz),
                         'recheck_pr_url', $2::text
                       ),
                     updated_at = NOW()
                 WHERE task_id = $1 AND kind = 'merge' AND status = 'failed' AND detail = 'Closed without merging'
                   AND $3::timestamptz IS NOT NULL
                   AND COALESCE((data->>'closed_at')::double precision, '-Infinity'::double precision)
                     < EXTRACT(EPOCH FROM $3::timestamptz)",
            )
            .bind(task_id)
            .bind(expected_pr_url)
            .bind(observed_at)
            .execute(&mut *tx)
            .await?;
        }
        let clarification_agents = settle_task_closure(
            &mut tx,
            task_id,
            None,
            if merged { "Quest merged" } else { "Quest failed" },
        )
        .await?;
        let event_type = if merged { "task.merged" } else { "task.pr_closed" };
        let announced = if recorded_merge {
            true
        } else if !merged && close_transition {
            false
        } else {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM world_events WHERE task_id = $1 AND type = $2 AND payload->>'pr_url' = $3)",
            )
            .bind(task_id)
            .bind(event_type)
            .bind(expected_pr_url)
            .fetch_one(&mut *tx)
            .await?
        };
        if let Some(agent_id) = agent_id.filter(|_| !announced) {
            let status = if merged { "done" } else { "failed" };
            let error = (!merged).then_some(PR_CLOSED_ERROR);
            let session_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM agent_sessions WHERE task_id = $1 AND role = 'task' ORDER BY created_at DESC LIMIT 1",
            )
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?;
            let main_session_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active'",
            )
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?;
            self.insert_reply_with(
                &mut *tx,
                NewMessage {
                    agent_id,
                    author_kind: "system",
                    author_user_id: None,
                    author_agent_id: Some(agent_id),
                    body: if merged { "The pull request was merged." } else { "The pull request was closed without merging." },
                    session_id: main_session_id,
                    run_id: None,
                    meta: json!({
                        "kind": "quest",
                        "title": title,
                        "status": status,
                        "pr_url": expected_pr_url,
                        "error": error,
                        "task_id": task_id,
                        "session_id": session_id,
                        "merged": merged,
                    }),
                },
                None,
            )
            .await?;
            let event = NewEvent::new(event_type)
                .agent(agent_id)
                .task(Some(task_id))
                .payload(json!({ "task_id": task_id, "title": title, "pr_url": expected_pr_url, "status": status }));
            emit_in(
                &mut tx,
                &event,
                Some((agent_id, event_type)),
            )
            .await?;
        }
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&clarification_agents).await;
        Ok(true)
    }

    pub async fn settle_merged_quests(&self) -> Result<Vec<Uuid>> {
        let candidates = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT t.id, t.pr_url FROM agent_tasks t
             WHERE t.pr_url IS NOT NULL AND t.status <> 'cancelled'
               AND t.merged_pr_url = t.pr_url
               AND (t.status <> 'done' OR t.error_message IS NOT NULL OR EXISTS (
                 SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.status IN ('pending', 'active')
               ))
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs o
                 WHERE o.kind IN ('task', 'followup', 'review', 'publish') AND o.status IN ('queued', 'running')
                   AND (o.task_id = t.id OR o.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
               )",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut settled = Vec::with_capacity(candidates.len());
        for (task_id, pr_url) in candidates {
            if self
                .settle_pr_inner(
                    task_id,
                    PullRequestResult { url: &pr_url, merged: true, observed_at: None, order: PullRequestOrder::Legacy },
                    None,
                    true,
                    None,
                    None,
                )
                .await?
            {
                settled.push(task_id);
            }
        }
        Ok(settled)
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
               AND (t.merged_pr_url IS NULL OR t.merged_pr_url IS DISTINCT FROM t.pr_url)
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
               AND t.merged_pr_url = t.pr_url",
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
             JOIN session_runs r ON r.id = t.run_id AND r.status = 'cancelled' AND r.agent_id = t.assigned_agent_id AND r.carries_on IS NULL
             JOIN agent_sessions s ON s.id = r.session_id AND s.status <> 'archived'
             WHERE NOT (r.kind = 'review' AND s.asked_by->'compare' IS NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs carrying
                 WHERE carrying.carries_on IS NOT NULL
                   AND (carrying.task_id = t.id OR carrying.session_id IN (SELECT id FROM agent_sessions WHERE task_id = t.id))
               )
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
                 ) OR EXISTS (
                   SELECT 1 FROM session_runs carrying
                   WHERE carrying.carries_on IS NOT NULL
                     AND (carrying.task_id = $1 OR carrying.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))
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
            let clarification_agents = settle_task_closure(&mut tx, task_id, None, "Quest failed").await?;
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
            let _ = self.clear_clarification_waiting(&clarification_agents).await;
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
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        crate::orgs::lock_org_parent(&mut tx, org).await?;
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
        let contextual_agents = settle_deleted_task_clarifications(&mut tx, &[task_id], &note).await?;
        let mut cleanup = clean_up_task(&mut tx, task_id, &note).await?;
        cleanup.clarification_agents.extend(contextual_agents);
        for sub in &subtasks {
            let more = clean_up_task(&mut tx, sub.id, &note).await?;
            cleanup.dropped += more.dropped;
            cleanup.archived += more.archived;
            cleanup.running.extend(more.running);
            cleanup.clarification_agents.extend(more.clarification_agents);
        }
        let payload = json!({
            "title": task.title,
            "status": task.status,
            "by": by,
            "reason": reason,
            "subtasks": subtasks.len(),
            "dropped_runs": cleanup.dropped,
            "stopping_runs": cleanup.running.len(),
        });
        sqlx::query("SELECT world_emit_task_deleted(t, $2) FROM agent_tasks t WHERE t.id = $1")
            .bind(task_id)
            .bind(payload)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task_id).execute(&mut *tx).await?;
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
        let secs = task_time_limit_secs(minutes)?;
        let mut tx = self.pool.begin().await?;
        set_task_time_limit_in(&mut tx, task_id, secs).await?;
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
        if from_status == "done" && (status != "done" || agent != from_agent) {
            clarification_agents.extend(
                settle_contextual_task_clarifications(
                    &mut tx,
                    &[task_id],
                    &format!("Quest moved to {status} by {by}"),
                )
                .await?,
            );
        }
        if status != from_status && matches!(status.as_str(), "done" | "cancelled") {
            let verb = if status == "done" { "marked done" } else { "cancelled" };
            let cleanup = clean_up_task(&mut tx, task_id, &closing_note(verb, by, None)).await?;
            running.extend(cleanup.running);
            clarification_agents.extend(cleanup.clarification_agents);
        } else if status != from_status {
            let note = format!("Quest moved to {status} by {by}");
            clarification_agents.extend(retire_task_continuations(&mut tx, task_id, None, &note).await?);
            if matches!(status.as_str(), "todo" | "planning" | "plan_review" | "review" | "failed") {
                clarification_agents.extend(settle_task_clarifications(&mut tx, task_id, None, None, None, &note).await?);
            }
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
        clarification_agents.sort_unstable();
        clarification_agents.dedup();
        let _ = self.clear_clarification_waiting(&clarification_agents).await;
        Ok(QuestEdited { status, running })
    }

    pub async fn complete_task_run(&self, id: Uuid, run_created_at: DateTime<Utc>) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let pending_prework = sqlx::query_scalar::<_, bool>(
            "SELECT COALESCE(asked_by->>'before_big_work_phase' IN ('pending', 'planning'), FALSE)
             FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if pending_prework {
            return Ok(false);
        }
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
        let (role, status, pending_prework) = sqlx::query_as::<_, (String, String, bool)>(
            "SELECT role, status, COALESCE(asked_by->>'before_big_work_phase' IN ('pending', 'planning'), FALSE)
             FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if pending_prework {
            return Ok(FollowupAdvance::Wait);
        }
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

    pub async fn fail_task_session(&self, task_id: Uuid, session_id: Uuid, error: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let task_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM agent_tasks WHERE id = $1 FOR UPDATE",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        if task_status.as_deref().is_none_or(|status| matches!(status, "done" | "cancelled")) {
            return Ok(false);
        }
        let session = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_sessions
             WHERE id = $1 AND task_id = $2 AND role = 'task' AND status <> 'archived'
             FOR UPDATE",
        )
        .bind(session_id)
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        if session.is_none() {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE agent_tasks SET status = 'failed', error_message = $2
             WHERE id = $1 AND status NOT IN ('done', 'cancelled')",
        )
        .bind(task_id)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_sessions SET status = 'failed', summary = $2, updated_at = NOW()
             WHERE id = $1 AND status <> 'archived'",
        )
        .bind(session_id)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE session_runs
             SET status = 'failed', error = $2, finished_at = NOW()
             WHERE session_id = $1 AND kind = 'followup' AND status = 'queued'",
        )
        .bind(session_id)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        let clarification_agents = settle_task_closure(&mut tx, task_id, None, "Quest failed").await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&clarification_agents).await;
        Ok(true)
    }

    pub async fn request_review(
        &self,
        task_session: &AgentSession,
        reviewer: Option<&WorldAgent>,
        choice: Option<&ReviewerChoice>,
        focus: Option<&str>,
        requested_by: Uuid,
    ) -> Result<SessionRun> {
        let picked = match (choice, reviewer) {
            (None, None) => match self.get_agent(task_session.agent_id).await? {
                Some(agent) => Some(self.reviewer_for(&agent, task_session).await?),
                None => None,
            },
            _ => None,
        };
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
            (None, None) => match (current.reviewer.is_some() || agent_default.is_some(), picked) {
                (false, Some(picked)) => picked,
                _ => current.review_target(agent_default.as_ref()),
            },
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
        let locked_task = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
            "SELECT t.id, t.status, t.pr_url
             FROM agent_tasks t JOIN session_runs r ON r.task_id = t.id
             WHERE r.id = $1 FOR NO KEY UPDATE OF t",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
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
        let task = match (run.task_id, locked_task) {
            (Some(task_id), Some((locked_id, status, pr_url))) if task_id == locked_id => Some((status, pr_url)),
            (Some(_), _) => return Err(RetryRunError::Conflict("the linked quest no longer exists".into())),
            (None, _) => None,
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
        transfer_retry_clarification_resources(&mut tx, run.id, retried.id).await?;
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

    pub async fn delete_agent(&self, id: Uuid) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let Some(org) = sqlx::query_scalar::<_, Uuid>("SELECT org_id FROM agent_profiles WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            tx.commit().await?;
            return Ok(0);
        };
        crate::orgs::lock_org_parent(&mut tx, org).await?;
        sqlx::query(
            "SELECT world_quest_changed(id, 'details') FROM agent_tasks WHERE assigned_agent_id = $1 AND org_id = $2",
        )
        .bind(id)
        .bind(org)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM channel_members WHERE member_kind = 'agent' AND member_id = $1").bind(id).execute(&mut *tx).await?;
        let deleted = sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(id).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(deleted)
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

    pub(crate) async fn clear_clarification_waiting(&self, agents: &[Uuid]) -> Result<()> {
        for agent_id in agents {
            let reset = sqlx::query_scalar::<_, Uuid>(
                "UPDATE agent_profiles SET status = 'idle', status_detail = NULL, updated_at = NOW()
                 WHERE id = $1 AND status = 'waiting'
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_sessions
                     WHERE agent_id = $1 AND plan_state = 'clarifying' AND status <> 'archived'
                   )
                   AND NOT EXISTS (
                     SELECT 1 FROM world_notifications
                     WHERE agent_id = $1 AND kind = 'ask' AND cleared_at IS NULL
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
        self.drop_stale_clarification_continuations(agent_id).await?;
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

    async fn drop_stale_clarification_continuations(&self, agent_id: Uuid) -> Result<()> {
        let items = sqlx::query_as::<_, InboxItem>(
            "SELECT id, agent_id, kind, event_seq, payload, status, attempts, created_at
             FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'task.assigned' AND status IN ('pending', 'processing')
               AND payload ? 'clarification_continuation'",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        for item in items {
            self.clarification_continuation_open(&item).await?;
        }
        Ok(())
    }

    pub async fn clarification_continuation_open(&self, item: &InboxItem) -> Result<bool> {
        if item.payload.get("clarification_continuation").is_none() {
            return Ok(true);
        }
        if clarification_continuation_unbound(item) {
            let mut tx = self.pool.begin().await?;
            let current = sqlx::query_as::<_, (Uuid, String, String, Value)>(
                "SELECT agent_id, kind, status, payload FROM agent_inbox WHERE id = $1 FOR UPDATE",
            )
            .bind(item.id)
            .fetch_optional(&mut *tx)
            .await?;
            let valid = current.as_ref().is_some_and(|(agent_id, kind, status, payload)| {
                clarification_continuation_unbound_matches(item, *agent_id, kind, status, payload)
            });
            tx.commit().await?;
            return Ok(valid);
        }
        let Some((_, task_id, _, _)) = clarification_continuation_meta(&item.payload) else {
            sqlx::query(
                "UPDATE agent_inbox SET status = 'done', error = 'Invalid clarification continuation', done_at = NOW()
                 WHERE id = $1 AND status IN ('pending', 'processing') AND payload ? 'clarification_continuation'",
            )
            .bind(item.id)
            .execute(&self.pool)
            .await?;
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, (String, Option<Uuid>)>(
            "SELECT status, assigned_agent_id FROM agent_tasks WHERE id = $1 FOR UPDATE",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let current = sqlx::query_as::<_, (Uuid, String, String, Value)>(
            "SELECT agent_id, kind, status, payload FROM agent_inbox WHERE id = $1 FOR UPDATE",
        )
        .bind(item.id)
        .fetch_optional(&mut *tx)
        .await?;
        let valid = match (task, current.as_ref()) {
            (Some((status, assigned)), Some((agent_id, kind, inbox_status, payload))) => {
                clarification_continuation_matches(item, &status, assigned, *agent_id, kind, inbox_status, payload)
            }
            _ => false,
        };
        if !valid {
            sqlx::query(
                "UPDATE agent_inbox SET status = 'done', error = 'Quest changed after the clarification answer', done_at = NOW()
                 WHERE id = $1 AND status IN ('pending', 'processing') AND payload ? 'clarification_continuation'",
            )
            .bind(item.id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(valid)
    }

    pub async fn drop_started_assignments(&self, agent_id: Uuid) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE agent_inbox i SET status = 'done', error = 'Already started', done_at = NOW()
             WHERE i.agent_id = $1 AND i.status = 'pending' AND i.kind = 'task.assigned'
               AND NOT i.payload ? 'clarification_continuation'
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
            "UPDATE agent_inbox SET status = $2, error = $3, done_at = NOW()
             WHERE id = ANY($1) AND (status = 'processing' OR NOT (payload ? 'clarification_continuation'))",
        )
        .bind(ids)
        .bind(status)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_inbox_now(&self, ids: &[i64], error: &str) -> Result<()> {
        sqlx::query(&requeue_inbox_sql("id = ANY($1)"))
            .bind(ids)
            .bind(error)
            .bind(Utc::now())
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

    pub async fn requeue_clarification_starts(&self, ids: &[i64]) -> Result<()> {
        sqlx::query(&requeue_inbox_sql("id = ANY($1) AND status = 'processing'"))
            .bind(ids)
            .bind("Could not start the clarification continuation")
            .bind(Option::<DateTime<Utc>>::None)
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
        let main = self.main_session(agent_id).await?;
        let memories = match &main {
            Some(main) => self.queued_fork_memories(agent_id, main.id, QUEST_FORKS, crate::context::FreshWhy::Overflow).await?,
            None => Vec::new(),
        };
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
            unfork_queued_runs(&mut tx, id, QUEST_FORKS, &memories).await?;
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

    pub async fn queued_fork_memories(
        &self,
        agent_id: Uuid,
        main: Uuid,
        kinds: &[&str],
        why: crate::context::FreshWhy,
    ) -> Result<Vec<(String, String)>> {
        let fork_backends = sqlx::query_scalar::<_, String>(&format!(
            "SELECT DISTINCT q.backend FROM session_runs r
             JOIN agent_sessions q ON q.id = r.session_id
             WHERE {QUEUED_FORK_SQL}"
        ))
        .bind(main)
        .bind(kinds)
        .fetch_all(&self.pool)
        .await?;
        let mut memories = Vec::with_capacity(fork_backends.len());
        if !fork_backends.is_empty() {
            if let Some(agent) = self.get_agent(agent_id).await? {
                for fork_backend in fork_backends {
                    let intro = why.intro(&agent.name, &fork_backend);
                    memories.push((self.fresh_start_memory(&agent, &intro).await?, fork_backend));
                }
            }
        }
        Ok(memories)
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

    pub async fn set_session_summary(&self, id: Uuid, summary: &str) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE agent_sessions SET summary = $2, updated_at = NOW()
             WHERE id = $1 AND status <> 'archived'",
        )
        .bind(id)
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
        let mut conn = self.pool.acquire().await?;
        create_run_in(&mut conn, new).await
    }

    pub async fn create_clarification_run(&self, item: &InboxItem, new: NewRun<'_>) -> Result<Option<SessionRun>> {
        let unbound = clarification_continuation_unbound(item);
        let task_id = clarification_continuation_meta(&item.payload)
            .map(|(_, task_id, _, _)| task_id)
            .or_else(|| unbound.then_some(new.task_id).flatten());
        let Some(task_id) = task_id else {
            return Ok(None);
        };
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, (String, Option<Uuid>)>(
            "SELECT status, assigned_agent_id FROM agent_tasks WHERE id = $1 FOR NO KEY UPDATE",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let reopening_done = task.as_ref().is_some_and(|(status, _)| status == "done");
        let bound_pending = unbound && clarification_continuation_pending_with(&mut tx, task_id, item.agent_id).await?;
        let continuation_kind = if bound_pending {
            None
        } else {
            match task.as_ref() {
                Some((status, assigned)) => {
                    clarification_continuation_current_for_task(&mut tx, item, task_id, status, *assigned).await?
                }
                None => None,
            }
        };
        let valid = continuation_kind.is_some()
            && new.task_id == Some(task_id)
            && new.agent_id == item.agent_id
            && new.status == "queued";
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs
               WHERE task_id = $1 AND kind IN ('task', 'followup', 'review', 'publish') AND status IN ('queued', 'running')
             )",
        )
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        if active && !unbound {
            sqlx::query("UPDATE agent_sessions SET status = 'archived', updated_at = NOW() WHERE id = $1")
                .bind(new.session_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            anyhow::bail!("quest work already started")
        }
        if !valid || active {
            let error = if active { "Already started" } else { "Quest changed after the clarification answer" };
            if !unbound {
                finish_clarification_continuation(&mut tx, item.id, Some(error)).await?;
            }
            sqlx::query("UPDATE agent_sessions SET status = 'archived', updated_at = NOW() WHERE id = $1")
                .bind(new.session_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(None);
        }
        let run = create_run_in(&mut tx, new).await?;
        link_clarification_continuation(&mut tx, run.id, item).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress', error_message = NULL WHERE id = $1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        let contextual_agents = if reopening_done {
            settle_contextual_task_clarifications(&mut tx, &[task_id], "Quest resumed").await?
        } else {
            Vec::new()
        };
        finish_clarification_continuation(&mut tx, item.id, None).await?;
        tx.commit().await?;
        let _ = self.clear_clarification_waiting(&contextual_agents).await;
        Ok(Some(run))
    }

    pub async fn claim_queued_run(&self, worker: &str) -> Result<Option<SessionRun>> {
        let mut tx = self.pool.begin().await?;
        let candidate = sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT q.id FROM session_runs q
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
                 AND {folder_free}
                 AND (NOT {writes} OR pg_try_advisory_xact_lock({folder_lock}))
               ORDER BY {urgent} DESC, q.created_at FOR UPDATE OF s SKIP LOCKED LIMIT 1",
            urgent = urgent_run_sql("q"),
            folder_free = folder_free_sql("q", "s"),
            writes = writes_folder_sql("q", "s"),
            folder_lock = folder_lock_sql("s.runner_id", "s.working_dir"),
        ))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(candidate) = candidate else {
            tx.rollback().await?;
            return Ok(None);
        };
        let claimed = sqlx::query_as::<_, SessionRun>(&format!(
            "UPDATE session_runs q SET status = 'running', claimed_by = $1, started_at = NOW(), heartbeat_at = NOW()
             FROM agent_sessions s, agent_profiles a
             WHERE q.id = $2 AND q.status = 'queued' AND q.runner_id IS NULL
               AND s.id = q.session_id AND s.runner_id IS NULL AND a.id = q.agent_id AND a.paused_reason IS NULL
               AND NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = q.session_id AND r.status = 'running')
               AND (q.kind <> 'followup' OR s.cli_session_id IS NOT NULL)
               AND (q.not_before IS NULL OR q.not_before <= NOW()) AND {folder_free}
             RETURNING q.id, q.session_id, q.agent_id, q.task_id, q.kind, q.status, q.fork_from, q.prompt, q.output, q.error,
                       q.timeout_secs, q.cost_usd, q.input_tokens, q.cached_input_tokens, q.output_tokens,
                       q.created_at, q.started_at, q.finished_at",
            folder_free = folder_free_sql("q", "s"),
        ))
        .bind(worker)
        .bind(candidate)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(claimed)
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

    pub async fn quest_clarification(
        &self,
        task_id: Uuid,
        agent_id: Uuid,
        owner_user_id: Uuid,
    ) -> Result<Option<AgentMessage>> {
        let mut conn = self.pool.acquire().await?;
        quest_clarification_with(&mut conn, task_id, agent_id, owner_user_id).await
    }

    pub async fn run_answer_payloads(&self, run_id: Uuid) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar::<_, Value>(
            "WITH run_ids AS (
               SELECT current_run.id, current_run.session_id, current_run.agent_id, current_run.task_id, 0 AS lineage
               FROM session_runs current_run WHERE current_run.id = $1
               UNION ALL
               SELECT source_run.id, source_run.session_id, source_run.agent_id, source_run.task_id, 1 AS lineage
               FROM session_runs current_run
               JOIN agent_sessions current_session ON current_session.id = current_run.session_id
               JOIN session_runs source_run
                 ON source_run.session_id::text = current_session.asked_by->>'best_of_prework_session_id'
                AND source_run.task_id = current_run.task_id AND source_run.agent_id = current_run.agent_id
               WHERE current_run.id = $1
             ), resources AS (
               SELECT i.payload, ids.lineage, 0 AS source, i.id::text AS resource_id,
                      i.id AS inbox_order, NULL::uuid AS message_order
               FROM run_ids ids
               JOIN runner_run_inbox x ON x.run_id = ids.id
               JOIN agent_inbox i ON i.id = x.inbox_id
               WHERE i.agent_id = ids.agent_id
                 AND (ids.lineage = 0 OR i.status = 'done' AND i.payload ? 'clarification_continuation')
               UNION ALL
               SELECT answer.meta, ids.lineage, 1 AS source, answer.id::text AS resource_id,
                      NULL::bigint AS inbox_order, answer.id AS message_order
               FROM run_ids ids
               JOIN agent_messages answer ON answer.run_id = ids.id
               WHERE answer.agent_id = ids.agent_id
                 AND answer.author_kind = 'user' AND answer.meta ? 'answers'
               AND EXISTS (
                 SELECT 1 FROM agent_messages card
                 JOIN agent_sessions card_session
                   ON card_session.id::text = card.meta->'clarification'->>'session_id'
                  AND card_session.agent_id = ids.agent_id
                  AND card_session.task_id = ids.task_id
                 WHERE card.id::text = answer.meta->>'answers' AND card.agent_id = answer.agent_id
                   AND card.author_kind <> 'user'
                   AND card.meta->'clarification'->>'task_id' = ids.task_id::text
               )
             ), unique_resources AS (
               SELECT DISTINCT ON (source, resource_id)
                      payload, lineage, source, inbox_order, message_order
               FROM resources
               ORDER BY source, resource_id, lineage
             )
             SELECT payload FROM unique_resources ORDER BY lineage, source, inbox_order, message_order",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn insert_message(&self, m: NewMessage<'_>) -> Result<AgentMessage> {
        self.insert_reply(m, None).await
    }

    async fn insert_main_clarification(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        task_id: Option<Uuid>,
        agent_name: &str,
        choices: &[String],
        message: NewMessage<'_>,
    ) -> Result<AgentMessage> {
        let agent_id = message.agent_id;
        let session_id = message.session_id;
        let run_id = message.run_id;
        let context_task_id = message
            .meta
            .get("clarify_request")
            .and_then(main_clarification_context_task);
        let body = message.body.to_string();
        let posted = self.insert_reply_with(&mut **tx, message, None).await?;
        let mut created = NewEvent::new("message.created")
            .agent(agent_id)
            .payload(json!({
                "message_id": posted.id,
                "author": agent_name,
                "author_kind": "agent",
                "body": body,
                "ask": true,
            }));
        let mut attention = NewEvent::new("attention.requested")
            .agent(agent_id)
            .payload(json!({
                "text": body,
                "agent": agent_name,
                "message_id": posted.id,
                "choices": choices,
                "clarification": true,
            }));
        if let Some(task_id) = task_id.or(context_task_id) {
            created = created.task(Some(task_id));
            attention = attention.task(Some(task_id));
        }
        if let Some(session_id) = session_id {
            created = created.session(session_id);
            attention = attention.session(session_id);
        }
        if let Some(run_id) = run_id {
            created = created.run(run_id);
            attention = attention.run(run_id);
        }
        emit_in(tx, &created, None).await?;
        emit_in(tx, &attention, None).await?;
        let changed = sqlx::query(
            "UPDATE agent_profiles SET status = 'waiting', status_detail = $2, updated_at = NOW()
             WHERE id = $1 AND (status IS DISTINCT FROM 'waiting' OR status_detail IS DISTINCT FROM $2)",
        )
        .bind(agent_id)
        .bind(crate::quests::CLARIFICATION_WAITING_DETAIL)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if changed > 0 {
            emit_in(
                tx,
                &NewEvent::new("agent.status")
                    .agent(agent_id)
                    .payload(json!({ "status": "waiting", "detail": crate::quests::CLARIFICATION_WAITING_DETAIL })),
                None,
            )
            .await?;
        }
        Ok(posted)
    }

    async fn post_clarification_channel_notice(&self, agent_id: Uuid, card_id: Uuid, route: &Value) -> Result<()> {
        let Some((channel_id, _, root_id)) = clarification_channel_route(route) else {
            return Ok(());
        };
        let Some(channel) = self.get_channel(channel_id).await? else {
            return Ok(());
        };
        self.post_channel_reply(
            &channel,
            crate::channels::Author::Agent(agent_id),
            &format!("[Answer privately](msg:{card_id})"),
            json!({ "no_wake": true }),
            Some(root_id),
        )
        .await?;
        Ok(())
    }

    pub async fn post_main_clarification(
        &self,
        task_id: Uuid,
        agent_name: &str,
        choices: &[String],
        message: NewMessage<'_>,
    ) -> Result<Option<AgentMessage>> {
        let (_, posted) = self
            .post_main_clarification_outcome(task_id, agent_name, choices, message)
            .await?;
        Ok(posted)
    }

    async fn post_main_clarification_outcome(
        &self,
        task_id: Uuid,
        agent_name: &str,
        choices: &[String],
        message: NewMessage<'_>,
    ) -> Result<(MainClarificationPost, Option<AgentMessage>)> {
        let Some(origin) = message.meta.get("clarify_request") else {
            return Ok((MainClarificationPost::Refused, None));
        };
        let linked_task = main_clarification_task(origin);
        if linked_task != Some(task_id) {
            return Ok((MainClarificationPost::Refused, None));
        }
        let resumed = main_clarification_resumes(origin);
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, (String, Option<Uuid>, Option<Uuid>)>(
            "SELECT t.status, t.assigned_agent_id, a.owner_user_id
             FROM agent_tasks t LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
             WHERE t.id = $1 FOR NO KEY UPDATE OF t",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((status, assigned, Some(owner))) = task else {
            return Ok((MainClarificationPost::Refused, None));
        };
        let source_id = main_clarification_source(origin);
        if !main_clarification_source_valid(&mut tx, origin, message.agent_id, Some(task_id)).await? {
            return Ok((MainClarificationPost::Refused, None));
        }
        let replayed = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM agent_messages
                   WHERE agent_id = $1 AND author_kind = 'agent' AND meta->>'kind' = 'ask'
                     AND meta->'clarify_request'->>'task_id' = $2::text
                     AND meta->'clarify_request'->>'source_inbox_id' = $3::text
                 )",
            )
            .bind(message.agent_id)
            .bind(task_id)
            .bind(source_id)
            .fetch_one(&mut *tx)
            .await?
        } else if let Some(run_id) = message.run_id {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                   SELECT 1 FROM agent_messages
                   WHERE agent_id = $1 AND run_id = $2 AND author_kind = 'agent' AND meta->>'kind' = 'ask'
                     AND meta->'clarify_request'->>'task_id' = $3::text
                 )",
            )
            .bind(message.agent_id)
            .bind(run_id)
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?
        } else {
            false
        };
        if replayed
            || clarification_continuation_pending_with(&mut tx, task_id, message.agent_id).await?
            || quest_clarification_with(&mut tx, task_id, message.agent_id, owner).await?.is_some()
        {
            return Ok((MainClarificationPost::AlreadyHandled, None));
        }
        if !main_clarification_open(&status, assigned, message.agent_id, resumed) {
            return Ok((MainClarificationPost::Refused, None));
        }
        let run_id = message.run_id;
        let work_started = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM session_runs work
               WHERE work.kind IN ('task', 'followup')
                 AND (work.task_id = $1 OR work.session_id IN (SELECT id FROM agent_sessions WHERE task_id = $1))
                 AND (
                   work.status IN ('queued', 'running')
                   OR ($2::uuid IS NOT NULL AND work.created_at > (SELECT created_at FROM session_runs WHERE id = $2))
                 )
             )",
        )
        .bind(task_id)
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
        if work_started {
            return Ok((MainClarificationPost::Refused, None));
        }
        let posted = self
            .insert_main_clarification(&mut tx, Some(task_id), agent_name, choices, message)
            .await?;
        tx.commit().await?;
        Ok((MainClarificationPost::Posted, Some(posted)))
    }

    pub async fn post_main_request_clarification(
        &self,
        agent_name: &str,
        choices: &[String],
        message: NewMessage<'_>,
    ) -> Result<MainClarificationPost> {
        let Some(origin) = message.meta.get("clarify_request") else {
            return Ok(MainClarificationPost::Refused);
        };
        if let Some(task_id) = main_clarification_task(origin) {
            return self
                .post_main_clarification_outcome(task_id, agent_name, choices, message)
                .await
                .map(|(outcome, _)| outcome);
        }
        let Some(source_id) = main_clarification_source(origin) else {
            return Ok(MainClarificationPost::Refused);
        };
        let channel_route = origin.get("clarification_channel").cloned();
        let agent_id = message.agent_id;
        let mut tx = self.pool.begin().await?;
        if let Some(task_id) = main_clarification_context_task(origin) {
            let valid_context = sqlx::query_scalar::<_, bool>(
                "SELECT i.created_at >= COALESCE(t.completed_at, t.updated_at, t.created_at)
                 FROM agent_tasks t
                 JOIN agent_inbox i ON i.id = $2 AND i.agent_id = $3
                 WHERE t.id = $1 AND t.status = 'done' AND t.assigned_agent_id = $3
                   AND i.kind = 'message' AND i.payload->>'task_id' = $1::text
                 FOR NO KEY UPDATE OF t FOR UPDATE OF i",
            )
            .bind(task_id)
            .bind(source_id)
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
            if !valid_context {
                return Ok(MainClarificationPost::Refused);
            }
        }
        if !main_clarification_source_valid(&mut tx, origin, message.agent_id, None).await? {
            return Ok(MainClarificationPost::Refused);
        }
        let replayed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND author_kind = 'agent' AND meta->>'kind' = 'ask'
                 AND meta->'clarify_request'->>'source_inbox_id' = $2::text
             )",
        )
        .bind(message.agent_id)
        .bind(source_id)
        .fetch_one(&mut *tx)
        .await?;
        if replayed {
            return Ok(MainClarificationPost::AlreadyHandled);
        }
        let posted = self.insert_main_clarification(&mut tx, None, agent_name, choices, message).await?;
        tx.commit().await?;
        if let Some(route) = channel_route {
            self.post_clarification_channel_notice(agent_id, posted.id, &route).await?;
        }
        Ok(MainClarificationPost::Posted)
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
                     context_sent = CASE WHEN $3 THEN '{}'::jsonb ELSE context_sent END,
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
                 rules_hash = NULL, context_sent = '{}'::jsonb, context_tokens = 0, updated_at = NOW()
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
             AND m.kind IN ('chat', 'needs_you') AND NOT (m.meta ? 'room_only')
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
        let patterns = event_type_patterns(types);
        let mut rows = sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM world_events
             WHERE (cardinality($1::text[]) = 0 OR type LIKE ANY($1))
               AND ($2::uuid IS NULL OR agent_id = $2)
               AND ($4::uuid IS NULL OR org_id = $4)
               AND (cardinality($1::text[]) > 0 OR COALESCE(payload->>'quiet', '') <> 'true')
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

    pub async fn mcp_agent_feed(
        &self,
        org: Uuid,
        caller: Uuid,
        owner: Option<Uuid>,
        owned: &[Uuid],
        types: &[String],
        limit: i64,
    ) -> Result<Vec<WorldEvent>> {
        let patterns = event_type_patterns(types);
        let org_visible: Vec<String> = crate::event_scope::ORG_VISIBLE
            .iter()
            .map(|kind| if kind.ends_with('.') { format!("{kind}%") } else { (*kind).to_string() })
            .collect();
        let loka_private: Vec<String> = crate::loka::PRIVATE_PREFIXES.iter().map(|kind| format!("{kind}%")).collect();
        let mut rows = sqlx::query_as::<_, WorldEvent>(
            "SELECT e.* FROM world_events e
             CROSS JOIN LATERAL (
               SELECT COALESCE(
                 e.related_task_id,
                 world_event_related_task_id(e.task_id, e.run_id, e.session_id, e.payload)
               ) AS task_id
             ) event_quest
             LEFT JOIN agent_tasks quest ON quest.id = event_quest.task_id
             WHERE (cardinality($1::text[]) = 0 OR e.type LIKE ANY($1))
               AND e.org_id = $2
               AND (e.agent_id IS NULL
                 OR e.agent_id = ANY($5)
                 OR (e.quest_scope IS NOT NULL AND e.type = 'task.changed'))
               AND (cardinality($1::text[]) > 0 OR COALESCE(e.payload->>'quiet', '') <> 'true')
               AND ((e.type IN ('task.changed', 'task.deleted')
                     AND (e.quest_scope IS NOT NULL
                       OR (e.agent_id IS NULL AND jsonb_typeof(e.payload->'audience') = 'array')))
                 OR (event_quest.task_id IS NULL AND e.type NOT LIKE 'task.%')
                 OR (quest.org_id = e.org_id
                   AND CASE WHEN $4::uuid IS NULL THEN quest.assigned_agent_id = $3
                     ELSE EXISTS (
                       SELECT 1 FROM agent_profiles quest_agent
                       WHERE quest_agent.id = quest.assigned_agent_id AND quest_agent.owner_user_id = $4)
                       OR (quest.assigned_agent_id IS NULL AND (quest.user_id = $4 OR quest.assignee_user_id = $4))
                   END))
               AND NOT EXISTS (
                 SELECT 1 FROM channels c
                 WHERE c.id = world_safe_uuid(e.payload->>'channel_id') AND c.kind <> 'channel'
                   AND NOT EXISTS (
                     SELECT 1 FROM channel_members m
                     WHERE m.channel_id = c.id
                       AND ((m.member_kind = 'user' AND m.member_id = $4)
                         OR (m.member_kind = 'agent' AND m.member_id = $3))))
               AND CASE WHEN $4::uuid IS NULL THEN
                 e.type NOT LIKE 'slack.%'
                 AND NOT (e.type IN ('task.changed', 'task.deleted')
                          AND (e.quest_scope IS NOT NULL OR e.agent_id IS NULL)
                          AND NOT (e.type = 'task.deleted' AND e.quest_scope IS NOT NULL AND e.agent_id = $3))
                 AND (e.type LIKE ANY($6) OR e.agent_id = $3)
               ELSE
                 NOT (((e.type LIKE 'slack.%' OR e.type LIKE 'todo.%' OR e.type LIKE 'github.%')
                       AND e.payload->>'owner_user_id' IS DISTINCT FROM $4::text)
                   OR (e.type LIKE 'timer.%' AND COALESCE(jsonb_typeof(e.payload->'owner_user_id') = 'string', false)
                       AND e.payload->>'owner_user_id' IS DISTINCT FROM $4::text))
                 AND NOT (
                   e.payload->>'owner_user_id' IS DISTINCT FROM $4::text
                   AND NOT (e.type LIKE 'team.%' AND COALESCE(e.payload->'audience' ? $4::text, false))
                   AND ((e.type LIKE ANY($7)
                         AND NOT (e.type LIKE ANY(ARRAY['region.%', 'report.%', 'presence.%'])
                                  AND COALESCE(e.payload->>'visibility' = 'public', false)))
                     OR (e.type LIKE 'item.%' AND COALESCE(e.payload->'private' = 'true'::jsonb, false))))
                 AND CASE
                   WHEN e.agent_id IS NOT NULL AND world_safe_uuid(e.payload->>'thread_user_id') IS NOT NULL
                     THEN world_safe_uuid(e.payload->>'thread_user_id') = $4
                   WHEN e.agent_id IS NOT NULL AND (e.type LIKE 'message.%' OR e.type LIKE 'messages.%')
                     THEN CASE WHEN e.actor_user_id IS NOT NULL THEN e.actor_user_id = $4 ELSE e.agent_id = ANY($5) END
                   WHEN e.quest_scope IS NOT NULL AND e.type IN ('task.changed', 'task.deleted')
                     THEN CASE WHEN e.type = 'task.deleted'
                       THEN world_quest_event_detail_allows_in(e.quest_scope, $4, e.org_id)
                       ELSE world_quest_event_scope_allows_in(e.quest_scope, $4, e.org_id)
                     END
                   WHEN e.agent_id IS NULL AND e.type IN ('task.changed', 'task.deleted')
                     THEN COALESCE(e.payload->'audience' ? $4::text, false)
                   WHEN e.type LIKE ANY($6) THEN true
                   ELSE e.actor_user_id = $4
                     OR e.payload->>'owner_user_id' = $4::text
                     OR COALESCE(e.payload->'audience' ? $4::text, false)
                     OR e.agent_id = ANY($5)
                 END
               END
             ORDER BY e.seq DESC LIMIT $8",
        )
        .bind(&patterns)
        .bind(org)
        .bind(caller)
        .bind(owner)
        .bind(owned)
        .bind(&org_visible)
        .bind(&loka_private)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        if let Some(viewer) = owner {
            rows = rows
                .into_iter()
                .map(|event| crate::event_scope::card_invalidation_for_viewer(event, viewer, false, false))
                .collect();
        }
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
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, kind, task_id, action, on_event, fired",
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
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, kind, task_id, action, on_event, fired",
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

async fn reopen_failed_session(tx: &mut Transaction<'_, Postgres>, session_id: Uuid) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE agent_sessions s SET status = 'active', review_rounds = 0, updated_at = NOW()
         WHERE s.id = $1 AND s.role = 'task' AND s.status = 'failed'
           AND (s.attempt_state IS NULL OR s.attempt_state = 'won')
           AND NOT EXISTS (SELECT 1 FROM agent_tasks t WHERE t.id = s.task_id AND t.status = 'cancelled')",
    )
    .bind(session_id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
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
    let clarification_agents = settle_task_closure(tx, task_id, Some(agent_id), note).await?;
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
    let clarification_agents = settle_task_closure(tx, task_id, None, note).await?;
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

async fn retire_task_continuations(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    note: &str,
) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "UPDATE agent_inbox SET status = 'done', error = $3, done_at = NOW()
         WHERE kind IN ('task.assigned', 'message') AND status IN ('pending', 'processing')
           AND (payload->>'task_id' = $1::text
             OR payload->'clarification_waiting_for'->>'task_id' = $1::text)
           AND payload ? 'clarification_continuation'
           AND ($2::uuid IS NULL OR agent_id = $2)
         RETURNING agent_id",
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(note)
    .fetch_all(&mut **tx)
    .await?)
}

pub(crate) async fn settle_task_closure(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    note: &str,
) -> Result<Vec<Uuid>> {
    let task_ids = match agent_id {
        Some(_) => vec![task_id],
        None => {
            sqlx::query_scalar::<_, Uuid>(
                "WITH RECURSIVE closed(id, parent_task_id) AS (
                   SELECT id, parent_task_id FROM agent_tasks WHERE id = $1
                   UNION
                   SELECT parent.id, parent.parent_task_id
                   FROM agent_tasks parent JOIN closed child ON parent.id = child.parent_task_id
                   WHERE parent.status = 'done' AND parent.completed_at = NOW()
                 )
                 SELECT id FROM closed",
            )
            .bind(task_id)
            .fetch_all(&mut **tx)
            .await?
        }
    };
    let mut agents = Vec::new();
    for closed_task in task_ids {
        let close_note = if closed_task == task_id { note } else { "All quest parts finished" };
        agents.extend(settle_task_clarifications(tx, closed_task, agent_id, None, None, close_note).await?);
        agents.extend(retire_task_continuations(tx, closed_task, agent_id, close_note).await?);
    }
    agents.sort_unstable();
    agents.dedup();
    Ok(agents)
}

pub async fn settle_task_clarification_siblings(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    agent_id: Uuid,
    keep_card: Uuid,
    keep_session: Option<Uuid>,
    note: &str,
) -> Result<Vec<Uuid>> {
    settle_task_clarifications(tx, task_id, Some(agent_id), Some(keep_card), keep_session, note).await
}

async fn settle_task_clarifications(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    keep_card: Option<Uuid>,
    keep_session: Option<Uuid>,
    note: &str,
) -> Result<Vec<Uuid>> {
    let cards = sqlx::query_as::<_, (Uuid, Option<Uuid>, Uuid, Uuid, Uuid)>(
        "SELECT m.id,
                CASE WHEN m.meta->'clarification' IS NOT NULL THEN s.id ELSE m.session_id END,
                m.agent_id, a.owner_user_id, $1::uuid
         FROM agent_messages m
         JOIN agent_profiles a ON a.id = m.agent_id AND a.owner_user_id IS NOT NULL
         LEFT JOIN agent_sessions s ON s.id::text = m.meta->'clarification'->>'session_id' AND s.agent_id = m.agent_id
         WHERE COALESCE(m.meta->'clarification'->>'task_id', m.meta->'clarify_request'->>'task_id') = $1::uuid::text
           AND ($2::uuid IS NULL OR m.agent_id = $2)
           AND ($3::uuid IS NULL OR m.id <> $3)
           AND (m.meta->'clarify_request' IS NOT NULL OR (s.task_id = $1 AND s.plan_state = 'clarifying'))
           AND NOT EXISTS (SELECT 1 FROM agent_messages answer WHERE answer.agent_id = m.agent_id AND answer.meta->>'answers' = m.id::text)
         ORDER BY m.created_at FOR UPDATE OF m",
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(keep_card)
    .fetch_all(&mut **tx)
    .await?;
    let mut agents = settle_clarification_cards(tx, cards, note).await?;
    agents.extend(sqlx::query_scalar::<_, Uuid>(
        "UPDATE agent_sessions SET plan_state = NULL, updated_at = NOW()
         WHERE task_id = $1 AND ($2::uuid IS NULL OR agent_id = $2)
           AND ($3::uuid IS NULL OR id <> $3) AND plan_state = 'clarifying'
         RETURNING agent_id",
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(keep_session)
    .fetch_all(&mut **tx)
    .await?);
    agents.sort_unstable();
    agents.dedup();
    Ok(agents)
}

pub(crate) async fn settle_deleted_task_clarifications(
    tx: &mut Transaction<'_, Postgres>,
    task_ids: &[Uuid],
    note: &str,
) -> Result<Vec<Uuid>> {
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let locked = sqlx::query_scalar::<_, Uuid>(
        "WITH RECURSIVE deleted(id) AS (
           SELECT id FROM agent_tasks WHERE id = ANY($1)
           UNION
           SELECT child.id FROM agent_tasks child JOIN deleted parent ON child.parent_task_id = parent.id
         )
         SELECT task.id FROM agent_tasks task JOIN deleted ON deleted.id = task.id
         ORDER BY task.id FOR UPDATE OF task",
    )
    .bind(task_ids)
    .fetch_all(&mut **tx)
    .await?;
    if locked.is_empty() {
        return Ok(Vec::new());
    }
    settle_contextual_task_clarifications(tx, &locked, note).await
}

pub(crate) async fn settle_contextual_task_clarifications(
    tx: &mut Transaction<'_, Postgres>,
    task_ids: &[Uuid],
    note: &str,
) -> Result<Vec<Uuid>> {
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let cards = sqlx::query_as::<_, (Uuid, Option<Uuid>, Uuid, Uuid, Uuid)>(
        "SELECT m.id, m.session_id, m.agent_id, a.owner_user_id,
                world_safe_uuid(m.meta->'clarify_request'->>'context_task_id')
         FROM agent_messages m
         JOIN agent_profiles a ON a.id = m.agent_id AND a.owner_user_id IS NOT NULL
         WHERE m.author_kind <> 'user'
           AND world_safe_uuid(m.meta->'clarify_request'->>'context_task_id') = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM agent_messages answer WHERE answer.agent_id = m.agent_id AND answer.meta->>'answers' = m.id::text)
         ORDER BY m.created_at FOR UPDATE OF m",
    )
    .bind(task_ids)
    .fetch_all(&mut **tx)
    .await?;
    settle_clarification_cards(tx, cards, note).await
}

async fn settle_clarification_cards(
    tx: &mut Transaction<'_, Postgres>,
    cards: Vec<(Uuid, Option<Uuid>, Uuid, Uuid, Uuid)>,
    note: &str,
) -> Result<Vec<Uuid>> {
    let mut agents = Vec::new();
    for (card_id, session_id, agent_id, owner, task_id) in cards {
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
        let mut event = NewEvent::new("message.created")
            .agent(agent_id)
            .task(Some(task_id))
            .actor(Some(owner))
            .payload(json!({
                "message_id": message_id,
                "author": "Loka",
                "author_kind": "user",
                "body": note,
                "quiet": true,
                "answer_card": card_id,
            }));
        if let Some(session_id) = session_id {
            event = event.session(session_id);
        }
        emit_in(tx, &event, None).await?;
        agents.push(agent_id);
    }
    agents.sort_unstable();
    agents.dedup();
    Ok(agents)
}

pub const RUN_END_EVENTS: [&str; 3] = ["run.completed", "run.failed", "run.timed_out"];

pub(crate) async fn fail_completed_run_in(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    kind: &str,
    error: &str,
    diagnosis: &crate::run_causes::Diagnosis,
) -> Result<bool> {
    let flipped = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        "UPDATE session_runs SET status = 'failed', error = $2, cause = $3, diagnosis = $4
         WHERE id = $1 AND status = 'completed'
         RETURNING agent_id, session_id, task_id",
    )
    .bind(run_id)
    .bind(error)
    .bind(diagnosis.cause.as_str())
    .bind(serde_json::to_value(diagnosis)?)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((agent_id, session_id, task_id)) = flipped else {
        return Ok(false);
    };
    if !run_end_announced(tx, run_id).await? {
        let mut payload = crate::cli_errors::failure_payload(task_id, error, session_id, kind);
        diagnosis.merge_into(&mut payload);
        payload["quiet"] = json!(true);
        let event = NewEvent::new("run.failed").agent(agent_id).session(session_id).run(run_id).task(task_id).payload(payload);
        emit_in(tx, &event, None).await?;
    }
    Ok(true)
}

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
pub(crate) mod tests {
    use std::collections::HashSet;
    use std::str::FromStr;

    pub(crate) fn test_connections() -> u32 {
        std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(2)
    }

    pub(crate) fn steer_db() -> &'static tokio::sync::RwLock<()> {
        static STEER_DB: std::sync::OnceLock<tokio::sync::RwLock<()>> = std::sync::OnceLock::new();
        STEER_DB.get_or_init(|| tokio::sync::RwLock::new(()))
    }

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::PgPool;

    use super::{
        adopt_saved_clarifications_into_queued_run, parse_skill_markdown, raw_skill_url, AgentMessage, AgentUpdate,
        ClosedPullRequest, InboxItem, MainClarificationPost, NewEvent, NewMessage, NewRun, Nudge, QuestAssignee,
        QuestEdit, ReplayGap, Resume, RetryRunError, TaskResume, WorldStore, REPLAY_MAX_EVENTS, VERDICT_NUDGE,
    };
    use crate::{milestones::MilestoneStore, run_causes::Required};
    use chrono::{DateTime, Duration, Utc};
    use serde_json::{json, Value};
    use uuid::Uuid;

    fn webhook_result<'a>(
        url: &'a str,
        repo: &'a str,
        branch: &'a str,
        number: i32,
        merged: bool,
        closed_at: Option<DateTime<Utc>>,
    ) -> ClosedPullRequest<'a> {
        ClosedPullRequest { url, repo, branch, number, merged, closed_at }
    }

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
        let Ok(database_url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(8).connect(&database_url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("retry-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Retry', 'codex', 'test', $2) RETURNING id",
        )
        .bind(format!("retry-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let workspace = std::env::temp_dir().join(format!("world-retry-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace)?;
        let workspace = workspace.to_string_lossy().to_string();
        let store = WorldStore::new(pool.clone());
        let main_session = Uuid::new_v4();
        insert_session(&pool, main_session, agent, "main", "active", None, None, Some("main"), Some(&workspace), None).await?;

        let failed_task = Uuid::new_v4();
        let task_session = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, status, error_message)
             VALUES ($1, $2, 'failed retry', 'failed', 'boom')",
        )
        .bind(failed_task)
        .bind(user)
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
        let task_run = insert_run(&pool, task_session, agent, Some(failed_task), "task", "failed", Some(main_session)).await?;
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
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, status, error_message)
             VALUES ($1, $2, 'review retry', 'failed', 'review failed')",
        )
        .bind(review_task)
        .bind(user)
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
        sqlx::query("INSERT INTO agent_tasks (id, user_id, title, status) VALUES ($1, $2, 'manual review retry', 'done')")
            .bind(manual_task)
            .bind(user)
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
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, status, error_message)
             VALUES ($1, $2, 'missing workspace retry', 'failed', 'boom')",
        )
        .bind(missing_task)
        .bind(user)
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
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, status, error_message)
             VALUES ($1, $2, 'worker workspace retry', 'failed', 'boom')",
        )
        .bind(worker_only_task)
        .bind(user)
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
        let newer_run = insert_run(&pool, stale_session, agent, None, "aside", "completed", None).await?;
        sqlx::query(
            "UPDATE session_runs SET created_at = CASE id
               WHEN $1 THEN TIMESTAMPTZ '2025-01-01 00:00:00+00'
               ELSE TIMESTAMPTZ '2025-01-01 00:00:01+00'
             END WHERE id IN ($1, $2)",
        )
        .bind(stale_run)
        .bind(newer_run)
        .execute(&pool)
        .await?;
        let ordered: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM session_runs WHERE session_id = $1 ORDER BY created_at, id")
            .bind(stale_session)
            .fetch_all(&pool)
            .await?;
        assert_eq!(ordered, vec![stale_run, newer_run]);
        assert!(matches!(store.retry_run(stale_run, user).await, Err(RetryRunError::Conflict(_))));

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
        sqlx::query("INSERT INTO agent_tasks (id, user_id, title, status) VALUES ($1, $2, 'simultaneous retry', 'done')")
            .bind(shared_task)
            .bind(user)
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

        sqlx::query("DELETE FROM world_events WHERE agent_id = $1 OR actor_user_id = $2")
            .bind(agent)
            .bind(user)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        pool.close().await;
        std::fs::remove_dir_all(&workspace)?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        use crate::context_budget::RECOVERY_CAUSE;
        use crate::run_causes::{diagnose, RunEnd};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let inbox_upload = Uuid::new_v4();
        let answer_upload = Uuid::new_v4();
        let continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, status, payload)
             VALUES ($1, 'task.assigned', 'done', $2) RETURNING id",
        )
        .bind(agent_id)
        .bind(json!({
            "task_id": task_id,
            "clarification_continuation": Uuid::new_v4(),
            "attachments": [{ "id": inbox_upload, "name": "inbox.bin", "mime": "application/octet-stream" }],
        }))
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2)")
            .bind(failed)
            .bind(continuation)
            .execute(&pool)
            .await?;
        let card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, body, meta)
             VALUES ($1, 'agent', 'Which file?', $2) RETURNING id",
        )
        .bind(agent_id)
        .bind(json!({ "clarification": { "task_id": task_id, "session_id": task_session } }))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, body, run_id, meta)
             VALUES ($1, 'user', 'This one', $2, $3)",
        )
        .bind(agent_id)
        .bind(failed)
        .bind(json!({
            "answers": card,
            "attachments": [{ "id": answer_upload, "name": "answer.bin", "mime": "application/octet-stream" }],
        }))
        .execute(&pool)
        .await?;
        let end = RunEnd { kind: "task", error: Some("Prompt is too long"), output: "", turns: 0, timed_out: false };
        let diagnosis = diagnose(&end);
        let retry = store.recover_overflow(failed, diagnosis.overflowed()).await?.expect("one fresh retry");
        let retry_payloads = store.run_answer_payloads(retry).await?;
        let retry_attachments = retry_payloads
            .iter()
            .filter_map(|payload| payload.get("attachments").and_then(Value::as_array))
            .flatten()
            .filter_map(|attachment| attachment.get("id").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok()))
            .collect::<Vec<_>>();
        assert!(retry_attachments.contains(&inbox_upload));
        assert!(retry_attachments.contains(&answer_upload));
        let (fork_from, prompt, cause, status): (Option<Uuid>, String, Option<String>, String) =
            sqlx::query_as("SELECT fork_from, prompt, retry_cause, status FROM session_runs WHERE id = $1")
                .bind(retry)
                .fetch_one(&pool)
                .await?;
        assert_eq!(fork_from, None);
        assert_eq!(cause.as_deref(), Some(RECOVERY_CAUSE));
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
        assert_eq!(store.recover_overflow(retry, diagnosis.overflowed()).await?, None);
        assert_eq!(store.recover_overflow(failed, diagnosis.overflowed()).await?, None);
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
        assert_eq!((kind.as_str(), fork_from, cause.as_deref()), ("task", None, Some(RECOVERY_CAUSE)));
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
    async fn a_cli_session_that_was_never_saved_starts_over_from_notes() -> anyhow::Result<()> {
        use crate::context::StartOver;
        use crate::run_causes::LOST_SESSION_CAUSE;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("lost-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Neon', 'omp', 'openrouter/google/gemini-4-neon', $2) RETURNING id",
        )
        .bind(format!("lost-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("lost-{tag}-notes"), "notes", "Paint the sky in neon.")
            .await?;
        let lost_id = "01a21d98-c1f1-7311-ba20-3dde5fef60fa";
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, rules_hash, context_tokens)
             VALUES ($1, 'main', 'omp', 'gemini', $2, 'old-rules', 900) RETURNING id",
        )
        .bind(agent_id)
        .bind(lost_id)
        .fetch_one(&pool)
        .await?;
        let session = |role: &'static str, cli: Option<String>, task: Option<Uuid>| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, cli_session_id)
                     VALUES ($1, $2, 'omp', 'gemini', $3, $4, $5) RETURNING id",
                )
                .bind(agent_id)
                .bind(role)
                .bind(main_id)
                .bind(task)
                .bind(cli)
                .fetch_one(&pool)
                .await
            }
        };
        let run = |session: Uuid, kind: &'static str, fork_from: Option<Uuid>, task: Option<Uuid>, status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, fork_from, prompt)
                     VALUES ($1, $2, $3, $4, $5, $6, 'Draw the neon gate.') RETURNING id",
                )
                .bind(session)
                .bind(agent_id)
                .bind(task)
                .bind(kind)
                .bind(status)
                .bind(fork_from)
                .fetch_one(&pool)
                .await
            }
        };
        let fail = |run: Uuid, error: String| {
            let pool = pool.clone();
            async move {
                sqlx::query("UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW() - INTERVAL '30 seconds' WHERE id = $1")
                    .bind(run)
                    .bind(error)
                    .execute(&pool)
                    .await
                    .map(|_| run)
            }
        };
        let omp_error = format!("Error: Session \"{lost_id}\" not found.\nRun `omp --resume` without an argument to pick from recent sessions, or `omp` to start a new one.: omp exited before it was ready");
        let main_cli = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT cli_session_id FROM agent_sessions WHERE id = $1")
                    .bind(main_id)
                    .fetch_one(&pool)
                    .await
            }
        };

        let other = fail(run(main_id, "turn", None, None, "running").await?, omp_error.replace(lost_id, "01a21d98-0000-7000-8000-000000000000")).await?;
        assert_eq!(store.start_over_lost_session(other).await?, StartOver::Kept, "a different lost id never drops this session");
        let hiccup = fail(run(main_id, "turn", None, None, "running").await?, "stream disconnected before completion".to_string()).await?;
        assert_eq!(store.start_over_lost_session(hiccup).await?, StartOver::Kept);
        let unknown = fail(run(main_id, "turn", None, None, "queued").await?, "NotFoundError: Resource not found: /data/storage/session/info/ses_1.json".to_string()).await?;
        assert_eq!(store.start_over_lost_session(unknown).await?, StartOver::Kept, "without the id the run resumed nothing is dropped");
        assert_eq!(main_cli().await?.as_deref(), Some(lost_id));

        let task_id: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'gate', 'todo') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let fork_session = session("task", None, Some(task_id)).await?;
        let queued_fork = run(fork_session, "task", Some(main_id), Some(task_id), "queued").await?;
        let aside_session = session("aside", None, None).await?;
        let queued_aside = run(aside_session, "aside", Some(main_id), None, "queued").await?;
        let waiting_quick = session("quick", None, None).await?;
        let queued_quick = run(waiting_quick, "quick", None, None, "queued").await?;

        let turn = fail(run(main_id, "turn", None, None, "running").await?, omp_error.clone()).await?;
        assert_eq!(store.start_over_lost_session(turn).await?, StartOver::Forgotten);
        let main = store.get_session(main_id).await?.expect("main");
        assert_eq!(main.cli_session_id, None);
        assert_eq!(main.rules_hash, None, "the fresh turn gets rules, teams and skills again");
        assert_eq!(main.context_tokens, 0);
        let memory = main.summary.unwrap_or_default();
        assert!(memory.contains("could not be found on this device") && memory.contains("Paint the sky in neon."), "{memory}");
        for queued in [queued_fork, queued_aside, queued_quick] {
            let (fork_from, prompt): (Option<Uuid>, String) = sqlx::query_as("SELECT fork_from, prompt FROM session_runs WHERE id = $1")
                .bind(queued)
                .fetch_one(&pool)
                .await?;
            assert_eq!(fork_from, None, "work waiting to fork the lost session starts fresh");
            assert!(prompt.contains("Paint the sky in neon.") && prompt.ends_with("Draw the neon gate."), "{prompt}");
        }
        assert_eq!(store.start_over_lost_session(turn).await?, StartOver::Kept, "nothing is forgotten twice");

        sqlx::query("UPDATE agent_tasks SET status = 'in_progress' WHERE id = $1").bind(task_id).execute(&pool).await?;
        let quest_session = session("task", Some(format!("quest-{tag}")), Some(task_id)).await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, created_at)
             VALUES ($1, $2, $3, 'task', 'completed', 'intro [task session] Build the neon gate.', NOW() - INTERVAL '1 hour')",
        )
        .bind(quest_session)
        .bind(agent_id)
        .bind(task_id)
        .execute(&pool)
        .await?;
        let claude_error = format!("Claude Code exited with code 1: No conversation found with session ID: quest-{tag}");
        let followup = fail(run(quest_session, "followup", None, Some(task_id), "running").await?, claude_error.clone()).await?;
        let diagnosis = store.record_run_end(followup, "followup", Some(&claude_error), "", false).await?;
        assert!(diagnosis.lost_session());
        let retry = store.auto_retry(followup, &diagnosis).await?.expect("a lost quest session starts over");
        let (kind, fork_from, prompt, cause): (String, Option<Uuid>, String, Option<String>) =
            sqlx::query_as("SELECT kind, fork_from, prompt, retry_cause FROM session_runs WHERE id = $1")
                .bind(retry)
                .fetch_one(&pool)
                .await?;
        assert_eq!((kind.as_str(), fork_from, cause.as_deref()), ("task", None, Some(LOST_SESSION_CAUSE)));
        assert!(prompt.contains("git status") && prompt.contains("Paint the sky in neon."), "{prompt}");
        assert!(prompt.contains("could not be found on this device"), "{prompt}");
        assert!(prompt.contains("[task session] Build the neon gate.") && prompt.contains("Draw the neon gate."), "{prompt}");
        let (handed, reason): (Option<Uuid>, Option<String>) =
            sqlx::query_as("SELECT retried_as, diagnosis->>'reason' FROM session_runs WHERE id = $1").bind(followup).fetch_one(&pool).await?;
        assert_eq!(handed, Some(retry));
        assert_eq!(reason.as_deref(), Some(crate::cli_errors::LOST_SESSION_RESTARTED), "the failed run says what really happened");
        let retrying: Value = sqlx::query_scalar("SELECT payload FROM world_events WHERE type = 'run.retrying' AND run_id = $1")
            .bind(retry)
            .fetch_one(&pool)
            .await?;
        assert_eq!(retrying["cause"], "cli_error", "{retrying}");
        assert_eq!(retrying["reason"], crate::cli_errors::LOST_SESSION_RESTARTED, "the quest room shows why it started over");
        assert_eq!((retrying["failed_run_id"].as_str(), retrying["quiet"].as_bool()), (Some(followup.to_string().as_str()), Some(true)));
        let (cli, status): (Option<String>, String) = sqlx::query_as("SELECT cli_session_id, status FROM agent_sessions WHERE id = $1")
            .bind(quest_session)
            .fetch_one(&pool)
            .await?;
        assert_eq!((cli, status.as_str()), (None, "active"));
        fail(retry, claude_error.clone()).await?;
        assert_eq!(store.start_over_lost_session(retry).await?, StartOver::Kept, "a fresh start is never started over again");

        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1").bind(quest_session).bind(format!("review-{tag}")).execute(&pool).await?;
        let review = fail(run(quest_session, "review", None, Some(task_id), "running").await?, claude_error.replace("quest-", "review-")).await?;
        let StartOver::Retried(fresh_review) = store.start_over_lost_session(review).await? else {
            panic!("a review whose session is gone continues fresh like any other run");
        };
        let review_kind: String = sqlx::query_scalar("SELECT kind FROM session_runs WHERE id = $1").bind(fresh_review).fetch_one(&pool).await?;
        assert_eq!(review_kind, "review");
        let cancelled: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'gone', 'cancelled') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let closed_session = session("task", Some(format!("closed-{tag}")), Some(cancelled)).await?;
        let closed = fail(
            run(closed_session, "task", None, Some(cancelled), "running").await?,
            format!("Claude Code exited with code 1: No conversation found with session ID: closed-{tag}"),
        )
        .await?;
        assert_eq!(store.start_over_lost_session(closed).await?, StartOver::Kept, "a cancelled quest never restarts");
        assert_eq!(store.get_session(closed_session).await?.expect("closed").cli_session_id, Some(format!("closed-{tag}")));

        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1").bind(main_id).bind(lost_id).execute(&pool).await?;
        let late_fork_session = session("task", None, Some(task_id)).await?;
        let late_fork = run(late_fork_session, "task", Some(main_id), Some(task_id), "running").await?;
        sqlx::query("UPDATE agent_sessions SET cli_session_id = 'moved-on' WHERE id = $1").bind(main_id).execute(&pool).await?;
        fail(late_fork, omp_error.clone()).await?;
        let StartOver::Retried(unforked) = store.start_over_lost_session(late_fork).await? else {
            panic!("a fork whose parent moved on still starts fresh");
        };
        let fork_from: Option<Uuid> = sqlx::query_scalar("SELECT fork_from FROM session_runs WHERE id = $1").bind(unforked).fetch_one(&pool).await?;
        assert_eq!(fork_from, None);
        assert_eq!(main_cli().await?.as_deref(), Some("moved-on"), "the parent that moved on is left alone");

        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1").bind(main_id).bind(lost_id).execute(&pool).await?;
        let quick_session = session("quick", None, None).await?;
        let quick = fail(run(quick_session, "quick", None, None, "running").await?, omp_error.clone()).await?;
        let StartOver::Retried(again) = store.start_over_lost_session(quick).await? else {
            panic!("a quick chat that forks a lost main session starts over");
        };
        let (kind, prompt): (String, String) = sqlx::query_as("SELECT kind, prompt FROM session_runs WHERE id = $1")
            .bind(again)
            .fetch_one(&pool)
            .await?;
        assert_eq!(kind, "quick");
        assert!(prompt.contains("Paint the sky in neon.") && prompt.contains("Draw the neon gate."), "{prompt}");
        assert_eq!(main_cli().await?, None, "the lost main session is dropped too");
        assert!(
            store.quick_runs_without_reply(Some(quick_session), 0).await?.is_empty(),
            "a quick run handed to its retry never gets an I could not answer that bubble"
        );

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = ANY($1)").bind(vec![task_id, cancelled]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("lost-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn auto_quests_that_fork_and_fail_turn_one_retry_fresh_once() -> anyhow::Result<()> {
        use crate::context::{ForkChoice, QuestContext};
        use crate::context_budget::RECOVERY_CAUSE;
        use crate::run_causes::{diagnose, RunEnd};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        assert!(diagnosis.overflowed());
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

        let retry = store.recover_overflow(failed, diagnosis.overflowed()).await?.expect("one fresh retry");
        let (fork_from, prompt, cause): (Option<Uuid>, String, Option<String>) =
            sqlx::query_as("SELECT fork_from, prompt, retry_cause FROM session_runs WHERE id = $1")
                .bind(retry)
                .fetch_one(&pool)
                .await?;
        assert_eq!(fork_from, None, "the retry never forks the main session again");
        assert_eq!(cause.as_deref(), Some(RECOVERY_CAUSE));
        assert!(prompt.contains("Fit the green gear brief."));
        assert!(prompt.contains("Remember the green gear."));
        assert_eq!(store.recover_overflow(failed, diagnosis.overflowed()).await?, None);
        sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Prompt is too long' WHERE id = $1")
            .bind(retry)
            .execute(&pool)
            .await?;
        assert_eq!(store.recover_overflow(retry, diagnosis.overflowed()).await?, None, "a fresh retry is never retried again");

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
        use crate::context_budget::RECOVERY_CAUSE;
        use crate::world::COMPACT_PROMPT;
        use serde_json::json;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        assert!(diagnosis.overflowed());
        let retry = store.auto_retry(failed, &diagnosis).await?.expect("one fresh retry for a runner run");
        let cause: Option<String> = sqlx::query_scalar("SELECT retry_cause FROM session_runs WHERE id = $1")
            .bind(retry)
            .fetch_one(&pool)
            .await?;
        assert_eq!(cause.as_deref(), Some(RECOVERY_CAUSE));
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
            quest_scope: None,
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(agent).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
            .bind(org)
            .bind(user)
            .execute(&pool)
            .await?;
        let subscriber: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'Watcher', 'claude', 'haiku', $2, $3) RETURNING id",
        )
        .bind(format!("watch-{tag}"))
        .bind(user)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let peer_user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Peer', 'write') RETURNING id")
            .bind(format!("peer-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let assignee_user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Assignee', 'write') RETURNING id")
            .bind(format!("assignee-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member'), ($1, $3, 'member')",
        )
        .bind(org)
        .bind(peer_user)
        .bind(assignee_user)
        .execute(&pool)
        .await?;
        let peer_subscriber: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'Peer watcher', 'claude', 'haiku', $2, $3) RETURNING id",
        )
        .bind(format!("peer-watch-{tag}"))
        .bind(peer_user)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let assignee_subscriber: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'Assignee watcher', 'claude', 'haiku', $2, $3) RETURNING id",
        )
        .bind(format!("assignee-watch-{tag}"))
        .bind(assignee_user)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        store.subscribe(subscriber, &["task.deleted"]).await?;
        store.subscribe(peer_subscriber, &["task.deleted"]).await?;
        store.subscribe(assignee_subscriber, &["task.deleted"]).await?;
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
        let main_clarification: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, session_id, meta)
             VALUES ($1, 'agent', $1, 'Which workspace?', $2, $3) RETURNING id",
        )
        .bind(agent)
        .bind(session)
        .bind(json!({ "kind": "ask", "clarify_request": { "task_id": task, "kind": "task.assigned" } }))
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
        store
            .emit(
                NewEvent::new("attention.requested")
                    .agent(agent)
                    .session(session)
                    .task(Some(task))
                    .payload(json!({ "text": "Which workspace?", "message_id": main_clarification, "clarification": true })),
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
        let main_settled: (Value, Option<Uuid>) = sqlx::query_as(
            "SELECT meta, author_user_id FROM agent_messages WHERE agent_id = $1 AND meta->>'answers' = $2",
        )
        .bind(agent)
        .bind(main_clarification.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(main_settled.0["auto"], json!(true));
        assert_eq!(main_settled.1, Some(user));
        let close_event: Value = sqlx::query_scalar(
            "SELECT payload FROM world_events WHERE type = 'message.created' AND payload->>'answer_card' = $1",
        )
        .bind(clarification.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(close_event["quiet"], json!(true));
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "idle");
        let open_ask: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_notifications WHERE kind = 'ask' AND ref IN ($1, $2) AND cleared_at IS NULL")
            .bind(clarification.to_string())
            .bind(main_clarification.to_string())
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
        let main_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(main_clarification.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(main_answers, 1);

        let late_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'late question', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let main_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'claude', 'haiku', 'active') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let turn_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
             VALUES ($1, $2, 'turn', 'completed', 'decide', NOW() - INTERVAL '2 minutes') RETURNING id",
        )
        .bind(main_session)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let late_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'claude', 'haiku', 'late-cli', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(late_task)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, created_at)
             VALUES ($1, $2, $3, 'task', 'completed', 'build', NOW() - INTERVAL '1 minute')",
        )
        .bind(late_session)
        .bind(agent)
        .bind(late_task)
        .execute(&pool)
        .await?;
        let late_message = |body: &'static str| NewMessage {
            agent_id: agent,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent),
            body,
            session_id: Some(main_session),
            run_id: Some(turn_run),
            meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": late_task, "kind": "task.assigned" } }),
        };
        assert!(store.post_main_clarification(late_task, "Pri", &[], late_message("Too late?")).await?.is_none());
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'followup', 'queued', 'continue')",
        )
        .bind(late_session)
        .bind(agent)
        .bind(late_task)
        .execute(&pool)
        .await?;
        let active_message = NewMessage { run_id: None, ..late_message("While active?") };
        assert!(store.post_main_clarification(late_task, "Pri", &[], active_message).await?.is_none());
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE task_id = $1 AND status = 'queued'")
            .bind(late_task)
            .execute(&pool)
            .await?;
        let late_cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND body IN ('Too late?', 'While active?')")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(late_cards, 0);

        let legacy_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'old question', 'failed', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Old question?', $2)",
        )
        .bind(agent)
        .bind(json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": legacy_task, "kind": "task.assigned" } }))
        .execute(&pool)
        .await?;
        assert_eq!(store.resume_preview(agent, legacy_task).await?, crate::quests::ResumePlan::Fresh);

        let failed_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'fail cleanly', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let failed_card = store
            .post_main_clarification(
                failed_task,
                "Pri",
                &[],
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which failure?",
                    session_id: Some(main_session),
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": failed_task, "kind": "task.assigned" } }),
                },
            )
            .await?
            .expect("failed quest clarification")
            .id;
        let duplicate_main = store
            .post_main_clarification(
                failed_task,
                "Pri",
                &[],
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which duplicate failure?",
                    session_id: Some(main_session),
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": failed_task, "kind": "task.assigned" } }),
                },
            )
            .await?;
        assert!(duplicate_main.is_none());
        let failed_cards: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND meta->'clarify_request'->>'task_id' = $2",
        )
        .bind(agent)
        .bind(failed_task.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(failed_cards, 1);
        assert_eq!(store.resume_preview(agent, failed_task).await?, crate::quests::ResumePlan::AwaitingClarification);
        let failed_org: Uuid = sqlx::query_scalar("SELECT org_id FROM world_notifications WHERE kind = 'ask' AND ref = $1")
            .bind(failed_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert!(store.clear_notifications(user, Some(failed_org), super::BellClear::All).await? > 0);
        assert_eq!(store.resume_preview(agent, failed_task).await?, crate::quests::ResumePlan::AwaitingClarification);
        store
            .edit_quest(
                failed_task,
                &QuestEdit { status: Some("failed".into()), assignee: None },
                user,
                "Ana",
            )
            .await?;
        let failed_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(failed_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(failed_answers, 1);
        let failed_asks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_notifications WHERE kind = 'ask' AND ref = $1 AND cleared_at IS NULL")
            .bind(failed_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(failed_asks, 0);
        assert_eq!(store.resume_preview(agent, failed_task).await?, crate::quests::ResumePlan::Fresh);
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "idle");

        let next_agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Sol', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("sol-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let handoff: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'handoff', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let choices = vec!["world".to_string(), "loka".to_string()];
        let handoff_card = store
            .post_main_clarification(
                handoff,
                "Pri",
                &choices,
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which repo?",
                    session_id: None,
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": choices.clone(), "clarify_request": { "task_id": handoff, "kind": "task.assigned" } }),
                },
            )
            .await?
            .expect("live todo clarification")
            .id;
        let reassigned_continuations: Vec<i64> = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status) VALUES
               ($1, 'task.assigned', $2, 'pending'),
               ($1, 'task.assigned', $2, 'processing')
             RETURNING id",
        )
        .bind(agent)
        .bind(json!({
            "task_id": handoff,
            "resume": true,
            "clarification_continuation": handoff_card,
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_all(&pool)
        .await?;
        store
            .edit_quest(
                handoff,
                &QuestEdit { status: None, assignee: Some(QuestAssignee::Agent(next_agent)) },
                user,
                "Ana",
            )
            .await?;
        let handoff_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(handoff_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(handoff_answers, 1);
        assert_eq!(store.get_agent(agent).await?.expect("old agent").status, "idle");
        let reassigned_statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = ANY($1)")
            .bind(&reassigned_continuations)
            .fetch_all(&pool)
            .await?;
        assert!(reassigned_statuses.iter().all(|status| status == "done"));
        let late = store
            .post_main_clarification(
                handoff,
                "Pri",
                &[],
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which branch?",
                    session_id: None,
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": handoff, "kind": "task.assigned" } }),
                },
            )
            .await?;
        assert!(late.is_none());
        let done_continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2) RETURNING id",
        )
        .bind(next_agent)
        .bind(json!({
            "task_id": handoff,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&pool)
        .await?;
        store
            .edit_quest(
                handoff,
                &QuestEdit { status: Some("done".into()), assignee: None },
                user,
                "Ana",
            )
            .await?;
        let done_continuation_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(done_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(done_continuation_status, "done");
        let ordinary_done = store
            .post_main_clarification(
                handoff,
                "Sol",
                &[],
                NewMessage {
                    agent_id: next_agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(next_agent),
                    body: "Which branch?",
                    session_id: None,
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": handoff, "kind": "task.assigned" } }),
                },
            )
            .await?;
        assert!(ordinary_done.is_none());
        let resumed_card = store
            .post_main_clarification(
                handoff,
                "Sol",
                &[],
                NewMessage {
                    agent_id: next_agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(next_agent),
                    body: "Which branch?",
                    session_id: None,
                    run_id: None,
                    meta: json!({ "kind": "ask", "choices": [], "clarify_request": { "task_id": handoff, "kind": "task.assigned", "resume": true } }),
                },
            )
            .await?
            .expect("an explicit resume stays answerable")
            .id;
        let resumed_done_item = sqlx::query_as::<_, InboxItem>(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2)
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(next_agent)
        .bind(json!({
            "task_id": handoff,
            "resume": true,
            "clarification_continuation": resumed_card,
            "clarification_status": "done",
            "clarification_resume": true,
        }))
        .fetch_one(&pool)
        .await?;
        assert!(store.clarification_continuation_open(&resumed_done_item).await?);
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Paused by Ana' WHERE id = $1")
            .bind(next_agent)
            .execute(&pool)
            .await?;
        assert_eq!(
            store.resume_preview(next_agent, handoff).await?,
            crate::quests::ResumePlan::Refused("your answer is saved, and the agent will continue when it is ready".into())
        );
        let pending_continuations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox
             WHERE agent_id = $1 AND status IN ('pending', 'processing') AND payload ? 'clarification_continuation'",
        )
        .bind(next_agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(pending_continuations, 1);
        sqlx::query("UPDATE agent_profiles SET paused_reason = NULL WHERE id = $1")
            .bind(next_agent)
            .execute(&pool)
            .await?;
        store.cancel_task(handoff, "Sol", None).await?;
        let resumed_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(resumed_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(resumed_answers, 1);

        let resumed_failed: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'resume failed', 'failed', $2) RETURNING id",
        )
        .bind(user)
        .bind(next_agent)
        .fetch_one(&pool)
        .await?;
        let resumed_failed_item = sqlx::query_as::<_, InboxItem>(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2)
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(next_agent)
        .bind(json!({
            "task_id": resumed_failed,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "failed",
            "clarification_resume": true,
        }))
        .fetch_one(&pool)
        .await?;
        assert!(store.clarification_continuation_open(&resumed_failed_item).await?);
        store
            .edit_quest(
                resumed_failed,
                &QuestEdit { status: Some("todo".into()), assignee: None },
                user,
                "Ana",
            )
            .await?;
        let resumed_failed_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(resumed_failed_item.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(resumed_failed_status, "done");

        let terminal_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'terminal continuation', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let terminal_continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status) VALUES ($1, 'task.assigned', $2, 'processing') RETURNING id",
        )
        .bind(agent)
        .bind(json!({
            "task_id": terminal_task,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&pool)
        .await?;
        store
            .edit_quest(
                terminal_task,
                &QuestEdit { status: Some("done".into()), assignee: None },
                user,
                "Ana",
            )
            .await?;
        store.requeue_inbox(&[terminal_continuation], "failed turn").await?;
        store
            .edit_quest(
                terminal_task,
                &QuestEdit { status: Some("todo".into()), assignee: None },
                user,
                "Ana",
            )
            .await?;
        let terminal_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(terminal_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(terminal_status, "done");

        let race_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'answer race', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let mut answer_tx = pool.begin().await?;
        sqlx::query("SELECT id FROM agent_tasks WHERE id = $1 FOR UPDATE")
            .bind(race_task)
            .execute(&mut *answer_tx)
            .await?;
        let race_continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({
            "task_id": race_task,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&mut *answer_tx)
        .await?;
        let closing_store = store.clone();
        let closing = tokio::spawn(async move {
            closing_store
                .edit_quest(
                    race_task,
                    &QuestEdit { status: Some("done".into()), assignee: None },
                    user,
                    "Ana",
                )
                .await
        });
        tokio::task::yield_now().await;
        answer_tx.commit().await?;
        closing.await??;
        let race_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(race_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(race_status, "done");

        let pickup_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'pickup race', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let pickup_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id)
             VALUES ($1, 'task', 'claude', 'haiku', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(pickup_task)
        .fetch_one(&pool)
        .await?;
        let pickup_item = sqlx::query_as::<_, InboxItem>(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status) VALUES ($1, 'task.assigned', $2, 'processing')
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(agent)
        .bind(json!({
            "task_id": pickup_task,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&pool)
        .await?;
        let mut close_tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(pickup_task)
            .execute(&mut *close_tx)
            .await?;
        let pickup_store = store.clone();
        let pickup = tokio::spawn(async move {
            pickup_store
                .create_clarification_run(
                    &pickup_item,
                    NewRun {
                        session_id: pickup_session,
                        agent_id: agent,
                        task_id: Some(pickup_task),
                        kind: "task",
                        status: "queued",
                        fork_from: None,
                        prompt: "use the saved answer",
                        timeout_secs: 3600,
                        request: None,
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        close_tx.commit().await?;
        assert!(pickup.await??.is_none());
        let pickup_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(pickup_task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(pickup_runs, 0);

        let stale_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'stale continuation', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let stale_continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({
            "task_id": stale_task,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1")
            .bind(stale_task)
            .execute(&pool)
            .await?;
        let taken = store.take_pending_inbox(agent, 20).await?;
        assert!(!taken.iter().any(|item| item.id == stale_continuation));
        let stale_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(stale_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(stale_status, "done");

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

        let grandchild: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id)
             VALUES ($1, 'grandchild', 'done', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(child)
        .fetch_one(&pool)
        .await?;
        let contextual_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Which archived detail?', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "kind": "ask", "clarify_request": { "context_task_id": grandchild } }))
        .fetch_one(&pool)
        .await?;

        let deleted = store.delete_task(task, "Pri", Some("clutter")).await?;
        assert_eq!(deleted.subtasks, 1);
        assert_eq!(deleted.dropped, 1);
        assert_eq!(deleted.running, vec![running]);
        assert_eq!(deleted.archived, 1);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_tasks WHERE id = ANY($1)")
            .bind([task, child, grandchild])
            .fetch_one(&pool)
            .await?;
        assert_eq!(left, 0);
        let contextual_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(contextual_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(contextual_answers, 1);
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
        let deleted_events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events
             WHERE type = 'task.deleted' AND payload->>'task_id' IN ($1::text, $2::text) ORDER BY seq",
        )
        .bind(task)
        .bind(child)
        .fetch_all(&pool)
        .await?;
        assert_eq!(deleted_events.len(), 2, "the parent and cascading subtask each invalidate open quest views once");
        let parent_event = deleted_events.iter().find(|event| event.payload["task_id"] == json!(task)).expect("parent delete event");
        assert_eq!((parent_event.task_id, parent_event.agent_id, parent_event.org_id), (None, Some(agent), Some(org)));
        assert_eq!(parent_event.payload["title"], json!("slack triage"));
        let child_event = deleted_events.iter().find(|event| event.payload["task_id"] == json!(child)).expect("child delete event");
        assert_eq!((child_event.task_id, child_event.agent_id, child_event.org_id), (None, Some(agent), Some(org)));
        assert_eq!(child_event.payload, json!({ "task_id": child }));
        let direct_inbox: Value = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(subscriber)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(direct_inbox["task_id"], json!(task));
        let peer_direct: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(peer_subscriber)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(peer_direct, 0);

        let legacy: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'legacy card', 'done', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(crate::db::Database::from_pool(pool.clone()).delete_task(legacy, user).await?, 1);
        let legacy_event = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text",
        )
        .bind(legacy)
        .fetch_one(&pool)
        .await?;
        let legacy_event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text",
        )
        .bind(legacy)
        .fetch_one(&pool)
        .await?;
        assert_eq!(legacy_event_count, 1);
        assert_eq!((legacy_event.task_id, legacy_event.org_id), (None, Some(org)));
        assert_eq!(legacy_event.payload, json!({ "task_id": legacy }));
        assert!(legacy_event.quest_scope.is_some());
        let legacy_inbox: Value = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(subscriber)
        .bind(legacy)
        .fetch_one(&pool)
        .await?;
        assert_eq!(legacy_inbox["task_id"], json!(legacy));
        let peer_legacy: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(peer_subscriber)
        .bind(legacy)
        .fetch_one(&pool)
        .await?;
        assert_eq!(peer_legacy, 0);
        let private_legacy: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, assignee_user_id, title, status, org_id) VALUES ($1, $2, 'private legacy card', 'done', $3) RETURNING id",
        )
        .bind(user)
        .bind(assignee_user)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        assert_eq!(crate::db::Database::from_pool(pool.clone()).delete_task(private_legacy, user).await?, 1);
        let private_event = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text",
        )
        .bind(private_legacy)
        .fetch_one(&pool)
        .await?;
        assert_eq!(private_event.agent_id, None);
        assert_eq!(private_event.payload, json!({ "task_id": private_legacy }));
        assert!(crate::event_scope::quest_scope_allows(&private_event, user));
        assert!(crate::event_scope::quest_scope_allows(&private_event, assignee_user));
        assert!(!crate::event_scope::quest_scope_allows(&private_event, peer_user));
        assert!(serde_json::to_value(&private_event)?.get("quest_scope").is_none());
        for watcher in [subscriber, assignee_subscriber] {
            let delivered: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
            )
            .bind(watcher)
            .bind(private_legacy)
            .fetch_one(&pool)
            .await?;
            assert_eq!(delivered, 1);
        }
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(peer_subscriber)
        .bind(private_legacy)
        .fetch_one(&pool)
        .await?;
        assert_eq!(leaked, 0);
        assert!(store.delete_task(task, "Pri", None).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn a_new_request_in_a_completed_quest_can_ask_without_reopening_it() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("completed-context-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Guide', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("completed-context-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Completed context', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        let message_id = Uuid::new_v4();
        let seq = store
            .emit(
                NewEvent::new("message.created")
                    .agent(agent)
                    .actor(Some(user))
                    .task(Some(task))
                    .payload(json!({
                        "message_id": message_id,
                        "body": "Build the matching export",
                        "from_owner": true,
                        "task_id": task,
                    })),
                Some((agent, "message")),
            )
            .await?;
        let source: i64 = sqlx::query_scalar("SELECT id FROM agent_inbox WHERE event_seq = $1")
            .bind(seq)
            .fetch_one(&pool)
            .await?;
        let private_question = format!("Which format should the export use? PRIVATE_{}", Uuid::new_v4());
        let origin = json!({
            "request": "Build the matching export",
            "message_id": message_id,
            "context_task_id": task,
            "source_inbox_id": source,
        });
        let ask = || NewMessage {
            agent_id: agent,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent),
            body: &private_question,
            session_id: None,
            run_id: None,
            meta: json!({ "kind": "ask", "choices": ["CSV"], "clarify_request": origin }),
        };
        assert_eq!(
            store.post_main_request_clarification("Guide", &["CSV".into()], ask()).await?,
            MainClarificationPost::Posted
        );
        let card = sqlx::query_as::<_, AgentMessage>(
            "SELECT * FROM agent_messages
             WHERE agent_id = $1 AND author_kind = 'agent' AND meta->>'kind' = 'ask'
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(card.body, private_question);
        assert_eq!(card.meta["clarify_request"]["context_task_id"], task.to_string());
        assert!(card.meta["clarify_request"].get("task_id").is_none_or(Value::is_null));
        let profile: (String, Option<String>) = sqlx::query_as("SELECT status, status_detail FROM agent_profiles WHERE id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(profile, ("waiting".into(), Some(crate::quests::CLARIFICATION_WAITING_DETAIL.into())));
        let status_event: Value = sqlx::query_scalar(
            "SELECT payload FROM world_events WHERE type = 'agent.status' AND agent_id = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(status_event["detail"], crate::quests::CLARIFICATION_WAITING_DETAIL);
        assert!(!status_event.to_string().contains(&private_question));
        assert_eq!(
            store.post_main_request_clarification("Guide", &["CSV".into()], ask()).await?,
            MainClarificationPost::AlreadyHandled
        );
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(runs, 0);
        let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "done");
        assert_eq!(
            store.contextual_quest_clarification(task, agent, user).await?.map(|message| message.id),
            Some(card.id)
        );
        store
            .edit_quest(task, &QuestEdit { status: Some("in_progress".into()), assignee: None }, user, "Ana")
            .await?;
        assert!(store.contextual_quest_clarification(task, agent, user).await?.is_none());
        let settled: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND meta->>'answers' = $2 AND meta->>'auto' = 'true'",
        )
        .bind(agent)
        .bind(card.id.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(settled, 1);

        let stale_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Stale context', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let stale_seq = store
            .emit(
                NewEvent::new("message.created")
                    .agent(agent)
                    .actor(Some(user))
                    .task(Some(stale_task))
                    .payload(json!({
                        "message_id": Uuid::new_v4(),
                        "body": "Old request",
                        "from_owner": true,
                        "task_id": stale_task,
                    })),
                Some((agent, "message")),
            )
            .await?;
        let stale_source: i64 = sqlx::query_scalar("SELECT id FROM agent_inbox WHERE event_seq = $1")
            .bind(stale_seq)
            .fetch_one(&pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(stale_task)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET completed_at = NOW() + INTERVAL '1 second' WHERE id = $1")
        .bind(stale_task)
        .execute(&pool)
        .await?;
        let stale = NewMessage {
            agent_id: agent,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent),
            body: "Which one?",
            session_id: None,
            run_id: None,
            meta: json!({
                "kind": "ask",
                "clarify_request": {
                    "request": "Old request",
                    "context_task_id": stale_task,
                    "source_inbox_id": stale_source,
                }
            }),
        };
        assert_eq!(
            store.post_main_request_clarification("Guide", &[], stale).await?,
            MainClarificationPost::Refused
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_unbound_clarification_binds_once_to_the_selected_open_quest() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("unbound-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Guide', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("unbound-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Selected quest', 'todo', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id) VALUES ($1, 'task', 'claude', 'sonnet', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let upload = Uuid::new_v4();
        let payload = json!({
            "task_id": null,
            "from_owner": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": {
                "request": "Prepare the import",
                "question": "Which format?",
                "answer": "CSV"
            },
            "attachments": [{ "id": upload, "name": "rows.csv", "mime": "text/csv" }]
        });
        let seq = store
            .emit(
                NewEvent::new("message.created").agent(agent).actor(Some(user)).payload(payload),
                Some((agent, "message")),
            )
            .await?;
        let item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE event_seq = $1 AND agent_id = $2 AND kind = 'message'
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(seq)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert!(super::clarification_continuation_unbound(&item));
        assert!(store.clarification_continuation_open(&item).await?);
        let first = store
            .create_clarification_run(
                &item,
                NewRun {
                    session_id: session,
                    agent_id: agent,
                    task_id: Some(task),
                    kind: "task",
                    status: "queued",
                    fork_from: None,
                    prompt: "use the accepted exchange",
                    timeout_secs: 3600,
                    request: None,
                },
            )
            .await?
            .expect("first binding");
        let duplicate_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id) VALUES ($1, 'task', 'claude', 'sonnet', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert!(store
            .create_clarification_run(
                &item,
                NewRun {
                    session_id: duplicate_session,
                    agent_id: agent,
                    task_id: Some(task),
                    kind: "task",
                    status: "queued",
                    fork_from: None,
                    prompt: "duplicate",
                    timeout_secs: 3600,
                    request: None,
                },
            )
            .await?
            .is_none());
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(runs, 1);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runner_run_inbox WHERE run_id = $1 AND inbox_id = $2")
            .bind(first.id)
            .bind(item.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(links, 1);
        assert!(store.run_answer_payloads(first.id).await?.iter().any(|value| value.to_string().contains(&upload.to_string())));
        let item_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(item.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(item_status, "done");

        let resume_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Resume quest', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let resume_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'claude', 'sonnet', 'resume-cli', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(resume_task)
        .fetch_one(&pool)
        .await?;
        let resume_payload = json!({
            "task_id": null,
            "from_owner": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": { "request": "Resume it", "question": "Which path?", "answer": "The safe one" }
        });
        let resume_seq = store
            .emit(
                NewEvent::new("message.created").agent(agent).actor(Some(user)).payload(resume_payload),
                Some((agent, "message")),
            )
            .await?;
        let resume_item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE event_seq = $1 AND agent_id = $2 AND kind = 'message'
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(resume_seq)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let TaskResume::Resumed { session_id, run_id } = store
            .resume_clarification_task_session(agent, resume_task, "Fea", Some("continue with the answer"), &resume_item)
            .await?
        else {
            panic!("unbound answer did not resume")
        };
        assert_eq!(session_id, resume_session);
        assert!(store
            .resume_clarification_task_session(agent, resume_task, "Fea", Some("duplicate"), &resume_item)
            .await
            .is_err());
        let resumed_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(resume_task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(resumed_runs, 1);
        let resume_links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runner_run_inbox WHERE run_id = $1 AND inbox_id = $2")
            .bind(run_id)
            .bind(resume_item.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(resume_links, 1);

        let guarded_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Guarded quest', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let guarded_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'claude', 'sonnet', 'guarded-cli', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(guarded_task)
        .fetch_one(&pool)
        .await?;
        let bound_item: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, status, payload) VALUES ($1, 'task.assigned', 'pending', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({
            "task_id": guarded_task,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "in_progress",
            "clarification_resume": false
        }))
        .fetch_one(&pool)
        .await?;
        let guarded_payload = json!({
            "task_id": null,
            "from_owner": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": { "request": "A regular reply", "question": "Continue?", "answer": "Yes" }
        });
        let guarded_seq = store
            .emit(
                NewEvent::new("message.created").agent(agent).actor(Some(user)).payload(guarded_payload),
                Some((agent, "message")),
            )
            .await?;
        let guarded_item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE event_seq = $1 AND agent_id = $2 AND kind = 'message'
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(guarded_seq)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let error = store
            .resume_clarification_task_session(agent, guarded_task, "Fea", Some("ordinary reply"), &guarded_item)
            .await
            .expect_err("a bound clarification should win");
        assert_eq!(error.to_string(), crate::quests::CLARIFICATION_CONTINUING);
        let fresh_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id) VALUES ($1, 'task', 'claude', 'sonnet', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(guarded_task)
        .fetch_one(&pool)
        .await?;
        assert!(store
            .create_clarification_run(
                &guarded_item,
                NewRun {
                    session_id: fresh_session,
                    agent_id: agent,
                    task_id: Some(guarded_task),
                    kind: "task",
                    status: "queued",
                    fork_from: None,
                    prompt: "ordinary reply",
                    timeout_secs: 3600,
                    request: None,
                },
            )
            .await?
            .is_none());
        let guarded_statuses: (String, String) = sqlx::query_as(
            "SELECT (SELECT status FROM agent_inbox WHERE id = $1), (SELECT status FROM agent_inbox WHERE id = $2)",
        )
        .bind(guarded_item.id)
        .bind(bound_item)
        .fetch_one(&pool)
        .await?;
        assert_eq!(guarded_statuses, ("processing".into(), "pending".into()));
        let guarded_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
            .bind(guarded_task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(guarded_runs, 0);
        let current_session_status: String = sqlx::query_scalar("SELECT status FROM agent_sessions WHERE id = $1")
            .bind(guarded_session)
            .fetch_one(&pool)
            .await?;
        assert_eq!(current_session_status, "active");

        let closed_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Closed quest', 'done', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let closed_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id) VALUES ($1, 'task', 'claude', 'sonnet', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(closed_task)
        .fetch_one(&pool)
        .await?;
        let closed_payload = json!({
            "task_id": null,
            "from_owner": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": { "request": "Other work", "question": "Where?", "answer": "Here" }
        });
        let closed_seq = store
            .emit(
                NewEvent::new("message.created").agent(agent).actor(Some(user)).payload(closed_payload),
                Some((agent, "message")),
            )
            .await?;
        let closed_item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE event_seq = $1 AND agent_id = $2 AND kind = 'message'
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(closed_seq)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert!(store
            .create_clarification_run(
                &closed_item,
                NewRun {
                    session_id: closed_session,
                    agent_id: agent,
                    task_id: Some(closed_task),
                    kind: "task",
                    status: "queued",
                    fork_from: None,
                    prompt: "stale selection",
                    timeout_secs: 3600,
                    request: None,
                },
            )
            .await?
            .is_none());
        let closed_item_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(closed_item.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(closed_item_status, "processing");
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id IN ($1, $2)")
            .bind(first.id)
            .bind(run_id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_inbox SET status = 'done', done_at = NOW() WHERE id = $1")
            .bind(closed_item.id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_inbox SET status = 'done', done_at = NOW() WHERE id = ANY($1)")
            .bind(vec![guarded_item.id, bound_item])
            .execute(&pool)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_task_delete_waits_behind_organization_delete() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Deletion lock') RETURNING id")
            .bind(format!("deletion-lock-{}", &tag[..16]))
            .fetch_one(&pool)
            .await?;
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Deletion owner', 'write') RETURNING id")
            .bind(format!("deletion-owner-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, org_id) VALUES ($1, 'Deletion race', 'done', $2) RETURNING id",
        )
        .bind(user)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let mut deleting_org = pool.begin().await?;
        crate::orgs::lock_org(&mut deleting_org, org).await?;
        sqlx::query("SELECT id FROM orgs WHERE id = $1 FOR UPDATE").bind(org).execute(&mut *deleting_org).await?;

        let application_name = format!("legacy-delete-{tag}");
        let options = PgConnectOptions::from_str(&url)?.application_name(&application_name);
        let legacy_pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let deleting_task = tokio::spawn(async move { crate::db::Database::from_pool(legacy_pool).delete_task(task, user).await });
        let mut waiting = false;
        for _ in 0..100 {
            waiting = sqlx::query_scalar(
                "SELECT EXISTS (
                   SELECT 1 FROM pg_stat_activity
                   WHERE datname = current_database() AND application_name = $1
                     AND wait_event_type = 'Lock' AND cardinality(pg_blocking_pids(pid)) > 0)",
            )
            .bind(&application_name)
            .fetch_one(&pool)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(waiting, "the legacy delete reaches the organization lock barrier");
        sqlx::query("SELECT id FROM agent_tasks WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(task)
            .execute(&mut *deleting_org)
            .await?;
        sqlx::query("DELETE FROM orgs WHERE id = $1").bind(org).execute(&mut *deleting_org).await?;
        deleting_org.commit().await?;
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2), deleting_task).await???, 0);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn limit_errors_defer_turns_without_burning_attempts() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts)
             VALUES ($1, 'task.assigned', $2, 'processing', 3) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "clarification_continuation": Uuid::new_v4() }))
        .fetch_one(&pool)
        .await?;
        store.requeue_clarification_starts(&[continuation]).await?;
        let exhausted: (String, i32) = sqlx::query_as("SELECT status, attempts FROM agent_inbox WHERE id = $1")
            .bind(continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(exhausted, ("failed".into(), 3));
        Ok(())
    }

    #[tokio::test]
    async fn daily_budget_warns_pauses_and_releases() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LIMITS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let fresh = store.recover_overflow(overflow, too_long.overflowed()).await?.expect("a fork that was too long starts fresh");
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let inbox_upload = Uuid::new_v4();
        let answer_upload = Uuid::new_v4();
        let continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, status, payload) VALUES ($1, 'task.assigned', 'done', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "clarification_continuation": Uuid::new_v4(), "attachments": [{ "id": inbox_upload }] }))
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2)")
            .bind(first)
            .bind(continuation)
            .execute(&pool)
            .await?;
        let card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, body, meta) VALUES ($1, 'agent', 'Which?', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "clarification": { "task_id": task, "session_id": session } }))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, body, run_id, meta) VALUES ($1, 'user', 'This', $2, $3)",
        )
        .bind(agent)
        .bind(first)
        .bind(json!({ "answers": card, "attachments": [{ "id": answer_upload }] }))
        .execute(&pool)
        .await?;
        let diagnosis = store.record_run_end(first, "task", None, "", false).await?;
        assert_eq!(diagnosis.cause, Cause::NeverStarted);
        assert!(!diagnosis.auto_retried);
        assert!(diagnosis.duration_secs >= 2);
        let stored: Option<String> = sqlx::query_scalar("SELECT cause FROM session_runs WHERE id = $1").bind(first).fetch_one(&pool).await?;
        assert_eq!(stored.as_deref(), Some("never_started"));
        assert_eq!(store.nudge_missing(first, session, Required::Report).await?, Nudge::Exhausted, "a session that never started has nothing to nudge");

        let retry = store.auto_retry(first, &diagnosis).await?.expect("first never_started run is retried");
        let retry_payloads = store.run_answer_payloads(retry).await?;
        assert!(retry_payloads.iter().any(|payload| payload.to_string().contains(&inbox_upload.to_string())));
        assert!(retry_payloads.iter().any(|payload| payload.to_string().contains(&answer_upload.to_string())));
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
        sqlx::query("UPDATE agent_tasks SET status = 'failed', error_message = 'manual retry' WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'failed', working_dir = '/tmp/loka-retry-test' WHERE id = $1")
            .bind(session)
            .execute(&pool)
            .await?;
        let manual = store.retry_run(retry, user).await?.run.expect("manual retry run");
        let manual_payloads = store.run_answer_payloads(manual.id).await?;
        assert!(manual_payloads.iter().any(|payload| payload.to_string().contains(&inbox_upload.to_string())));
        assert!(manual_payloads.iter().any(|payload| payload.to_string().contains(&answer_upload.to_string())));

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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
    async fn clarification_answers_join_followups_in_both_orders() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("followup-clarification-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Kim', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("followup-clarification-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'keep the answer', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let session_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id)
             VALUES ($1, 'task', 'claude', 'haiku', 'clarification-cli', 'active', $2) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let session = store.get_session(session_id).await?.expect("task session");
        let save_answer = |answer: &'static str, accepted_status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2) RETURNING id",
                )
                .bind(agent)
                .bind(json!({
                    "task_id": task,
                    "resume": true,
                    "brief": format!("Original request:\nKeep it private\n\nQuestion:\nWhich account?\n\nTheir answer:\n{answer}"),
                    "clarification_continuation": Uuid::new_v4(),
                    "clarification_status": accepted_status,
                    "clarification_resume": accepted_status == "failed",
                    "attachments": [{ "id": Uuid::new_v4(), "name": "private.bin" }],
                }))
                .fetch_one(&pool)
                .await
            }
        };

        let answer_first = save_answer("Team", "in_progress").await?;
        let first = store.start_followup(&session, "also keep the ordinary follow-up", "Ana", false).await?;
        let first_prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1")
            .bind(first.id)
            .fetch_one(&pool)
            .await?;
        assert!(first_prompt.contains("also keep the ordinary follow-up"));
        assert!(first_prompt.contains("Their answer:\nTeam"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runner_run_inbox WHERE run_id = $1 AND inbox_id = $2")
                .bind(first.id)
                .bind(answer_first)
                .fetch_one(&pool)
                .await?,
            1
        );
        assert!(store.pending_steers(session.id).await?.iter().all(|steer| steer.id != first.id));
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1")
            .bind(first.id)
            .execute(&pool)
            .await?;

        let followup_first = store.start_followup(&session, "the queued request", "Ana", false).await?;
        let second_answer = save_answer("Personal", "in_progress").await?;
        let mut tx = pool.begin().await?;
        assert_eq!(
            adopt_saved_clarifications_into_queued_run(&mut tx, agent, task).await?,
            Some(followup_first.id)
        );
        assert_eq!(adopt_saved_clarifications_into_queued_run(&mut tx, agent, task).await?, None);
        tx.commit().await?;
        let second_prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1")
            .bind(followup_first.id)
            .fetch_one(&pool)
            .await?;
        assert!(second_prompt.contains("Their answer:\nPersonal"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runner_run_inbox WHERE run_id = $1 AND inbox_id = $2")
                .bind(followup_first.id)
                .bind(second_answer)
                .fetch_one(&pool)
                .await?,
            1
        );

        sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1")
            .bind(followup_first.id)
            .execute(&pool)
            .await?;
        let during_run = save_answer("Shared", "in_progress").await?;
        let item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE id = $1
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(during_run)
        .fetch_one(&pool)
        .await?;
        let TaskResume::Resumed { session_id, run_id } = store
            .resume_clarification_task_session(agent, task, "Kim", Some("continue"), &item)
            .await?
        else {
            panic!("the accepted answer should wait behind the running work")
        };
        assert_eq!(session_id, session.id);
        assert_ne!(run_id, followup_first.id);
        assert!(store.pending_steers(session.id).await?.iter().all(|steer| steer.id != run_id));
        assert!(!store.mark_steered(run_id, followup_first.id).await?);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runner_run_inbox WHERE run_id = $1 AND inbox_id = $2")
                .bind(run_id)
                .bind(during_run)
                .fetch_one(&pool)
                .await?,
            1
        );
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id IN ($1, $2)")
            .bind(followup_first.id)
            .bind(run_id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'failed' WHERE id = $1")
            .bind(session.id)
            .execute(&pool)
            .await?;
        let failed_answer = save_answer("Retry", "failed").await?;
        let failed_followup = store.start_followup(&session, "try the failed quest again", "Ana", false).await?;
        let failed_prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1")
            .bind(failed_followup.id)
            .fetch_one(&pool)
            .await?;
        assert!(failed_prompt.contains("Their answer:\nRetry"));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM agent_inbox WHERE id = $1")
                .bind(failed_answer)
                .fetch_one(&pool)
                .await?,
            "done"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1")
                .bind(task)
                .fetch_one(&pool)
                .await?,
            "in_progress"
        );
        Ok(())
    }

    #[tokio::test]
    async fn steer_queue_delivers_direct_messages_into_running_folder_work() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().read().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        sqlx::query(
            "UPDATE agent_tasks SET pr_url = 'https://github.com/acme/app/pull/9', merged_pr_url = 'https://github.com/acme/app/pull/9' WHERE id = $1",
        )
        .bind(merged)
        .execute(&pool)
        .await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("merged-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Mer', 'codex', 'gpt', $2) RETURNING id")
            .bind(format!("mer-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let cases = [
            ("review", Some("https://github.com/o/r/pull/1"), "done", None, Some("https://github.com/o/r/pull/1"), "done", true),
            ("in_progress", Some("https://github.com/o/r/pull/2"), "done", None, Some("https://github.com/o/r/pull/2"), "done", true),
            ("review", Some("https://github.com/o/r/pull/3"), "active", None, Some("https://github.com/o/r/pull/3"), "done", true),
            ("review", None, "done", None, None, "review", false),
            ("failed", Some("https://github.com/o/r/pull/5"), "done", None, Some("https://github.com/o/r/pull/5"), "done", true),
            ("cancelled", Some("https://github.com/o/r/pull/6"), "done", None, Some("https://github.com/o/r/pull/6"), "cancelled", false),
            ("in_progress", Some("https://github.com/o/r/pull/7"), "done", Some("running"), Some("https://github.com/o/r/pull/7"), "in_progress", false),
            ("review", Some("https://github.com/o/r/pull/8"), "done", Some("completed"), Some("https://github.com/o/r/pull/8"), "done", true),
            ("done", Some("https://github.com/o/r/pull/9"), "done", None, None, "done", false),
            ("failed", Some("https://github.com/o/r/pull/10"), "done", None, None, "failed", false),
            ("review", Some("https://github.com/o/r/pull/11"), "done", None, Some("https://github.com/o/r/pull/12"), "review", false),
        ];
        let mut tasks = Vec::new();
        for (status, pr, merge, run, merged_pr, expected, settles) in cases {
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
            if let Some(merged_pr) = merged_pr {
                sqlx::query("UPDATE agent_tasks SET merged_pr_url = $2 WHERE id = $1")
                    .bind(task)
                    .bind(merged_pr)
                    .execute(&pool)
                    .await?;
            }
            if let Some(run) = run {
                let session = Uuid::new_v4();
                insert_session(&pool, session, agent, "task", "active", Some(task), None, Some("cli-task"), None, None).await?;
                insert_run(&pool, session, agent, Some(task), "followup", run, None).await?;
            }
            tasks.push((task, expected, settles));
        }
        let (merged_card, merged_continuation) = insert_closure_clarification(&pool, agent, tasks[0].0).await?;
        let settled = store.settle_merged_quests().await?;
        for (task, expected, settles) in tasks {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!(status, expected);
            assert_eq!(settled.contains(&task), settles);
            let open_work: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_milestones WHERE task_id = $1 AND kind = 'work' AND status = 'pending'")
                .bind(task)
                .fetch_one(&pool)
                .await?;
            assert_eq!(open_work, i64::from(!settles));
        }
        let merged_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(merged_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(merged_answers, 1);
        let merged_continuation_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(merged_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(merged_continuation_status, "done");
        assert!(store.settle_merged_quests().await?.iter().all(|id| !settled.contains(id)));
        let merged_answers_after_retry: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(merged_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(merged_answers_after_retry, 1);
        Ok(())
    }

    #[tokio::test]
    async fn automatic_terminal_status_settles_clarifications_once() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let db = crate::db::Database::from_pool(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("terminal-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, status, status_detail)
             VALUES ($1, 'Ter', 'codex', 'gpt', $2, 'waiting', 'Waiting for answers') RETURNING id",
        )
        .bind(format!("ter-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let unrelated_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'unrelated', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;

        let done_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'finish', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let (done_card, done_continuation) = insert_closure_clarification(&pool, agent, done_task).await?;
        let done_inbox_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert!(db.update_status(done_task, "done").await?);
        assert!(db.update_status(done_task, "done").await?);
        let done_inbox_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(done_inbox_after, done_inbox_before);

        let failed_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'fail', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let (failed_card, failed_continuation) = insert_closure_clarification(&pool, agent, failed_task).await?;
        let failed_inbox_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        db.set_error(failed_task, "failed once").await?;
        let failed_inbox_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(failed_inbox_after, failed_inbox_before);
        let (resumed_card, resumed_continuation) = insert_closure_clarification(&pool, agent, failed_task).await?;
        let retry_inbox_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        db.set_error(failed_task, "failed again").await?;
        let retry_inbox_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(retry_inbox_after, retry_inbox_before);

        for (card, answers) in [(done_card, 1_i64), (failed_card, 1), (resumed_card, 0)] {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
                .bind(card.to_string())
                .fetch_one(&pool)
                .await?;
            assert_eq!(count, answers);
        }
        for (continuation, expected) in [
            (done_continuation, "done"),
            (failed_continuation, "done"),
            (resumed_continuation, "pending"),
        ] {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
                .bind(continuation)
                .fetch_one(&pool)
                .await?;
            assert_eq!(status, expected);
        }
        let quiet_answers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events
             WHERE type = 'message.created' AND payload->>'answer_card' IN ($1, $2) AND payload->>'quiet' = 'true'",
        )
        .bind(done_card.to_string())
        .bind(failed_card.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(quiet_answers, 2);
        let answered_agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, status, status_detail)
             VALUES ($1, 'Ans', 'codex', 'gpt', $2, 'waiting', 'Waiting for work') RETURNING id",
        )
        .bind(format!("ans-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let answered_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'answered', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(answered_agent)
        .fetch_one(&pool)
        .await?;
        let (answered_card, answered_continuation) = insert_closure_clarification(&pool, answered_agent, answered_task).await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta)
             VALUES ($1, 'user', $2, 'Use the default', $3)",
        )
        .bind(answered_agent)
        .bind(user)
        .bind(json!({ "answers": answered_card }))
        .execute(&pool)
        .await?;
        assert!(db.update_status(answered_task, "done").await?);
        let answered_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(answered_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(answered_count, 1);
        let answered_continuation_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(answered_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(answered_continuation_status, "done");
        assert_eq!(store.get_agent(answered_agent).await?.expect("answered agent").status, "idle");
        let unrelated_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(unrelated_task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(unrelated_status, "in_progress");
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "idle");
        Ok(())
    }

    #[tokio::test]
    async fn child_completion_settles_only_newly_closed_parent_clarifications() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let db = crate::db::Database::from_pool(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("parent-close-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Par', 'codex', 'gpt', $2) RETURNING id",
        )
        .bind(format!("par-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let parent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'parent', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let child: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id)
             VALUES ($1, 'child', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(parent)
        .fetch_one(&pool)
        .await?;
        let (parent_card, parent_continuation) = insert_closure_clarification(&pool, agent, parent).await?;
        assert!(db.update_status(child, "done").await?);
        let parent_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(parent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(parent_status, "done");
        let parent_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(parent_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(parent_answers, 1);
        let parent_continuation_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(parent_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(parent_continuation_status, "done");

        let old_parent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, completed_at)
             VALUES ($1, 'old parent', 'done', $2, NOW() - INTERVAL '1 minute') RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let old_child: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id)
             VALUES ($1, 'old child', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(old_parent)
        .fetch_one(&pool)
        .await?;
        let (old_card, old_continuation) = insert_closure_clarification(&pool, agent, old_parent).await?;
        assert!(db.update_status(old_child, "done").await?);
        let old_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
            .bind(old_card.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(old_answers, 0);
        let old_continuation_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(old_continuation)
            .fetch_one(&pool)
            .await?;
        assert_eq!(old_continuation_status, "pending");
        Ok(())
    }

    #[tokio::test]
    async fn legacy_delete_settles_clarifications_before_cascading() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let options = url.parse::<PgConnectOptions>()?.statement_cache_capacity(0);
        let pool = PgPoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = WorldStore::new(pool.clone());
        let db = crate::db::Database::from_pool(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Owner', 'write') RETURNING id")
            .bind(format!("delete-owner-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Other', 'write') RETURNING id")
            .bind(format!("delete-other-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, status, status_detail)
             VALUES ($1, 'Del', 'codex', 'gpt', $2, 'waiting', 'Waiting for answers') RETURNING id",
        )
        .bind(format!("delete-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let parent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'parent', 'in_progress', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let child: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id)
             VALUES ($1, 'child', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .bind(parent)
        .fetch_one(&pool)
        .await?;
        let (parent_card, parent_continuation) = insert_closure_clarification(&pool, agent, parent).await?;
        let (child_card, child_continuation) = insert_closure_clarification(&pool, agent, child).await?;
        let contextual_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Which child detail?', $2) RETURNING id",
        )
        .bind(agent)
        .bind(json!({ "kind": "ask", "clarify_request": { "context_task_id": child } }))
        .fetch_one(&pool)
        .await?;
        let inbox_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;

        assert_eq!(db.delete_task(parent, other).await?, 0);
        let untouched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_tasks WHERE id IN ($1, $2)")
            .bind(parent)
            .bind(child)
            .fetch_one(&pool)
            .await?;
        assert_eq!(untouched, 2);
        let unauthorized_answers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' IN ($1, $2)",
        )
        .bind(parent_card.to_string())
        .bind(child_card.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(unauthorized_answers, 0);
        let unauthorized_contextual_answers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1",
        )
        .bind(contextual_card.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(unauthorized_contextual_answers, 0);
        for continuation in [parent_continuation, child_continuation] {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
                .bind(continuation)
                .fetch_one(&pool)
                .await?;
            assert_eq!(status, "pending");
        }
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "waiting");

        assert_eq!(db.delete_task(parent, owner).await?, 1);
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_tasks WHERE id IN ($1, $2)")
            .bind(parent)
            .bind(child)
            .fetch_one(&pool)
            .await?;
        assert_eq!(remaining, 0);
        for card in [parent_card, child_card, contextual_card] {
            let answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'answers' = $1")
                .bind(card.to_string())
                .fetch_one(&pool)
                .await?;
            assert_eq!(answers, 1);
        }
        for continuation in [parent_continuation, child_continuation] {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
                .bind(continuation)
                .fetch_one(&pool)
                .await?;
            assert_eq!(status, "done");
        }
        let inbox_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(inbox_after, inbox_before);
        assert_eq!(store.get_agent(agent).await?.expect("agent").status, "idle");
        Ok(())
    }

    #[tokio::test]
    async fn confirmed_pr_results_settle_every_quest_state_once() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("pr-results-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Mer', 'codex', 'gpt', $2) RETURNING id")
            .bind(format!("pr-results-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let mut tasks = Vec::new();
        for (index, status) in ["failed", "failed", "review"].into_iter().enumerate() {
            let pr = format!("https://github.com/o/r/pull/{}", index + 20);
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url, error_message)
                 VALUES ($1, 'ship it', $2, $3, $4, 'earlier failure') RETURNING id",
            )
            .bind(user)
            .bind(status)
            .bind(agent)
            .bind(&pr)
            .fetch_one(&pool)
            .await?;
            sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(task).execute(&pool).await?;
            sqlx::query(
                "INSERT INTO task_milestones (task_id, title, kind, status)
                 VALUES ($1, 'work', 'work', 'pending'), ($1, 'merge', 'merge', 'active')",
            )
            .bind(task)
            .execute(&pool)
            .await?;
            if index == 1 {
                let session = Uuid::new_v4();
                insert_session(&pool, session, agent, "task", "failed", Some(task), None, Some("cli-task"), None, None).await?;
                let run = insert_run(&pool, session, agent, Some(task), "task", "timed_out", None).await?;
                sqlx::query("UPDATE session_runs SET cause = 'timed_out' WHERE id = $1").bind(run).execute(&pool).await?;
            }
            assert!(store.settle_pr(task, &pr, true).await?);
            assert!(store.settle_pr(task, &pr, true).await?);
            assert!(!store.settle_pr(task, &pr, false).await?);
            tasks.push(task);
        }
        for task in &tasks {
            let state: (String, Option<String>, i64, i64) = sqlx::query_as(
                "SELECT t.status, t.error_message, COUNT(*) FILTER (WHERE m.status = 'done'), COUNT(*) FILTER (WHERE m.status = 'skipped')
                 FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id
                 WHERE t.id = $1 GROUP BY t.id",
            )
            .bind(*task)
            .fetch_one(&pool)
            .await?;
            assert_eq!((state.0.as_str(), state.1, state.2, state.3), ("done", None, 1, 1));
            let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.merged'")
                .bind(*task)
                .fetch_one(&pool)
                .await?;
            assert_eq!(events, 1);
        }

        let work_pr = "https://github.com/o/r/pull/29";
        let work_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url)
             VALUES ($1, 'work states', 'failed', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(work_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(work_task).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, detail, done_at)
             VALUES ($1, 'merge', 'merge', 'active', 'Waiting', NULL),
                    ($1, 'pending work', 'work', 'pending', 'Not started', NULL),
                    ($1, 'active work', 'work', 'active', 'In progress', NULL),
                    ($1, 'done work', 'work', 'done', 'Passed', TIMESTAMPTZ '2024-01-01 00:00:00+00'),
                    ($1, 'failed work', 'work', 'failed', 'Failed check', TIMESTAMPTZ '2024-01-02 00:00:00+00')",
        )
        .bind(work_task)
        .execute(&pool)
        .await?;
        assert!(store.settle_pr(work_task, work_pr, true).await?);
        type WorkMilestoneState = (String, String, Option<String>, Option<DateTime<Utc>>, DateTime<Utc>);
        let work_states: Vec<WorkMilestoneState> = sqlx::query_as(
            "SELECT title, status, detail, done_at, updated_at FROM task_milestones WHERE task_id = $1 ORDER BY title",
        )
        .bind(work_task)
        .fetch_all(&pool)
        .await?;
        let state = |title: &str| work_states.iter().find(|row| row.0 == title).expect("milestone");
        assert_eq!((state("merge").1.as_str(), state("merge").2.as_deref()), ("done", Some("Merged")));
        assert!(state("merge").3.is_some());
        assert_eq!((state("pending work").1.as_str(), state("pending work").2.as_deref(), state("pending work").3), ("skipped", Some("Not started"), None));
        assert_eq!((state("active work").1.as_str(), state("active work").2.as_deref(), state("active work").3), ("skipped", Some("In progress"), None));
        assert_eq!((state("done work").1.as_str(), state("done work").2.as_deref(), state("done work").3.map(|at| at.date_naive().to_string())), ("done", Some("Passed"), Some("2024-01-01".into())));
        assert_eq!((state("failed work").1.as_str(), state("failed work").2.as_deref(), state("failed work").3.map(|at| at.date_naive().to_string())), ("failed", Some("Failed check"), Some("2024-01-02".into())));
        let updated: Vec<(String, DateTime<Utc>)> = work_states.iter().map(|row| (row.0.clone(), row.4)).collect();
        assert!(store.settle_pr(work_task, work_pr, true).await?);
        let updated_again: Vec<(String, DateTime<Utc>)> =
            sqlx::query_as("SELECT title, updated_at FROM task_milestones WHERE task_id = $1 ORDER BY title")
                .bind(work_task)
                .fetch_all(&pool)
                .await?;
        assert_eq!(updated_again, updated);

        let owner_tasks = store.owner_tasks(user, None, Some(agent), false, 30).await?;
        let agent_tasks = store.agent_tasks(Some(agent), false, 30).await?;
        for task in &tasks {
            assert!(owner_tasks.iter().any(|row| row.id == *task && row.status == "done"));
            assert!(agent_tasks.iter().any(|row| row.id == *task && row.status == "done"));
            let card_status: String = sqlx::query_scalar(
                "SELECT meta->>'status' FROM agent_messages WHERE meta->>'task_id' = $1 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(task.to_string())
            .fetch_one(&pool)
            .await?;
            assert_eq!(card_status, "done");
        }

        let webhook_pr = "https://github.com/acme/Castle/pull/34";
        let webhook_payload_pr = "https://github.com/acme/castle/pull/34";
        let webhook_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch, pr_url, pr_number, error_message)
             VALUES ($1, 'webhook merge', 'failed', $2, 'https://github.com/acme/castle.git', 'mer-34/fix', $3, 34, 'earlier failure') RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(webhook_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(webhook_task).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status)
             VALUES ($1, 'work', 'work', 'pending'), ($1, 'merge', 'merge', 'active')",
        )
        .bind(webhook_task)
        .execute(&pool)
        .await?;
        assert!(store
            .settle_pr_from_webhook(webhook_task, webhook_result(webhook_payload_pr, "acme/castle", "mer-35/fix", 34, true, None))
            .await?);
        assert!(!store
            .settle_pr_from_webhook(webhook_task, webhook_result(webhook_payload_pr, "acme/castle", "mer-35/fix", 35, true, None))
            .await?);
        sqlx::query("UPDATE agent_tasks SET repo = 'Warlord-K/other', pr_number = 999 WHERE id = $1")
            .bind(webhook_task)
            .execute(&pool)
            .await?;
        let webhook_db = crate::db::Database::connect(&url).await?;
        assert_eq!(
            webhook_db
                .find_task_by_branch("acme/castle", "mer-34/fix-2", Some(webhook_payload_pr))
                .await?
                .map(|task| task.id),
            Some(webhook_task)
        );
        assert!(webhook_db
            .find_task_by_branch(
                "Warlord-K/other",
                "mer-34/fix",
                Some("https://github.com/Warlord-K/other/pull/34"),
            )
            .await?
            .is_none());
        assert_eq!(
            webhook_db
                .find_task_by_branch("Warlord-K/other", "mer-34/fix", None)
                .await?
                .map(|task| task.id),
            Some(webhook_task)
        );
        assert!(store
            .settle_pr_from_webhook(webhook_task, webhook_result(webhook_payload_pr, "acme/castle", "mer-34/fix", 34, true, None))
            .await?);
        let webhook_state: (String, Option<String>, Option<String>, Option<i32>, i64, i64) = sqlx::query_as(
            "SELECT t.status, t.error_message, t.pr_url, t.pr_number,
                    COUNT(*) FILTER (WHERE m.status = 'done'), COUNT(*) FILTER (WHERE m.status = 'skipped')
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id
             WHERE t.id = $1 GROUP BY t.id",
        )
        .bind(webhook_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(webhook_state, ("done".into(), None, Some(webhook_pr.into()), Some(34), 1, 1));
        let webhook_card: (String, String) = sqlx::query_as(
            "SELECT meta->>'status', meta->>'pr_url' FROM agent_messages
             WHERE meta->>'task_id' = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(webhook_task.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(webhook_card, ("done".into(), webhook_pr.into()));

        let closed_webhook_pr = "https://github.com/acme/castle/pull/38";
        let closed_webhook: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch, error_message)
             VALUES ($1, 'webhook close', 'failed', $2, 'acme/castle', 'mer-38/fix', 'earlier failure') RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(closed_webhook).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, detail, done_at)
             VALUES ($1, 'work', 'work', 'done', 'Built', NOW()),
                    ($1, 'merge', 'merge', 'skipped', 'No pull request was opened', NULL)",
        )
        .bind(closed_webhook)
        .execute(&pool)
        .await?;
        let first_closed_at = Utc::now() - Duration::minutes(5);
        assert!(!store
            .settle_pr_from_webhook(closed_webhook, webhook_result(closed_webhook_pr, "acme/castle", "mer-39/fix", 38, false, None))
            .await?);
        assert!(!store
            .settle_pr_from_webhook(closed_webhook, webhook_result(closed_webhook_pr, "Warlord-K/other", "mer-38/fix", 38, false, None))
            .await?);
        assert!(store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, Some(first_closed_at)),
            )
            .await?);
        assert!(store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, Some(first_closed_at)),
            )
            .await?);
        type ClosedWebhookState = (
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            String,
            Option<String>,
            String,
            Option<String>,
        );
        let closed_webhook_state: ClosedWebhookState = sqlx::query_as(
            "SELECT t.status, t.error_message, t.pr_url, t.pr_number,
                    merge.status, merge.data->>'closed_pr_url', work.status, work.detail
             FROM agent_tasks t
             JOIN task_milestones merge ON merge.task_id = t.id AND merge.kind = 'merge'
             JOIN task_milestones work ON work.task_id = t.id AND work.kind = 'work'
             WHERE t.id = $1",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            closed_webhook_state,
            (
                "failed".into(),
                Some(super::PR_CLOSED_ERROR.into()),
                Some(closed_webhook_pr.into()),
                Some(38),
                "failed".into(),
                Some(closed_webhook_pr.into()),
                "done".into(),
                Some("Built".into()),
            )
        );
        let closed_webhook_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.pr_closed' AND payload->>'pr_url' = $2",
        )
        .bind(closed_webhook)
        .bind(closed_webhook_pr)
        .fetch_one(&pool)
        .await?;
        assert_eq!(closed_webhook_events, 1);
        let milestone_store = MilestoneStore::new(pool.clone());
        let reopened_at = first_closed_at + Duration::minutes(2);
        assert!(milestone_store.rearm_open_pr(closed_webhook, closed_webhook_pr, reopened_at).await?);
        assert!(!milestone_store.rearm_open_pr(closed_webhook, closed_webhook_pr, reopened_at).await?);
        assert!(!milestone_store
            .rearm_open_pr(closed_webhook, closed_webhook_pr, reopened_at - Duration::seconds(1))
            .await?);
        assert!(!store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, Some(first_closed_at)),
            )
            .await?);
        let ordered_open_state: (String, String, f64, String) = sqlx::query_as(
            "SELECT t.status, m.status, (m.data->>'recheck_after')::double precision, m.data->>'recheck_pr_url'
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert_eq!((ordered_open_state.0.as_str(), ordered_open_state.1.as_str(), ordered_open_state.3.as_str()), ("failed", "active", closed_webhook_pr));
        assert!((ordered_open_state.2 - reopened_at.timestamp_micros() as f64 / 1_000_000.0).abs() <= 0.000001);
        assert!(milestone_store.retry_check_now(closed_webhook).await?);
        assert!(!store
            .settle_pr_from_webhook(closed_webhook, webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, None))
            .await?);
        assert!(!store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, Some(first_closed_at)),
            )
            .await?);
        let rechecked_state: (String, String) = sqlx::query_as(
            "SELECT t.status, m.status FROM agent_tasks t
             JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge' WHERE t.id = $1",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert_eq!(rechecked_state, ("failed".into(), "active".into()));
        let later_closed_at = Utc::now();
        assert!(store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(
                    "https://github.com/acme/CASTLE/pull/38",
                    "acme/CASTLE",
                    "mer-38/fix",
                    38,
                    false,
                    Some(later_closed_at),
                ),
            )
            .await?);
        let closed_webhook_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.pr_closed' AND payload->>'pr_url' = $2",
        )
        .bind(closed_webhook)
        .bind(closed_webhook_pr)
        .fetch_one(&pool)
        .await?;
        assert_eq!(closed_webhook_events, 2);
        assert!(store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(
                    "https://github.com/acme/CASTLE/pull/38",
                    "acme/CASTLE",
                    "mer-38/fix",
                    38,
                    false,
                    Some(later_closed_at),
                ),
            )
            .await?);
        let replayed_close_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.pr_closed' AND payload->>'pr_url' = $2",
        )
        .bind(closed_webhook)
        .bind(closed_webhook_pr)
        .fetch_one(&pool)
        .await?;
        assert_eq!(replayed_close_events, 2);
        sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1")
            .bind(closed_webhook)
            .execute(&pool)
            .await?;
        assert!(!store
            .settle_pr_from_webhook(
                closed_webhook,
                webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, Some(later_closed_at)),
            )
            .await?);
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1")
            .bind(closed_webhook)
            .execute(&pool)
            .await?;
        let saved_closed_at: f64 = sqlx::query_scalar(
            "SELECT (data->>'closed_at')::double precision FROM task_milestones WHERE task_id = $1 AND kind = 'merge'",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert!((saved_closed_at - later_closed_at.timestamp_millis() as f64 / 1000.0).abs() < 0.001);
        assert!(milestone_store.retry_check_now(closed_webhook).await?);
        let retry_order: (f64, f64) = sqlx::query_as(
            "SELECT (data->>'closed_at')::double precision, (data->>'recheck_after')::double precision
             FROM task_milestones WHERE task_id = $1 AND kind = 'merge'",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert_eq!(retry_order.0, saved_closed_at);
        assert!(retry_order.1 >= retry_order.0);
        assert!(store
            .settle_pr_from_webhook(closed_webhook, webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, true, None))
            .await?);
        assert!(!store
            .settle_pr_from_webhook(closed_webhook, webhook_result(closed_webhook_pr, "acme/castle", "mer-38/fix", 38, false, None))
            .await?);
        let reopened_state: (String, Option<String>, Option<String>, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, t.merged_pr_url, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(closed_webhook)
        .fetch_one(&pool)
        .await?;
        assert_eq!(reopened_state, ("done".into(), None, Some(closed_webhook_pr.into()), "done".into()));
        let terminal_close_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.pr_closed' AND payload->>'pr_url' = $2",
        )
        .bind(closed_webhook)
        .bind(closed_webhook_pr)
        .fetch_one(&pool)
        .await?;
        assert_eq!(terminal_close_events, 2);

        let manual_skip_pr = "https://github.com/acme/castle/pull/40";
        let manual_skip: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch)
             VALUES ($1, 'manual skip', 'failed', $2, 'acme/castle', 'mer-40/fix') RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(manual_skip).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, detail)
             VALUES ($1, 'merge', 'merge', 'skipped', 'Not applicable')",
        )
        .bind(manual_skip)
        .execute(&pool)
        .await?;
        assert!(store
            .settle_pr_from_webhook(manual_skip, webhook_result(manual_skip_pr, "acme/castle", "mer-40/fix", 40, false, None))
            .await?);
        let manual_skip_state: (String, Option<String>, String, Option<String>) = sqlx::query_as(
            "SELECT t.status, t.error_message, m.status, m.detail
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(manual_skip)
        .fetch_one(&pool)
        .await?;
        assert_eq!(manual_skip_state, ("failed".into(), Some(super::PR_CLOSED_ERROR.into()), "skipped".into(), Some("Not applicable".into())));

        let different_pr = "https://github.com/acme/castle/pull/36";
        let different: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch, pr_url, pr_number)
             VALUES ($1, 'different PR', 'failed', $2, 'acme/castle', 'mer-35/fix', $3, 36) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(different_pr)
        .fetch_one(&pool)
        .await?;
        assert!(!store
            .settle_pr_from_webhook(
                different,
                webhook_result("https://github.com/acme/castle/pull/35", "acme/castle", "mer-35/fix", 35, true, None),
            )
            .await?);
        assert!(!store
            .settle_pr_from_webhook(
                different,
                webhook_result("https://github.com/acme/castle/pull/35", "acme/castle", "mer-35/fix", 35, false, None),
            )
            .await?);
        let different_state: (String, Option<String>, Option<i32>) =
            sqlx::query_as("SELECT status, pr_url, pr_number FROM agent_tasks WHERE id = $1")
                .bind(different)
                .fetch_one(&pool)
                .await?;
        assert_eq!(different_state, ("failed".into(), Some(different_pr.into()), Some(36)));

        let cancelled_webhook: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch)
             VALUES ($1, 'cancelled webhook', 'cancelled', $2, 'acme/castle', 'mer-37/fix') RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert!(!store
            .settle_pr_from_webhook(
                cancelled_webhook,
                webhook_result("https://github.com/acme/castle/pull/37", "acme/castle", "mer-37/fix", 37, false, None),
            )
            .await?);
        assert!(!store
            .settle_pr_from_webhook(
                cancelled_webhook,
                webhook_result("https://github.com/acme/castle/pull/37", "acme/castle", "mer-37/fix", 37, true, None),
            )
            .await?);
        let cancelled_webhook_state: (String, Option<String>, Option<i32>) =
            sqlx::query_as("SELECT status, pr_url, pr_number FROM agent_tasks WHERE id = $1")
                .bind(cancelled_webhook)
                .fetch_one(&pool)
                .await?;
        assert_eq!(cancelled_webhook_state, ("cancelled".into(), None, None));

        let done_webhook_pr = "https://github.com/acme/castle/pull/39";
        let done_webhook: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch, pr_url, pr_number)
             VALUES ($1, 'done webhook', 'done', $2, 'acme/castle', 'mer-39/fix', $3, 39) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(done_webhook_pr)
        .fetch_one(&pool)
        .await?;
        assert!(!store
            .settle_pr_from_webhook(done_webhook, webhook_result(done_webhook_pr, "acme/castle", "mer-39/fix", 39, false, None))
            .await?);
        let done_webhook_state: (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, error_message, merged_pr_url FROM agent_tasks WHERE id = $1")
                .bind(done_webhook)
                .fetch_one(&pool)
                .await?;
        assert_eq!(done_webhook_state, ("done".into(), None, None));

        let cancelled_pr = "https://github.com/o/r/pull/30";
        let cancelled: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url) VALUES ($1, 'cancelled', 'cancelled', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(cancelled_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(cancelled).execute(&pool).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'active')")
            .bind(cancelled)
            .execute(&pool)
            .await?;
        assert!(!store.settle_pr(cancelled, cancelled_pr, true).await?);
        let cancelled_state: (String, String) = sqlx::query_as(
            "SELECT t.status, m.status FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id WHERE t.id = $1",
        )
        .bind(cancelled)
        .fetch_one(&pool)
        .await?;
        assert_eq!(cancelled_state, ("cancelled".into(), "active".into()));

        let closed_pr = "https://github.com/o/r/pull/31";
        let closed: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url) VALUES ($1, 'closed', 'failed', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(closed_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(closed).execute(&pool).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'active')")
            .bind(closed)
            .execute(&pool)
            .await?;
        assert!(store.settle_pr(closed, closed_pr, false).await?);
        let closed_state: (String, Option<String>, String, Option<String>) = sqlx::query_as(
            "SELECT t.status, t.error_message, m.status, m.data->>'closed_pr_url'
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id WHERE t.id = $1",
        )
        .bind(closed)
        .fetch_one(&pool)
        .await?;
        assert_eq!(closed_state, ("failed".into(), Some(super::PR_CLOSED_ERROR.into()), "failed".into(), Some(closed_pr.into())));

        let team: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_teams (owner_user_id, name, slug, lead_agent_id) VALUES ($1, 'Merge team', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(format!("merge-team-{tag}"))
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let parent_cases = [
            ("active solo parent", None, "in_progress", None, "done", None),
            ("failed solo parent", None, "failed", Some("earlier failure"), "done", None),
            ("cancelled solo parent", None, "cancelled", Some("cancelled earlier"), "cancelled", Some("cancelled earlier")),
            ("failed team parent", Some(team), "failed", Some("earlier failure"), "done", None),
            ("cancelled team parent", Some(team), "cancelled", Some("cancelled earlier"), "cancelled", Some("cancelled earlier")),
        ];
        for (index, (title, team_id, initial_status, initial_error, expected_status, expected_error)) in parent_cases.into_iter().enumerate() {
            let parent: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, error_message, assigned_agent_id, team_id)
                 VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(user)
            .bind(title)
            .bind(initial_status)
            .bind(initial_error)
            .bind(agent)
            .bind(team_id)
            .fetch_one(&pool)
            .await?;
            let first: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id, team_id)
                 VALUES ($1, 'first', 'in_progress', $2, $3, $4) RETURNING id",
            )
            .bind(user)
            .bind(agent)
            .bind(parent)
            .bind(team_id)
            .fetch_one(&pool)
            .await?;
            let child_pr = format!("https://github.com/o/r/pull/{}", index + 32);
            let child: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, parent_task_id, team_id, pr_url)
                 VALUES ($1, 'last', 'failed', $2, $3, $4, $5) RETURNING id",
            )
            .bind(user)
            .bind(agent)
            .bind(parent)
            .bind(team_id)
            .bind(&child_pr)
            .fetch_one(&pool)
            .await?;
            sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(child).execute(&pool).await?;
            sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'active')")
                .bind(child)
                .execute(&pool)
                .await?;
            sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(first).execute(&pool).await?;
            let parent_before: (String, Option<String>) =
                sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
                    .bind(parent)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(parent_before, (initial_status.into(), initial_error.map(str::to_string)), "{title}");
            assert!(store.settle_pr(child, &child_pr, true).await?);
            assert!(store.settle_pr(child, &child_pr, true).await?);
            let parent_state: (String, Option<String>) =
                sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
                    .bind(parent)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(parent_state, (expected_status.into(), expected_error.map(str::to_string)), "{title}");
            let auto_closed: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM world_events
                 WHERE task_id = $1 AND type = 'task.finished' AND payload->>'auto_closed' = 'true'",
            )
            .bind(parent)
            .fetch_one(&pool)
            .await?;
            assert_eq!(auto_closed, i64::from(expected_status == "done"), "{title}");
        }

        let runner_owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Device owner', 'write') RETURNING id")
            .bind(format!("pr-runner-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let runner = Uuid::new_v4();
        let token_hash = [runner.as_bytes().as_slice(), runner.as_bytes().as_slice()].concat();
        sqlx::query(
            "INSERT INTO world_runners (id, owner_user_id, name, token_hash, status) VALUES ($1, $2, 'Runner', $3, 'online')",
        )
        .bind(runner)
        .bind(runner_owner)
        .bind(token_hash)
        .execute(&pool)
        .await?;
        let runner_agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, runner_id) VALUES ($1, 'Runner agent', 'codex', 'gpt', $2, $3) RETURNING id",
        )
        .bind(format!("pr-runner-{tag}"))
        .bind(runner_owner)
        .bind(runner)
        .fetch_one(&pool)
        .await?;
        let team_pr = "https://github.com/o/r/pull/33";
        let team_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url) VALUES ($1, 'team task', 'failed', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(runner_agent)
        .bind(team_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(team_task).execute(&pool).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'active')")
            .bind(team_task)
            .execute(&pool)
            .await?;
        assert!(store.settle_pr_from_runner(team_task, team_pr, true, runner).await?);

        let stale_runner_pr = "https://github.com/o/r/pull/44";
        let stale_runner_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url)
             VALUES ($1, 'stale runner close', 'failed', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(runner_agent)
        .bind(stale_runner_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1").bind(stale_runner_task).execute(&pool).await?;
        let runner_claimed_at = Utc::now() - Duration::minutes(2);
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, detail, data)
             VALUES ($1, 'merge', 'merge', 'active', 'Waiting for review',
               jsonb_build_object(
                 'runner_pr_claimed_at', EXTRACT(EPOCH FROM $2::timestamptz),
                 'runner_pr_claimed_url', $3::text
               ))",
        )
        .bind(stale_runner_task)
        .bind(runner_claimed_at)
        .bind(stale_runner_pr)
        .execute(&pool)
        .await?;
        let runner_reopened_at = runner_claimed_at + Duration::minutes(1);
        assert!(MilestoneStore::new(pool.clone())
            .rearm_open_pr(stale_runner_task, stale_runner_pr, runner_reopened_at)
            .await?);
        assert!(!store
            .settle_pr_from_runner_observed(stale_runner_task, stale_runner_pr, false, runner, Some(runner_claimed_at))
            .await?);
        let fresh_runner_claim = runner_reopened_at + Duration::minutes(1);
        sqlx::query(
            "UPDATE task_milestones
             SET data = data || jsonb_build_object(
               'runner_pr_claimed_at', EXTRACT(EPOCH FROM $2::timestamptz),
               'runner_pr_claimed_url', $3::text
             )
             WHERE task_id = $1 AND kind = 'merge'",
        )
        .bind(stale_runner_task)
        .bind(fresh_runner_claim)
        .bind("https://github.com/other/r/pull/44")
        .execute(&pool)
        .await?;
        assert!(!store
            .settle_pr_from_runner_observed(stale_runner_task, stale_runner_pr, false, runner, Some(fresh_runner_claim))
            .await?);
        sqlx::query(
            "UPDATE task_milestones SET data = jsonb_set(data, '{runner_pr_claimed_url}', to_jsonb($2::text))
             WHERE task_id = $1 AND kind = 'merge'",
        )
        .bind(stale_runner_task)
        .bind(stale_runner_pr)
        .execute(&pool)
        .await?;
        assert!(store
            .settle_pr_from_runner_observed(stale_runner_task, stale_runner_pr, false, runner, Some(fresh_runner_claim))
            .await?);
        assert!(store
            .settle_pr_from_runner_observed(stale_runner_task, stale_runner_pr, false, runner, Some(fresh_runner_claim))
            .await?);
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
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Feed') RETURNING id")
            .bind(format!("feed-{tag}"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, 'Feed', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("feed-{tag}"))
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let peer: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, 'Peer', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("peer-{tag}"))
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let elsewhere: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Elsewhere') RETURNING id")
            .bind(format!("elsewhere-{}", &tag[..20]))
            .fetch_one(&pool)
            .await?;
        let outsider: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, 'Outside', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("outside-{tag}"))
            .bind(elsewhere)
            .fetch_one(&pool)
            .await?;
        for (kind, payload) in [
            ("task.finished", json!({ "tag": tag })),
            ("task.changed", json!({ "quiet": true, "tag": tag })),
            ("run.failed", json!({ "quiet": true, "tag": tag, "cause": "no_report" })),
            ("run.retrying", json!({ "quiet": true, "tag": tag })),
            ("milestones.updated", json!({ "quiet": true, "tag": tag })),
            ("message.created", json!({ "tag": tag })),
        ] {
            store.emit(NewEvent::new(kind).agent(agent).payload(payload), None).await?;
        }
        for other in [peer, outsider] {
            store
                .emit(NewEvent::new("run.failed").agent(other).payload(json!({ "quiet": true, "tag": tag, "cause": "no_report" })), None)
                .await?;
        }
        let kinds = |events: Vec<super::WorldEvent>| events.into_iter().map(|e| e.event_type).collect::<Vec<_>>();
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &[], 10).await?), vec!["task.finished", "message.created"]);
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &["task.".to_string()], 10).await?), vec!["task.finished", "task.changed"]);
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &["run.failed".to_string()], 10).await?), vec!["run.failed"]);
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &["run.retrying".to_string()], 10).await?), vec!["run.retrying"]);
        assert_eq!(kinds(store.agent_feed(None, Some(agent), &["milestones.updated".to_string()], 10).await?), vec!["milestones.updated"]);
        assert_eq!(kinds(store.agent_feed(Some(org), None, &["run.failed".to_string()], 10).await?), vec!["run.failed", "run.failed"]);
        assert!(store.agent_feed(Some(elsewhere), Some(agent), &["run.failed".to_string()], 10).await?.is_empty());
        assert_eq!(store.recent_events(Some(agent), 10).await?.len(), 6, "the raw reader behind the live stream still sees everything");
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
        let card = json!({ "task_id": quest, "quiet": true });
        assert_eq!(changes().await?, vec![(Some(agent), card.clone())], "a new quest shows up on boards");

        let session = Uuid::new_v4();
        insert_session(&pool, session, agent, "task", "failed", Some(quest), None, Some("cli-status"), None, None).await?;
        let failed = store.get_session(session).await?.expect("session");
        store.start_followup(&failed, "Try the drawer again", "Ana", false).await?;
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card.clone())), "a follow-up that reopens a failed quest tells every open card");

        let seen = changes().await?.len();
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress', title = 'Stuck drawer' WHERE id = $1").bind(quest).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET error_message = 'noise' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert_eq!(changes().await?.len(), seen, "writes that change nothing a card shows say nothing");

        sqlx::query(
            "INSERT INTO world_events (type, agent_id, task_id, payload)
             SELECT 'run.activity', $1, $2, jsonb_build_object('quiet', true, 'line', n) FROM generate_series(1, 2000) n",
        )
        .bind(agent)
        .bind(quest)
        .execute(&pool)
        .await?;
        let before = changes().await?.len();
        sqlx::query("UPDATE agent_tasks SET pr_url = 'https://github.com/acme/app/pull/7' WHERE id = $1").bind(quest).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1);
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card.clone())), "a PR link on the card");

        let before = changes().await?.len();
        sqlx::query("UPDATE agent_sessions SET plan_state = 'awaiting' WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1);
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card.clone())), "Needs you appears");
        let before = changes().await?.len();
        sqlx::query("UPDATE agent_sessions SET status = 'archived' WHERE id = $1").bind(session).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1, "archiving the waiting session clears Needs you");

        let before = changes().await?.len();
        let milestone: Uuid = sqlx::query_scalar("INSERT INTO task_milestones (task_id, title) VALUES ($1, 'Write the fix') RETURNING id")
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        assert_eq!(changes().await?.len(), before + 1);
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card.clone())));
        let before = changes().await?.len();
        sqlx::query("UPDATE task_milestones SET next_check_at = NOW(), data = '{\"sha\": \"abc\"}' WHERE id = $1").bind(milestone).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before, "merge watch bookkeeping is not a card change");
        sqlx::query("UPDATE task_milestones SET status = 'done' WHERE id = $1").bind(milestone).execute(&pool).await?;
        assert_eq!(changes().await?.len(), before + 1, "a ticked milestone is");

        let before = changes().await?.len();
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET status = 'review', title = 'Drawer almost unstuck' WHERE id = $1").bind(quest).execute(&mut *tx).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done', title = 'Drawer unstuck' WHERE id = $1").bind(quest).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title) VALUES ($1, 'Review'), ($1, 'Ship')").bind(quest).execute(&mut *tx).await?;
        tx.commit().await?;
        assert_eq!(changes().await?.len(), before + 1, "one change per quest per transaction");
        assert_eq!(changes().await?.last(), Some(&(Some(agent), card)));
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_tasks WHERE id = $1").bind(quest).fetch_one(&pool).await?;
        let refetched = store.quests(user, org, 300).await?.into_iter().find(|task| task.id == quest).expect("quest on board");
        assert_eq!((refetched.status.as_str(), refetched.title.as_str()), ("done", "Drawer unstuck"));

        let bell: i64 = sqlx::query_scalar("SELECT count(*) FROM world_notifications n JOIN world_events e ON e.seq = n.event_seq WHERE e.task_id = $1 AND e.type = 'task.changed'")
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        assert_eq!(bell, 0, "card changes are quiet");

        let rerouted: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id) VALUES ($1, 'Changing hands', 'todo', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let reroute_after: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM world_events").fetch_one(&pool).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET title = 'Changing hands now' WHERE id = $1").bind(rerouted).execute(&mut *tx).await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = NULL WHERE id = $1").bind(rerouted).execute(&mut *tx).await?;
        tx.commit().await?;
        let rerouted_events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND task_id = $2 ORDER BY seq",
        )
        .bind(reroute_after)
        .bind(rerouted)
        .fetch_all(&pool)
        .await?;
        assert_eq!(rerouted_events.len(), 1);
        assert_eq!(rerouted_events[0].agent_id, None);
        assert_eq!(rerouted_events[0].payload, json!({ "task_id": rerouted, "quiet": true }));

        let doomed: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id) VALUES ($1, 'Temporary drawer', 'todo', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let before_delete: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM world_events").fetch_one(&pool).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET title = 'Temporary drawer updated' WHERE id = $1").bind(doomed).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(doomed).execute(&mut *tx).await?;
        tx.commit().await?;
        let deleted = store
            .events_since_in(org, before_delete, 10)
            .await?
            .into_iter()
            .filter(|event| event.payload["task_id"] == json!(doomed))
            .collect::<Vec<_>>();
        assert_eq!(deleted.len(), 1, "an update followed by deletion delivers one invalidation");
        assert_eq!(deleted[0].event_type, "task.deleted");
        assert_eq!(deleted[0].task_id, None);
        assert_eq!(deleted[0].org_id, Some(org));
        assert_eq!(deleted[0].payload, json!({ "task_id": doomed }));
        Ok(())
    }

    #[tokio::test]
    async fn quest_card_events_keep_initial_and_final_viewers_separate() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Quest audience') RETURNING id")
            .bind(format!("quest-audience-{}", &tag[..16]))
            .fetch_one(&pool)
            .await?;
        let other_org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Other audience') RETURNING id")
            .bind(format!("other-audience-{}", &tag[..16]))
            .fetch_one(&pool)
            .await?;
        let person = |label: &'static str| {
            let pool = pool.clone();
            let tag = tag.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email, name, world_role) VALUES ($1, $2, 'write') RETURNING id")
                    .bind(format!("{label}-{tag}@test.dev"))
                    .bind(label)
                    .fetch_one(&pool)
                    .await
            }
        };
        let creator = person("creator").await?;
        let previous = person("previous").await?;
        let final_owner = person("final").await?;
        let transient = person("transient").await?;
        let unrelated = person("unrelated").await?;
        let admin = person("admin").await?;
        let outsider = person("outsider").await?;
        for (member, role) in [
            (creator, "member"),
            (previous, "member"),
            (final_owner, "member"),
            (transient, "member"),
            (unrelated, "member"),
            (admin, "admin"),
        ] {
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, $3)")
                .bind(org)
                .bind(member)
                .bind(role)
                .execute(&pool)
                .await?;
        }
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')")
            .bind(other_org)
            .bind(outsider)
            .execute(&pool)
            .await?;
        let agent = |label: &'static str, owner: Option<Uuid>, visibility: &'static str, organization: Uuid| {
            let pool = pool.clone();
            let tag = tag.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility, org_id)
                     VALUES ($1, $2, 'claude', 'haiku', $3, $4, $5) RETURNING id",
                )
                .bind(format!("{label}-{tag}"))
                .bind(label)
                .bind(owner)
                .bind(visibility)
                .bind(organization)
                .fetch_one(&pool)
                .await
            }
        };
        let previous_agent = agent("previous-agent", Some(previous), "private", org).await?;
        let final_agent = agent("final-agent", Some(final_owner), "private", org).await?;
        let transient_agent = agent("transient-agent", Some(transient), "private", org).await?;
        let ownerless_agent = agent("ownerless-agent", None, "private", org).await?;
        let public_agent = agent("public-agent", Some(previous), "public", org).await?;
        let ownerless_watcher = agent("ownerless-watcher", None, "public", org).await?;
        let previous_watcher = agent("previous-watcher", Some(previous), "public", org).await?;
        let final_watcher = agent("final-watcher", Some(final_owner), "public", org).await?;
        let transient_watcher = agent("transient-watcher", Some(transient), "public", org).await?;
        let creator_watcher = agent("creator-watcher", Some(creator), "public", org).await?;
        let unrelated_watcher = agent("unrelated-watcher", Some(unrelated), "public", org).await?;
        let admin_watcher = agent("admin-watcher", Some(admin), "public", org).await?;
        let outsider_watcher = agent("outsider-watcher", Some(outsider), "public", other_org).await?;
        let watchers = [
            previous_watcher,
            final_watcher,
            transient_watcher,
            creator_watcher,
            unrelated_watcher,
            admin_watcher,
            ownerless_watcher,
            outsider_watcher,
        ];
        for watcher in watchers {
            store.subscribe(watcher, &["task.deleted"]).await?;
        }
        let unrelated_viewer = crate::orgs::Viewer {
            user: unrelated,
            org: Some(org),
            role: crate::access::Role::Write,
            org_role: Some(crate::orgs::OrgRole::Member),
            instance_admin: false,
        };
        let visibility_agent = agent("visibility-agent", Some(previous), "private", org).await?;
        let visibility_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Visibility changes', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(visibility_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let visibility_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        store
            .update_agent(visibility_agent, &AgentUpdate { visibility: Some("public".into()), ..Default::default() })
            .await?;
        let became_public = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(visibility_after)
        .bind(visibility_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!((became_public.task_id, became_public.agent_id, became_public.org_id), (None, Some(visibility_agent), Some(org)));
        for viewer in [creator, previous, unrelated] {
            assert!(!crate::event_scope::private_from_admin(&became_public, viewer, &HashSet::new(), false));
        }
        assert!(!store.hidden_agents(&unrelated_viewer).await?.contains(&visibility_agent));

        let private_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        store
            .update_agent(visibility_agent, &AgentUpdate { visibility: Some("private".into()), ..Default::default() })
            .await?;
        let became_private = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(private_after)
        .bind(visibility_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(became_private.agent_id, Some(visibility_agent));
        for viewer in [creator, previous, unrelated] {
            assert!(!crate::event_scope::private_from_admin(&became_private, viewer, &HashSet::new(), false));
        }
        assert!(store.hidden_agents(&unrelated_viewer).await?.contains(&visibility_agent));

        let restored_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        store
            .update_agent(visibility_agent, &AgentUpdate { visibility: Some("public".into()), ..Default::default() })
            .await?;
        let restored = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(restored_after)
        .bind(visibility_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(restored.agent_id, Some(visibility_agent));
        assert!(!crate::event_scope::private_from_admin(&restored, unrelated, &HashSet::new(), false));
        assert!(!store.hidden_agents(&unrelated_viewer).await?.contains(&visibility_agent));

        store
            .update_agent(visibility_agent, &AgentUpdate { visibility: Some("private".into()), ..Default::default() })
            .await?;
        let transfer_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        assert_eq!(store.transfer_agent(visibility_agent, final_owner).await?, Ok((previous, Vec::new())));
        let transferred = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(transfer_after)
        .bind(visibility_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(transferred.agent_id, Some(visibility_agent));
        for viewer in [creator, previous, final_owner] {
            assert!(!crate::event_scope::private_from_admin(&transferred, viewer, &HashSet::new(), false));
        }
        assert!(crate::event_scope::private_from_admin(&transferred, unrelated, &HashSet::new(), false));

        let reassigned: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Repeated reassignment', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(previous_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let reassign_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = $1").bind(reassigned).bind(transient_agent).execute(&mut *tx).await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = $1").bind(reassigned).bind(final_agent).execute(&mut *tx).await?;
        tx.commit().await?;
        let reassigned_events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(reassign_after)
        .bind(reassigned)
        .fetch_all(&pool)
        .await?;
        assert_eq!(reassigned_events.len(), 1);
        let reassigned_event = &reassigned_events[0];
        assert_eq!((reassigned_event.agent_id, reassigned_event.payload.clone()), (Some(final_agent), json!({ "task_id": reassigned, "quiet": true })));
        for viewer in [creator, previous, final_owner] {
            assert!(!crate::event_scope::private_from_admin(reassigned_event, viewer, &HashSet::new(), false));
        }
        for viewer in [transient, unrelated] {
            assert!(crate::event_scope::private_from_admin(reassigned_event, viewer, &HashSet::new(), false));
        }
        assert!(!crate::event_scope::private_from_admin(reassigned_event, admin, &HashSet::new(), true));

        let doomed: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Reassigned then deleted', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(previous_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let delete_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = $1").bind(doomed).bind(transient_agent).execute(&mut *tx).await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = $1").bind(doomed).bind(final_agent).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(doomed).execute(&mut *tx).await?;
        tx.commit().await?;
        let events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events
             WHERE seq > $1 AND type IN ('task.changed', 'task.deleted') AND payload->>'task_id' = $2::text ORDER BY seq",
        )
        .bind(delete_after)
        .bind(doomed)
        .fetch_all(&pool)
        .await?;
        assert_eq!(events.len(), 1);
        let deleted = &events[0];
        assert_eq!(deleted.event_type, "task.deleted");
        assert_eq!((deleted.agent_id, deleted.payload.clone()), (Some(final_agent), json!({ "task_id": doomed })));
        for (viewer, admin_role, visible) in [
            (creator, false, true),
            (previous, false, true),
            (final_owner, false, true),
            (transient, false, false),
            (unrelated, false, false),
            (admin, true, true),
            (outsider, false, false),
        ] {
            assert_eq!(!crate::event_scope::private_from_admin(deleted, viewer, &HashSet::new(), admin_role), visible);
        }
        let previous_view = crate::event_scope::card_invalidation_for_viewer(deleted.clone(), previous, false, false);
        assert_eq!((previous_view.seq, previous_view.agent_id, previous_view.payload), (deleted.seq, None, json!({ "task_id": doomed, "quiet": true })));
        let final_view = crate::event_scope::card_invalidation_for_viewer(deleted.clone(), final_owner, false, false);
        assert_eq!((final_view.seq, final_view.agent_id, final_view.payload), (deleted.seq, Some(final_agent), json!({ "task_id": doomed })));
        let outsider_allowed: bool = sqlx::query_scalar("SELECT world_quest_event_scope_allows_in($1, $2, $3)")
            .bind(deleted.quest_scope.clone())
            .bind(outsider)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        assert!(!outsider_allowed);
        for (watcher, expected) in [
            (previous_watcher, 0),
            (final_watcher, 1),
            (transient_watcher, 0),
            (creator_watcher, 0),
            (unrelated_watcher, 0),
            (admin_watcher, 0),
            (ownerless_watcher, 0),
            (outsider_watcher, 0),
        ] {
            let delivered: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
            )
            .bind(watcher)
            .bind(doomed)
            .fetch_one(&pool)
            .await?;
            assert_eq!(delivered, expected, "watcher {watcher}");
        }
        assert!(serde_json::to_value(deleted)?.get("quest_scope").is_none());

        let ownerless_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Ownerless private quest', 'done', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(ownerless_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        assert_eq!(crate::db::Database::from_pool(pool.clone()).delete_task(ownerless_task, creator).await?, 1);
        let ownerless_event = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text",
        )
        .bind(ownerless_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!((ownerless_event.agent_id, ownerless_event.payload.clone()), (Some(ownerless_agent), json!({ "task_id": ownerless_task })));
        assert!(!crate::event_scope::private_from_admin(&ownerless_event, creator, &HashSet::new(), false));
        assert!(!crate::event_scope::private_from_admin(&ownerless_event, admin, &HashSet::new(), true));
        assert!(crate::event_scope::private_from_admin(&ownerless_event, unrelated, &HashSet::new(), false));
        let ownerless_deliveries: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = ANY($1) AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(watchers.to_vec())
        .bind(ownerless_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(ownerless_deliveries, 0);

        let removed_private_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Private agent removal', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(previous_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let private_removal_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        assert_eq!(store.delete_agent(previous_agent).await?, 1);
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT assigned_agent_id FROM agent_tasks WHERE id = $1")
                .bind(removed_private_task)
                .fetch_one(&pool)
                .await?,
            None
        );
        let private_removal_events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events
             WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(private_removal_after)
        .bind(removed_private_task)
        .fetch_all(&pool)
        .await?;
        assert_eq!(private_removal_events.len(), 1);
        let private_removal = &private_removal_events[0];
        assert_eq!((private_removal.agent_id, private_removal.payload.clone()), (None, json!({ "task_id": removed_private_task, "quiet": true })));
        for viewer in [creator, previous] {
            assert!(!crate::event_scope::private_from_admin(private_removal, viewer, &HashSet::new(), false));
        }
        assert!(crate::event_scope::private_from_admin(private_removal, unrelated, &HashSet::new(), false));

        let removed_public_agent = agent("removed-public-agent", Some(final_owner), "public", org).await?;
        let removed_public_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Public agent removal', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(removed_public_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let public_removal_after: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM world_events").fetch_one(&pool).await?;
        assert_eq!(crate::db::Database::from_pool(pool.clone()).delete_agent_profile(removed_public_agent).await?, 1);
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT assigned_agent_id FROM agent_tasks WHERE id = $1")
                .bind(removed_public_task)
                .fetch_one(&pool)
                .await?,
            None
        );
        let public_removal_events = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events
             WHERE seq > $1 AND type = 'task.changed' AND payload->>'task_id' = $2::text",
        )
        .bind(public_removal_after)
        .bind(removed_public_task)
        .fetch_all(&pool)
        .await?;
        assert_eq!(public_removal_events.len(), 1);
        let public_removal = &public_removal_events[0];
        assert_eq!((public_removal.agent_id, public_removal.payload.clone()), (None, json!({ "task_id": removed_public_task, "quiet": true })));
        for viewer in [creator, previous, unrelated] {
            assert!(!crate::event_scope::private_from_admin(public_removal, viewer, &HashSet::new(), false));
        }
        assert!(!sqlx::query_scalar::<_, bool>("SELECT world_quest_event_scope_allows_in($1, $2, $3)")
            .bind(&public_removal.quest_scope)
            .bind(outsider)
            .bind(org)
            .fetch_one(&pool)
            .await?);
        assert_eq!(store.delete_agent(Uuid::new_v4()).await?, 0);

        let public_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Public quest before deletion', 'done', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(public_agent)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2").bind(org).bind(previous).execute(&pool).await?;
        assert_eq!(crate::db::Database::from_pool(pool.clone()).delete_task(public_task, creator).await?, 1);
        let public_event = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE type = 'task.deleted' AND payload->>'task_id' = $1::text",
        )
        .bind(public_task)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_profiles SET visibility = 'private' WHERE id = $1").bind(public_agent).execute(&pool).await?;
        let viewer = |user| crate::orgs::Viewer {
            user,
            org: Some(org),
            role: crate::access::Role::Write,
            org_role: Some(crate::orgs::OrgRole::Member),
            instance_admin: false,
        };
        assert!(store.hidden_agents(&viewer(creator)).await?.contains(&public_agent));
        assert!(store.hidden_agents(&viewer(unrelated)).await?.contains(&public_agent));
        assert!(crate::event_scope::creator_can_refresh(&public_event, creator));
        assert!(!crate::event_scope::creator_can_refresh(&public_event, unrelated));
        let removed_owner_delivery: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(previous_watcher)
        .bind(public_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(removed_owner_delivery, 0);

        let chat_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, assignee_user_id, title, status, source, org_id)
             VALUES ($1, $2, 'Chat quest', 'in_progress', 'chat', $3) RETURNING id",
        )
        .bind(creator)
        .bind(final_owner)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        let chat_event = sqlx::query_as::<_, super::WorldEvent>(
            "SELECT * FROM world_events WHERE type = 'task.changed' AND payload->>'task_id' = $1::text",
        )
        .bind(chat_task)
        .fetch_one(&pool)
        .await?;
        assert!(!crate::event_scope::private_from_admin(&chat_event, creator, &HashSet::new(), false));
        assert!(crate::event_scope::private_from_admin(&chat_event, final_owner, &HashSet::new(), false));

        let removed_creator_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, org_id) VALUES ($1, 'Former member quest', 'done', $2) RETURNING id",
        )
        .bind(creator)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2").bind(org).bind(creator).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(removed_creator_task).execute(&pool).await?;
        let removed_creator_delivery: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.deleted' AND payload->>'task_id' = $2::text",
        )
        .bind(creator_watcher)
        .bind(removed_creator_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(removed_creator_delivery, 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_work_runs_fail_their_quest_so_it_can_be_resumed() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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

        let carrying_task = quest("in_progress", agent).await?;
        let carrying_session = Uuid::new_v4();
        insert_session(&pool, carrying_session, agent, "task", "active", Some(carrying_task), None, Some("cli-carry"), None, None).await?;
        let completed = insert_run(&pool, carrying_session, agent, Some(carrying_task), "followup", "completed", None).await?;
        let carrying = insert_run(&pool, carrying_session, agent, Some(carrying_task), "followup", "queued", None).await?;
        let newer = insert_run(&pool, carrying_session, agent, Some(carrying_task), "followup", "queued", None).await?;
        sqlx::query("UPDATE session_runs SET started_at = NOW(), finished_at = clock_timestamp() WHERE id = $1")
            .bind(completed)
            .execute(&pool)
            .await?;
        assert!(store.withdraw_queued(newer, "Ana").await?);
        assert!(store.withdraw_queued(carrying, "Ana").await?);
        let marker: Option<Uuid> = sqlx::query_scalar("SELECT carries_on FROM session_runs WHERE id = $1").bind(carrying).fetch_one(&pool).await?;
        assert_eq!(marker, Some(completed), "withdrawing the older run last leaves its carry marker behind the newer cancelled row");
        expected.push((carrying_task, agent, "older withdrawal still carrying on", "in_progress", false, Uuid::nil()));

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
            let announced: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE task_id = $1 AND type = 'task.changed'")
                .bind(task)
                .fetch_all(&pool)
                .await?;
            assert_eq!(announced.len(), 1 + usize::from(moves), "{name}: a quest that turns failed tells every open card");
            assert!(announced.iter().all(|payload| payload == &serde_json::json!({ "task_id": task, "quiet": true })), "{name}");
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
    async fn a_quest_counts_as_merged_only_with_evidence_for_its_recorded_pr() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let pr = "https://github.com/acme/app/pull/7";
        let cases = [
            ("done quest with no evidence", "done", Some(pr), None, None, false),
            ("done milestone with no evidence", "review", Some(pr), Some("done"), None, false),
            ("matching merge evidence", "review", Some(pr), Some("done"), Some(pr), true),
            ("merge evidence for an old PR", "done", Some(pr), Some("done"), Some("https://github.com/acme/app/pull/6"), false),
            ("done without a pr", "done", None, None, Some(pr), false),
            ("cancelled quest", "cancelled", Some(pr), Some("done"), Some(pr), false),
        ];
        for (name, status, pr_url, milestone, merged_pr, merged) in cases {
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
            if let Some(merged_pr) = merged_pr {
                sqlx::query("UPDATE agent_tasks SET merged_pr_url = $2 WHERE id = $1")
                    .bind(task)
                    .bind(merged_pr)
                    .execute(&pool)
                    .await?;
            }
            let found = store.quest_merged_pr(task).await?;
            assert_eq!(found.is_some(), merged, "{name}");
            if merged {
                assert_eq!(found.as_deref(), Some(pr), "{name}");
            }
        }
        let unassigned: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, pr_url) VALUES ($1, 'unassigned', 'failed', $2) RETURNING id",
        )
        .bind(user)
        .bind(pr)
        .fetch_one(&pool)
        .await?;
        assert!(store.settle_pr(unassigned, pr, true).await?);
        let merge_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.merged'",
        )
        .bind(unassigned)
        .fetch_one(&pool)
        .await?;
        assert_eq!(merge_events, 0);
        sqlx::query("DELETE FROM world_events WHERE task_id = $1")
            .bind(unassigned)
            .execute(&pool)
            .await?;
        assert_eq!(store.quest_merged_pr(unassigned).await?.as_deref(), Some(pr));
        sqlx::query("UPDATE agent_tasks SET status = 'failed', error_message = 'late failure' WHERE id = $1")
            .bind(unassigned)
            .execute(&pool)
            .await?;
        assert!(!store.settle_pr(unassigned, pr, false).await?);
        let recorded: (String, Option<String>) = sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
            .bind(unassigned)
            .fetch_one(&pool)
            .await?;
        assert_eq!(recorded, ("failed".into(), Some("late failure".into())));
        assert_eq!(store.quest_merged_pr(unassigned).await?.as_deref(), Some(pr));
        Ok(())
    }

    #[tokio::test]
    async fn pr_replacement_and_scoped_terminal_updates_are_atomic() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let database = crate::db::Database::from_pool(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, name, world_role) VALUES ($1, 'Atomic', 'write') RETURNING id",
        )
        .bind(format!("atomic-pr-{tag}@test.dev"))
        .fetch_one(&pool)
        .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id)
             VALUES ($1, 'Atomic', 'codex', 'gpt', $2) RETURNING id",
        )
        .bind(format!("atomic-pr-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;

        let old_pr = "https://github.com/Acme/App/pull/41";
        let new_pr = "https://github.com/acme/app/pull/42";
        let replacement_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks
               (user_id, title, status, error_message, assigned_agent_id, repo, branch, pr_url, pr_number)
             VALUES ($1, 'replace', 'failed', 'old error', $2, 'https://github.com/Acme/App.git', 'world/replace', $3, NULL)
             RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(old_pr)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1")
            .bind(replacement_task)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, detail)
             VALUES ($1, 'merge', 'merge', 'failed', 'Closed without merging')",
        )
        .bind(replacement_task)
        .execute(&pool)
        .await?;
        assert!(database.replace_current_pr(replacement_task, old_pr, 41, new_pr, 42).await?);
        let replacement: (String, Option<String>, Option<String>, Option<i32>, Value, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, t.pr_url, t.pr_number, t.past_prs, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(replacement_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!((replacement.0.as_str(), replacement.1, replacement.2.as_deref(), replacement.3, replacement.5.as_str()), ("review", None, Some(new_pr), Some(42), "active"));
        assert_eq!(replacement.4[0]["url"], old_pr);
        assert!(!database.replace_current_pr(replacement_task, old_pr, 41, "https://github.com/acme/app/pull/43", 43).await?);
        sqlx::query("UPDATE agent_tasks SET repo = 'Acme/Other' WHERE id = $1")
            .bind(replacement_task)
            .execute(&pool)
            .await?;
        assert!(!database.replace_current_pr(replacement_task, new_pr, 42, "https://github.com/acme/app/pull/43", 43).await?);

        let merged_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo, branch, pr_url, pr_number)
             VALUES ($1, 'settle follow-up', 'review', $2, 'Acme/App', 'world/settle', $3, 51) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind("https://github.com/acme/app/pull/51")
        .fetch_one(&pool)
        .await?;
        let merged_session = Uuid::new_v4();
        insert_session(&pool, merged_session, agent, "task", "active", Some(merged_task), None, Some("merge-session"), None, None).await?;
        let queued = insert_run(&pool, merged_session, agent, Some(merged_task), "followup", "queued", None).await?;
        let running = insert_run(&pool, merged_session, agent, Some(merged_task), "followup", "running", None).await?;
        let completed = insert_run(&pool, merged_session, agent, Some(merged_task), "followup", "completed", None).await?;
        assert!(store.settle_merged_followup(merged_task, merged_session, "https://github.com/acme/app/pull/51").await?);
        let statuses: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, status FROM session_runs WHERE id = ANY($1) ORDER BY id",
        )
        .bind(vec![queued, running, completed])
        .fetch_all(&pool)
        .await?;
        let status = |id| statuses.iter().find(|row| row.0 == id).map(|row| row.1.as_str());
        assert_eq!((status(queued), status(running), status(completed)), (Some("cancelled"), Some("running"), Some("completed")));
        let settled: (String, String) = sqlx::query_as(
            "SELECT t.status, s.status FROM agent_tasks t JOIN agent_sessions s ON s.task_id = t.id WHERE t.id = $1",
        )
        .bind(merged_task)
        .fetch_one(&pool)
        .await?;
        assert_eq!(settled, ("done".into(), "completed".into()));

        let failed_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id)
             VALUES ($1, 'fail follow-ups', 'in_progress', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let failed_session = Uuid::new_v4();
        insert_session(&pool, failed_session, agent, "task", "active", Some(failed_task), None, Some("fail-session"), None, None).await?;
        let failed_queued = insert_run(&pool, failed_session, agent, Some(failed_task), "followup", "queued", None).await?;
        let failed_running = insert_run(&pool, failed_session, agent, Some(failed_task), "followup", "running", None).await?;
        assert!(store.fail_task_session(failed_task, failed_session, "terminal failure").await?);
        let failure: (String, Option<String>, String, String, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, s.status, q.status, r.status
             FROM agent_tasks t
             JOIN agent_sessions s ON s.id = $2
             JOIN session_runs q ON q.id = $3
             JOIN session_runs r ON r.id = $4
             WHERE t.id = $1",
        )
        .bind(failed_task)
        .bind(failed_session)
        .bind(failed_queued)
        .bind(failed_running)
        .fetch_one(&pool)
        .await?;
        assert_eq!(failure, ("failed".into(), Some("terminal failure".into()), "failed".into(), "failed".into(), "running".into()));
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(failed_task)
            .execute(&pool)
            .await?;
        assert!(!store.fail_task_session(failed_task, failed_session, "late failure").await?);
        Ok(())
    }

    async fn insert_closure_clarification(
        pool: &PgPool,
        agent_id: Uuid,
        task_id: Uuid,
    ) -> anyhow::Result<(Uuid, i64)> {
        let card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'agent', $1, 'Which option?', $2) RETURNING id",
        )
        .bind(agent_id)
        .bind(json!({ "kind": "ask", "clarify_request": { "task_id": task_id, "kind": "task.assigned", "resume": true } }))
        .fetch_one(pool)
        .await?;
        let continuation: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', $2) RETURNING id",
        )
        .bind(agent_id)
        .bind(json!({
            "task_id": task_id,
            "resume": true,
            "clarification_continuation": card,
            "clarification_status": "in_progress",
            "clarification_resume": true,
        }))
        .fetch_one(pool)
        .await?;
        Ok((card, continuation))
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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
        let pool = PgPoolOptions::new().max_connections(test_connections()).connect(&url).await?;
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

    #[tokio::test]
    async fn one_writer_holds_a_quest_folder_on_the_local_worker() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = steer_db().write().await;
        let connections = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(4);
        let pool = PgPoolOptions::new().max_connections(connections).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model) VALUES ($1, 'Wren', 'claude', 'haiku') RETURNING id")
            .bind(format!("wren-{tag}"))
            .fetch_one(&pool)
            .await?;
        let folder = format!("/workspaces/tasks/{tag}/world");
        let dependent = format!("/workspaces/tasks/{tag}-part/world");
        let session = |role: &'static str, dir: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, working_dir, status)
                     VALUES ($1, $2, 'claude', 'haiku', 'cli', $3, 'active') RETURNING id",
                )
                .bind(agent)
                .bind(role)
                .bind(dir)
                .fetch_one(&pool)
                .await
            }
        };
        let queue = |session: Uuid, kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, created_at)
                     VALUES ($1, $2, $3, 'queued', 'work', clock_timestamp()) RETURNING id",
                )
                .bind(session)
                .bind(agent)
                .bind(kind)
                .fetch_one(&pool)
                .await
            }
        };
        let finish = |run: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(run).execute(&pool).await }
        };
        let claimed = || async {
            let mut ids = Vec::new();
            while let Some(run) = store.claim_queued_run("folder-test").await? {
                ids.push(run.id);
            }
            anyhow::Ok(ids)
        };

        let writer = session("task", folder.clone()).await?;
        let resumed = session("task", folder.clone()).await?;
        let review = session("review", folder.clone()).await?;
        let part = session("task", dependent.clone()).await?;

        let first = queue(writer, "task").await?;
        assert!(claimed().await?.contains(&first), "a single quest starts its writer");
        let second = queue(resumed, "followup").await?;
        let reviewing = queue(review, "review").await?;
        let stacked = queue(part, "task").await?;
        let started = claimed().await?;
        assert!(!started.contains(&second), "a second writer waits while another session writes in the same folder");
        assert!(started.contains(&reviewing), "a review only reads, so it runs beside the writer");
        assert!(started.contains(&stacked), "a quest on a dependent branch has its own folder and starts");
        assert_eq!(store.other_folder_writer(&store.get_session(resumed).await?.expect("session")).await?.as_deref().map(|w| w.contains(&first.to_string()[..8])), Some(true));

        let mut listener = sqlx::postgres::PgListener::connect(&url).await?;
        listener.listen("session_runs").await?;
        finish(first).await?;
        let woken = tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv())
            .await
            .map_err(|_| anyhow::anyhow!("writer completion did not wake the local worker"))??;
        assert_eq!(woken.payload(), first.to_string(), "the end of a writer wakes the worker for the one waiting on its folder");
        assert!(claimed().await?.contains(&second), "the waiting writer starts once the folder is free");

        let writer_session = store.get_session(writer).await?.expect("session");
        let other = store.other_folder_writer(&writer_session).await?.expect("the newer writer is another session");
        assert!(other.contains(&second.to_string()[..8]) && other.contains("follow-up"), "{other}");
        assert!(store.other_folder_writer(&store.get_session(resumed).await?.expect("session")).await?.is_none());
        finish(second).await?;
        finish(reviewing).await?;

        let followup = queue(writer, "followup").await?;
        let review_again = queue(review, "review").await?;
        let both = claimed().await?;
        assert!(both.contains(&review_again) && both.contains(&followup), "a follow-up starts while a review runs");
        finish(followup).await?;
        finish(review_again).await?;

        let stale = queue(resumed, "followup").await?;
        let running = queue(writer, "followup").await?;
        sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1").bind(running).execute(&pool).await?;
        assert!(store.withdraw_queued(stale, "Ana").await?, "a queued run can be withdrawn");
        assert!(!store.withdraw_queued(running, "Ana").await?, "withdrawing never touches a running run");
        assert!(!store.withdraw_queued(stale, "Ana").await?, "a withdrawn run stays withdrawn");
        let (status, error): (String, Option<String>) = sqlx::query_as("SELECT status, error FROM session_runs WHERE id = $1").bind(stale).fetch_one(&pool).await?;
        assert_eq!(status, "cancelled");
        assert!(error.is_some_and(|e| e.contains("Withdrawn by Ana")));
        finish(running).await?;
        assert!(!claimed().await?.contains(&stale), "a withdrawn run never starts");

        let behind = queue(resumed, "followup").await?;
        let publishing = queue(writer, "publish").await?;
        sqlx::query("UPDATE session_runs SET not_before = NOW() + INTERVAL '1 hour' WHERE id = $1").bind(publishing).execute(&pool).await?;
        assert!(!claimed().await?.contains(&behind), "finished work waiting to publish keeps the folder");
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1").bind(publishing).execute(&pool).await?;
        let checking: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir, status, parent_session_id)
             VALUES ($1, 'review', 'claude', 'haiku', $2, 'active', $3) RETURNING id",
        )
        .bind(agent)
        .bind(&folder)
        .bind(writer)
        .fetch_one(&pool)
        .await?;
        let checks = queue(checking, "review").await?;
        sqlx::query("UPDATE session_runs SET not_before = NOW() + INTERVAL '1 hour' WHERE id = $1").bind(checks).execute(&pool).await?;
        let own = queue(writer, "followup").await?;
        let during_review = claimed().await?;
        assert!(!during_review.contains(&behind), "finished work under its automatic review keeps the folder");
        assert!(during_review.contains(&own), "the reviewed session's own follow-up still starts during its review");
        finish(own).await?;
        sqlx::query("UPDATE agent_sessions SET asked_by = '{\"manual\": true}'::jsonb WHERE id = $1").bind(checking).execute(&pool).await?;
        assert!(claimed().await?.contains(&behind), "a review someone asked for by hand never holds back other work");
        finish(behind).await?;
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1").bind(checks).execute(&pool).await?;

        let main = session("main", folder.clone()).await?;
        assert!(store.hold_folder(&store.get_session(main).await?.expect("session")).await?.is_none(), "only quest folders are held");
        let hold = store.hold_folder(&writer_session).await?.expect("a task folder can be held");
        let handed = queue(resumed, "followup").await?;
        assert!(!claimed().await?.contains(&handed), "a writer waits while the folder is held for a handoff");
        hold.release().await?;
        assert!(claimed().await?.contains(&handed), "it starts once the handoff ends");
        finish(handed).await?;

        let answered = queue(writer, "followup").await?;
        let superseded = queue(writer, "followup").await?;
        let kept = queue(writer, "followup").await?;
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = clock_timestamp() WHERE id = $1").bind(answered).execute(&pool).await?;
        let carries_on = |run: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_scalar::<_, Option<Uuid>>("SELECT carries_on FROM session_runs WHERE id = $1").bind(run).fetch_one(&pool).await }
        };
        assert!(store.withdraw_queued(superseded, "Ana").await?);
        assert_eq!(carries_on(superseded).await?, None, "another queued follow-up still carries the work on");
        assert!(store.withdraw_queued(kept, "Ana").await?);
        assert_eq!(carries_on(kept).await?, Some(answered), "withdrawing the last follow-up hands the finished one back to be carried on");
        assert!(store.carry_on_candidates(Some(kept), true).await?.is_empty(), "a device only takes work from sessions on a device");
        let carry = store.claim_carry_on(kept, false).await?.expect("the local worker claims the handoff");
        assert_eq!(carry.prior(), answered);
        carry.release().await?;
        assert_eq!(carries_on(kept).await?, Some(answered), "an unfinished handoff stays available for retry");
        let carry = store.claim_carry_on(kept, false).await?.expect("the local worker retries the handoff");
        carry.complete().await?;
        assert!(store.carry_on_candidates(Some(kept), false).await?.is_empty(), "successful work clears the handoff");

        let concurrent_answer = queue(writer, "followup").await?;
        let concurrent_first = queue(writer, "followup").await?;
        let concurrent_second = queue(writer, "followup").await?;
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = clock_timestamp() WHERE id = $1")
            .bind(concurrent_answer)
            .execute(&pool)
            .await?;
        let (first, second) = tokio::join!(
            store.withdraw_queued(concurrent_first, "Ana"),
            store.withdraw_queued(concurrent_second, "Ana"),
        );
        assert!(first? && second?);
        let marked = [carries_on(concurrent_first).await?, carries_on(concurrent_second).await?];
        assert_eq!(marked.iter().filter(|prior| **prior == Some(concurrent_answer)).count(), 1, "the final concurrent withdrawal carries the completed work on");
        let waiting_for_carry = queue(resumed, "followup").await?;
        assert!(!claimed().await?.contains(&waiting_for_carry), "another session waits while the completed follow-up is carried on");
        let marked_run = if marked[0].is_some() { concurrent_first } else { concurrent_second };
        let mut carry_listener = sqlx::postgres::PgListener::connect(&url).await?;
        carry_listener.listen("session_runs").await?;
        store.claim_carry_on(marked_run, false).await?.expect("the handoff is claimable").complete().await?;
        let wake = tokio::time::timeout(std::time::Duration::from_secs(5), carry_listener.recv())
            .await
            .map_err(|_| anyhow::anyhow!("clearing the carry marker did not wake the local worker"))??;
        assert_eq!(wake.payload(), marked_run.to_string(), "clearing the handoff wakes the next writer");
        assert!(claimed().await?.contains(&waiting_for_carry), "the next session starts after the handoff succeeds");
        finish(waiting_for_carry).await?;
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
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(super::tests::test_connections()).connect(&url).await?;
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
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(super::tests::test_connections()).connect(&url).await?;
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
            quest_scope: None,
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
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(super::tests::test_connections()).connect(&url).await?;
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
