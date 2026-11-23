use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::run_recording::{bill_session, emit_waiting, requeue_for_space, write_session_after_run, Reported};
use crate::world::{emit_in, NewEvent};

pub const CHANNEL_RUNNER_JOBS: &str = "runner_jobs";
pub const RUNNER_LEASE_SECONDS: i64 = 60;
pub const RUNNER_SLEEP_GRACE_SECONDS: i64 = 3 * 3600;
pub const RUNNER_OUTAGE_GRACE_SECONDS: i64 = 3600;
pub const RUNNER_ASLEEP_ERROR: &str = "runner lease expired: the device was asleep or offline for over 3 hours";
pub const WORK_RUN_KINDS: [&str; 4] = ["task", "followup", "review", "publish"];
pub const RUNNER_UNREACHABLE_ERROR: &str = "runner lease expired: the runner could not reach Loka for over an hour";
const AWAY_GRACE: &str = "make_interval(secs => CASE WHEN w.platform = 'macos' THEN $1::double precision ELSE $2::double precision END)";
const RUNNER_AWAY: &str = "(l.expires_at <= NOW() AND (w.last_seen_at IS NULL OR w.last_seen_at < l.expires_at)
                 AND l.expires_at > NOW() - __GRACE__)";

pub fn away_grace_secs(platform: &str) -> i64 {
    if platform == "macos" {
        RUNNER_SLEEP_GRACE_SECONDS
    } else {
        RUNNER_OUTAGE_GRACE_SECONDS
    }
}

fn runner_away() -> String {
    RUNNER_AWAY.replace("__GRACE__", AWAY_GRACE)
}
pub const WORKSPACE_SETTLE_SECONDS: f64 = 3600.0;
pub const WORKSPACE_DONE_GRACE_SECONDS: f64 = 3.0 * 86400.0;
pub const WORKSPACE_FAILED_GRACE_SECONDS: f64 = 7.0 * 86400.0;
pub const WORKSPACE_IDLE_BUILD_SECONDS: f64 = 2.0 * 3600.0;
pub const FOLDER_SESSION_IDLE_SECONDS: f64 = 3.0 * 86400.0;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ClosedTask {
    pub id: Uuid,
    pub status: String,
    pub idle_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskFolder {
    pub name: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskNotice {
    pub free_bytes: u64,
    pub freed_bytes: u64,
    pub removed: usize,
    pub still_low: bool,
    #[serde(default)]
    pub folders: Vec<DiskFolder>,
    #[serde(default)]
    pub caches_bytes: u64,
    #[serde(default)]
    pub held_bytes: u64,
    #[serde(default)]
    pub kept: Vec<DiskFolder>,
    #[serde(default)]
    pub unowned: Vec<DiskFolder>,
    #[serde(default)]
    pub outside_bytes: u64,
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn folder_names(folders: &[DiskFolder]) -> String {
    folders.iter().map(|folder| format!("{} ({})", folder.name, gb(folder.bytes))).collect::<Vec<_>>().join(", ")
}

impl DiskNotice {
    pub fn cleared(&self) -> String {
        let caches = if self.caches_bytes > 0 { format!(" Loka cleared build output and test scratch of quests that were not running ({}).", gb(self.caches_bytes)) } else { String::new() };
        if self.removed > 0 {
            format!("{caches} Loka removed {} closed quest folder{} ({}).", self.removed, if self.removed == 1 { "" } else { "s" }, gb(self.freed_bytes.saturating_sub(self.caches_bytes)))
        } else {
            caches
        }
    }

    pub fn left(&self) -> String {
        let mut left = if self.removed == 0 && self.caches_bytes == 0 { " There was nothing Loka could safely clear on its own.".to_string() } else { String::new() };
        if self.held_bytes > 0 {
            left.push_str(&format!(" Another {} is build output of quests running now, which Loka clears once they finish.", gb(self.held_bytes)));
        }
        if !self.kept.is_empty() {
            left.push_str(&format!(" A program you started still uses the build folders of {}, so Loka left them alone: quit it to free that space.", folder_names(&self.kept)));
        }
        if !self.unowned.is_empty() {
            left.push_str(&format!(" These folders belong to no quest on this device, so delete them if nothing else uses them: {}.", folder_names(&self.unowned)));
        }
        if !self.folders.is_empty() {
            left.push_str(&format!(" Biggest folders: {}.", folder_names(&self.folders)));
        }
        if self.outside_bytes > 0 {
            left.push_str(&format!(" Loka has nothing more it can clear, so free up at least {} outside Loka, for example in Downloads, the Trash or other apps.", gb(self.outside_bytes)));
        }
        left
    }

    pub fn message(&self, device: &str) -> String {
        if self.still_low {
            format!("{device} has only {} of disk space left, so new quest runs there wait and start by themselves once there is room.{}{}", gb(self.free_bytes), self.cleared(), self.left())
        } else {
            format!("{device} was running low on disk space ({} free now).{}", gb(self.free_bytes), self.cleared())
        }
    }
}

const PREFIXES_PER_LOOKUP: usize = 500;

pub const RUNNER_MOVED_ERROR: &str = "run stopped: the agent was moved to another device while it ran";
pub const WORLD_WORKER_APP: &str = "loka-world";
const LEGACY_WORLD_WORKER_APP: &str = "karna-world";

pub fn serves_sql(agent: &str, runner: &str) -> String {
    format!("({agent}.owner_user_id = {runner}.owner_user_id)")
}

const FRESH_AGENT_SQL: &str = "NOT EXISTS (
                 SELECT 1 FROM agent_sessions s
                 WHERE s.agent_id = a.id AND (s.cli_session_id IS NOT NULL
                   OR EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id)))";

fn own_device_sql(agent: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM world_runners own
                  WHERE own.owner_user_id = {agent}.owner_user_id AND own.status <> 'revoked'
                    AND COALESCE((own.capabilities->'backends'->>{agent}.cli)::boolean, false))"
    )
}

fn live_runner_sql(runner: &str) -> String {
    format!("{runner}.status = 'online' AND {runner}.last_seen_at > NOW() - INTERVAL '75 seconds'")
}

const RUNS_HERE: &str = "COALESCE(a.runner_id = l.runner_id OR (r.kind = 'direct' AND r.runner_id = l.runner_id AND a.owner_user_id = w.owner_user_id), false)";

fn serving(sql: &str) -> String {
    sql.replace("__SERVES__", &serves_sql("a", "w"))
        .replace("__HOME__", RUNS_HERE)
        .replace("__URGENT__", &crate::world::urgent_run_sql("q"))
        .replace("__FOLDER_FREE_Q__", &crate::world::folder_free_sql("q", "s"))
        .replace("__FOLDER_FREE_R__", &crate::world::folder_free_sql("r", "s"))
}

pub const CLOUD_OFFLINE: &str = "No world server is running, so the cloud cannot run agents right now. Pick one of the devices instead.";

pub fn is_mac(runner: &RunnerView) -> bool {
    runner.platform == "macos" || runner.capabilities.get("platform").and_then(Value::as_str) == Some("macos")
}

pub fn shared_folders(capabilities: &Value) -> Vec<(Uuid, String)> {
    capabilities
        .get("folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f.get("id")?.as_str()?.parse().ok()?, f.get("name")?.as_str()?.to_string())))
        .collect()
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RunnerView {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub status: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub owner_user_id: Uuid,
    pub capabilities: Value,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub shared: bool,
    #[serde(default)]
    #[sqlx(default)]
    pub screen_access: bool,
    #[serde(default)]
    #[sqlx(default)]
    pub app_control: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRegistration {
    pub runner: RunnerView,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunner {
    pub name: String,
    #[serde(default = "default_platform")]
    pub platform: String,
}

pub const ONE_RUNNER: &str = "Loka runs on one Mac. Multi-device support is part of Loka Pro.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneRunner;

impl std::fmt::Display for OneRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(ONE_RUNNER)
    }
}

impl std::error::Error for OneRunner {}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRunner {
    pub name: Option<String>,
    pub paused: Option<bool>,
    #[serde(default)]
    pub screen_access: Option<bool>,
    #[serde(default)]
    pub app_control: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignRunner {
    pub runner_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandoffRunner {
    pub runner_id: Uuid,
    pub cli: String,
    pub model: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMove {
    pub runner_id: Option<Uuid>,
    pub cli: String,
    pub model: String,
    pub summary: String,
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerHeartbeat {
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub leases: Vec<Uuid>,
    #[serde(default)]
    pub disk_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPoll {
    #[serde(default)]
    pub heartbeat: Option<RunnerHeartbeat>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub views: bool,
    #[serde(default)]
    pub pr_checks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPrCheck {
    pub task_id: Uuid,
    pub repo: String,
    pub pr_url: String,
    pub pr_number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPrCheckResult {
    pub task_id: Uuid,
    pub repo: String,
    pub pr_url: String,
    pub pr_number: i64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunnerPolled {
    pub status: String,
    #[serde(default)]
    pub job: Option<RunnerJob>,
    #[serde(default)]
    pub shell: Option<crate::shell::ShellJob>,
    #[serde(default)]
    pub terminal: Option<crate::terminal::TerminalJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_check: Option<RunnerPrCheck>,
    #[serde(default)]
    pub retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_feed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean_workspaces: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean_caches: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerWorkspace {
    pub key: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub create: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerSkill {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerImage {
    pub id: Uuid,
    pub name: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerFile {
    pub id: Uuid,
    pub name: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPublish {
    pub repo: String,
    pub branch: String,
    #[serde(default)]
    pub recovery_branch: Option<String>,
    pub base_branch: String,
    pub commit_message: String,
    pub pr_title: String,
    pub pr_body: String,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub pr_number: Option<i64>,
    #[serde(default)]
    pub reported_pr_number: Option<i64>,
    #[serde(default)]
    pub stack_onto_pr: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_writer: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub context_tokens: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub const MAX_REPORTED_TOKENS: u64 = 100_000_000_000;
pub const MAX_REPORTED_COST_USD: f64 = 100_000.0;

pub fn plausible_pr_url(url: &str) -> bool {
    url.len() <= 512
        && url.starts_with("https://")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\'))
        && url.rsplit_once("/pull/").is_some_and(|(_, number)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

pub fn pr_url_matches(url: &str, repo: &str, number: i64) -> bool {
    crate::git_names::TrackedPr::parse(url).is_some_and(|tracked| {
        i64::from(tracked.number()) == number && tracked.repo().eq_ignore_ascii_case(repo)
    })
}

pub fn plausible_cost(cost_usd: f64) -> bool {
    cost_usd.is_finite() && (0.0..=MAX_REPORTED_COST_USD).contains(&cost_usd)
}

impl RunnerUsage {
    pub fn plausible(&self) -> bool {
        [self.input_tokens, self.cached_input_tokens, self.output_tokens, self.context_tokens]
            .iter()
            .all(|tokens| *tokens <= MAX_REPORTED_TOKENS)
    }

    pub fn since(&self, baseline: &Value) -> RunnerUsage {
        let before = |key: &str| baseline.get(key).and_then(Value::as_u64).unwrap_or(0);
        let previous = RunnerUsage {
            input_tokens: before("input_tokens"),
            cached_input_tokens: before("cached_input_tokens"),
            output_tokens: before("output_tokens"),
            context_tokens: 0,
        };
        let lower = self.input_tokens < previous.input_tokens
            || self.cached_input_tokens < previous.cached_input_tokens
            || self.output_tokens < previous.output_tokens;
        let all = |u: &RunnerUsage| u.input_tokens.saturating_add(u.cached_input_tokens).saturating_add(u.output_tokens);
        if lower && all(self).saturating_mul(2) < all(&previous) {
            return *self;
        }
        RunnerUsage {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            cached_input_tokens: self.cached_input_tokens.saturating_sub(previous.cached_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            context_tokens: self.context_tokens,
        }
    }

    pub fn record(&self, baseline: &mut Value) {
        if !baseline.is_object() {
            *baseline = serde_json::json!({});
        }
        if let Some(object) = baseline.as_object_mut() {
            object.insert("input_tokens".into(), self.input_tokens.into());
            object.insert("cached_input_tokens".into(), self.cached_input_tokens.into());
            object.insert("output_tokens".into(), self.output_tokens.into());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerJob {
    pub lease_id: Uuid,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub runner_id: Uuid,
    pub kind: String,
    pub cli: String,
    pub model: String,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: RunnerWorkspace,
    pub cli_session_id: Option<String>,
    pub resume: bool,
    pub fork: bool,
    pub lean: bool,
    pub max_turns: u32,
    pub timeout_secs: u64,
    pub effort: Option<String>,
    pub fast: bool,
    pub mcp_token: String,
    #[serde(default)]
    pub previous_run_id: Option<Uuid>,
    #[serde(default)]
    pub mcp_servers: Value,
    #[serde(default)]
    pub skills: Vec<RunnerSkill>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vault_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub provider_keys: BTreeMap<String, String>,
    #[serde(default)]
    pub images: Vec<RunnerImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RunnerFile>,
    pub publish: Option<RunnerPublish>,
    #[serde(default)]
    pub rewind_to: Option<String>,
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub live_sessions: Option<Vec<String>>,
    #[serde(default)]
    pub world_tools: bool,
    #[serde(default)]
    pub room_answer: bool,
    pub omp_models: Option<String>,
    #[serde(default)]
    pub omp_secrets: Vec<String>,
    #[serde(default)]
    pub command_rules: Option<crate::command_rules::Policy>,
    #[serde(default)]
    pub approved_actions: Vec<String>,
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    #[serde(default)]
    pub keys_version: Option<String>,
    #[serde(default)]
    pub metered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_scan: Option<crate::setup_scan::ScanRequest>,
    #[serde(default)]
    pub context_sent: crate::context::SentContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_action: Option<crate::pr_actions::ActionJob>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compact_first: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerCompletion {
    pub output: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
    pub exit_code: i32,
    #[serde(default)]
    pub usage: RunnerUsage,
    pub error: Option<String>,
    pub workspace_key: String,
    pub pr_url: Option<String>,
    pub pr_number: Option<i64>,
    #[serde(default)]
    pub publish_pr_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_pr_checked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steered: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_sent: Option<crate::context::SentContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_head: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseBeat {
    #[serde(default)]
    pub steer: bool,
    #[serde(default)]
    pub delivered: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseSteers {
    #[serde(default)]
    pub steers: Vec<crate::world::PendingSteer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps: Option<bool>,
}

pub const USAGE_EVENT: &str = "usage";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrSync {
    Merged,
    Unchanged,
    Changed,
}

impl PrSync {
    pub fn report(self) -> Option<&'static str> {
        match self {
            Self::Merged => Some("merged"),
            Self::Unchanged => Some("unchanged"),
            Self::Changed => None,
        }
    }

    pub fn from_report(state: Option<&str>) -> Self {
        match state {
            Some("merged") => Self::Merged,
            Some("unchanged") => Self::Unchanged,
            _ => Self::Changed,
        }
    }

    pub fn needs_review(self) -> bool {
        self == Self::Changed
    }
}

pub fn pr_sync_from(state: Option<&str>, dirty: bool, new_commits: bool, head_in_pr: bool) -> PrSync {
    match state {
        Some("MERGED") if !dirty && head_in_pr => PrSync::Merged,
        Some("MERGED") => PrSync::Changed,
        _ if dirty || new_commits => PrSync::Changed,
        _ => PrSync::Unchanged,
    }
}

pub const PUBLISH_ELSEWHERE: &str = "elsewhere";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerEvent {
    pub kind: String,
    pub tool: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerEvents {
    pub events: Vec<RunnerEvent>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ClaimedRun {
    pub lease_id: Uuid,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub runner_id: Uuid,
    pub task_id: Option<Uuid>,
    pub kind: String,
    pub prompt: String,
    pub timeout_secs: i32,
    pub backend: String,
    pub model: String,
    pub cli_session_id: Option<String>,
    pub fork_from: Option<Uuid>,
    pub role: String,
    pub task_repo: Option<String>,
    pub task_branch: Option<String>,
    pub task_base_branch: Option<String>,
    pub agent_name: String,
    pub agent_slug: String,
    pub agent_persona: Option<String>,
    pub agent_addendum: Option<String>,
    pub owner_user_id: Uuid,
}

#[derive(Debug, Clone, FromRow)]
pub struct LeaseContext {
    pub lease_id: Uuid,
    pub run_id: Uuid,
    pub runner_id: Uuid,
    pub agent_id: Uuid,
    pub session_id: Uuid,
    pub owner_user_id: Uuid,
    pub kind: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct LeaseState {
    pub lease_status: String,
    pub run_status: String,
    pub kind: String,
    pub cause: Option<String>,
    pub stop_requested_by: Option<String>,
    pub error: Option<String>,
    pub settled_secs_ago: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct RenewedLease {
    #[sqlx(flatten)]
    pub lease: LeaseContext,
    pub timeout_secs: i32,
    pub session_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignRunnerError {
    AgentNotFound,
    RunnerNotFound,
    ActiveHistory,
}

#[derive(Debug)]
pub enum HandoffRunnerError {
    AgentNotFound,
    RunnerNotFound,
    Busy,
    InvalidRequest(&'static str),
    Internal(anyhow::Error),
}

impl From<sqlx::Error> for HandoffRunnerError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeRunner {
    Revoked(Vec<Uuid>),
    NotFound,
    InUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginCompletion {
    Begun,
    AlreadyCompleted,
    Busy,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyCompletion {
    Applied,
    AlreadyApplied,
    Stopped,
}

#[derive(Clone)]
pub struct RunnerStore {
    pool: PgPool,
}

impl RunnerStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn claim_pr_check(&self, runner_id: Uuid) -> Result<Option<RunnerPrCheck>> {
        Ok(sqlx::query_as::<_, (Uuid, String, DateTime<Utc>)>(
            "WITH due AS (
               SELECT m.id, t.pr_url
               FROM task_milestones m
               JOIN agent_tasks t ON t.id = m.task_id
               JOIN agent_profiles a ON a.id = t.assigned_agent_id
               JOIN world_runners w ON w.id = $1
               WHERE m.kind = 'merge' AND m.status = 'active'
                 AND COALESCE(
                       CASE WHEN jsonb_typeof(m.data->'runner_next_check_at') = 'number'
                            THEN (m.data->>'runner_next_check_at')::numeric END,
                       0
                     ) <= EXTRACT(EPOCH FROM NOW())
                 AND t.pr_url IS NOT NULL
                 AND t.status <> 'cancelled' AND w.status = 'online'
                 AND a.owner_user_id = w.owner_user_id
                 AND a.runner_id = w.id AND a.paused_reason IS NULL
               ORDER BY CASE WHEN jsonb_typeof(m.data->'runner_next_check_at') = 'number'
                             THEN (m.data->>'runner_next_check_at')::numeric END NULLS FIRST
               FOR UPDATE OF m SKIP LOCKED
               LIMIT 1
             ), bumped AS (
               UPDATE task_milestones m
               SET data = (CASE WHEN jsonb_typeof(m.data) = 'object' THEN m.data ELSE '{}'::jsonb END)
                     || jsonb_build_object(
                       'runner_next_check_at', EXTRACT(EPOCH FROM NOW() + make_interval(secs => $2)),
                       'runner_pr_claimed_at', EXTRACT(EPOCH FROM NOW()),
                       'runner_pr_claimed_url', due.pr_url
                     ),
                   updated_at = NOW()
               FROM due WHERE m.id = due.id
               RETURNING m.task_id
             )
             SELECT t.id, t.pr_url, NOW()
             FROM bumped b JOIN agent_tasks t ON t.id = b.task_id",
        )
        .bind(runner_id)
        .bind(crate::milestones::check_every_secs() as f64)
        .fetch_optional(&self.pool)
        .await?
        .and_then(|(task_id, pr_url, observed_at)| {
            let tracked = crate::git_names::TrackedPr::parse(&pr_url)?;
            let repo = tracked.repo().to_string();
            let pr_number = i64::from(tracked.number());
            Some(RunnerPrCheck {
                task_id,
                repo,
                pr_url,
                pr_number,
                observed_at: Some(observed_at),
            })
        }))
    }

    pub async fn register(
        &self,
        owner: Uuid,
        request: &CreateRunner,
    ) -> Result<RunnerRegistration> {
        let name = request.name.trim();
        let platform = request.platform.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(anyhow!("runner name must be between 1 and 80 characters"));
        }
        if platform.is_empty() || platform.chars().count() > 40 {
            return Err(anyhow!(
                "runner platform must be between 1 and 40 characters"
            ));
        }
        let token = random_token()?;
        let hash = token_hash(&token);
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('world_runner_register'), hashtext($1::text))")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        let paired: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM world_runners WHERE owner_user_id = $1 AND status <> 'revoked')")
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
        if paired {
            return Err(anyhow!(OneRunner));
        }
        let runner = sqlx::query_as::<_, RunnerView>(
            "INSERT INTO world_runners (owner_user_id, name, platform, token_hash)
             VALUES ($1, $2, $3, $4)
             RETURNING id, name, platform, status, last_seen_at, owner_user_id, capabilities, created_at, shared",
        )
        .bind(owner)
        .bind(name)
        .bind(platform)
        .bind(hash)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RunnerRegistration { runner, token })
    }

    pub async fn list(&self, owner: Uuid) -> Result<Vec<RunnerView>> {
        Ok(sqlx::query_as::<_, RunnerView>(
            "SELECT id, name, platform,
               CASE WHEN status = 'online' AND last_seen_at < NOW() - INTERVAL '75 seconds' THEN 'offline' ELSE status END AS status,
               last_seen_at, owner_user_id, capabilities, created_at, shared, screen_access, app_control
             FROM world_runners WHERE owner_user_id = $1 AND status <> 'revoked'
             ORDER BY created_at",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn device_access(&self, id: Uuid) -> Result<(bool, bool)> {
        Ok(sqlx::query_as::<_, (bool, bool)>("SELECT screen_access, app_control FROM world_runners WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or((false, false)))
    }

    pub async fn get_live(&self, id: Uuid) -> Result<Option<RunnerView>> {
        Ok(sqlx::query_as::<_, RunnerView>(
            "SELECT id, name, platform,
               CASE WHEN status = 'online' AND last_seen_at < NOW() - INTERVAL '75 seconds' THEN 'offline' ELSE status END AS status,
               last_seen_at, owner_user_id, capabilities, created_at, shared, screen_access, app_control
             FROM world_runners WHERE id = $1 AND status <> 'revoked'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn update(
        &self,
        owner: Uuid,
        id: Uuid,
        request: &UpdateRunner,
    ) -> Result<(Option<RunnerView>, Vec<Uuid>)> {
        let name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if request.name.is_some() && name.is_none() {
            return Err(anyhow!("runner name cannot be empty"));
        }
        if name.is_some_and(|name| name.chars().count() > 80) {
            return Err(anyhow!("runner name cannot exceed 80 characters"));
        }
        let status = request
            .paused
            .map(|paused| if paused { "paused" } else { "offline" });
        let mut tx = self.pool.begin().await?;
        let runner = sqlx::query_as::<_, RunnerView>(
            "UPDATE world_runners SET name = COALESCE($3, name), status = COALESCE($4, status),
               screen_access = COALESCE($5, screen_access), app_control = COALESCE($6, app_control), updated_at = NOW()
             WHERE id = $1 AND owner_user_id = $2 AND status <> 'revoked'
             RETURNING id, name, platform, status, last_seen_at, owner_user_id, capabilities, created_at, shared, screen_access, app_control",
        )
        .bind(id)
        .bind(owner)
        .bind(name)
        .bind(status)
        .bind(request.screen_access)
        .bind(request.app_control)
        .fetch_optional(&mut *tx)
        .await?;
        let mut stopped = Vec::new();
        if runner.is_some() && request.paused == Some(true) {
            stopped = sqlx::query_scalar::<_, Uuid>(
                "UPDATE runner_leases l SET status = 'cancelled', finished_at = NOW()
                 FROM session_runs r
                 WHERE l.runner_id = $1 AND l.status IN ('active', 'finishing')
                   AND r.id = l.run_id AND r.status = 'running'
                 RETURNING l.run_id",
            )
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
            if !stopped.is_empty() {
                sqlx::query(
                    "UPDATE session_runs SET status = 'failed', error = 'runner was paused', finished_at = NOW()
                     WHERE id = ANY($1) AND status = 'running'",
                )
                .bind(&stopped)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE agent_inbox SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END,
                       error = 'runner was paused'
                     WHERE id IN (
                       SELECT x.inbox_id FROM runner_run_inbox x
                       JOIN session_runs r ON r.id = x.run_id
                       WHERE x.run_id = ANY($1) AND r.status = 'failed' AND r.error = 'runner was paused')",
                )
                .bind(&stopped)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok((runner, stopped))
    }

    pub async fn revoke(&self, owner: Uuid, id: Uuid) -> Result<RevokeRunner> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM world_runners
             WHERE id = $1 AND owner_user_id = $2 AND status <> 'revoked' FOR UPDATE",
        )
        .bind(id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Ok(RevokeRunner::NotFound);
        }
        let in_use = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM agent_profiles WHERE runner_id = $1)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if in_use {
            return Ok(RevokeRunner::InUse);
        }
        sqlx::query(
            "UPDATE world_runners SET status = 'revoked', revoked_at = NOW(), updated_at = NOW()
             WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let stopped = sqlx::query_scalar::<_, Uuid>(
            "UPDATE runner_leases l SET status = 'cancelled', finished_at = NOW()
             FROM session_runs r
             WHERE l.runner_id = $1 AND l.status IN ('active', 'finishing')
               AND r.id = l.run_id AND r.status = 'running'
             RETURNING l.run_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE session_runs SET status = 'failed', error = 'runner was revoked', finished_at = NOW()
             WHERE runner_id = $1 AND status = 'running'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RevokeRunner::Revoked(stopped))
    }

    pub async fn assign_agent(
        &self,
        owner: Uuid,
        agent_id: Uuid,
        runner_id: Option<Uuid>,
    ) -> std::result::Result<(), AssignRunnerError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| AssignRunnerError::AgentNotFound)?;
        if let Some(runner_id) = runner_id {
            let exists = sqlx::query_scalar::<_, Uuid>(
                "SELECT w.id FROM world_runners w JOIN agent_profiles a ON a.id = $3
                 WHERE w.id = $1 AND w.status <> 'revoked'
                   AND w.owner_user_id = $2 FOR SHARE OF w",
            )
            .bind(runner_id)
            .bind(owner)
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AssignRunnerError::RunnerNotFound)?;
            if exists.is_none() {
                return Err(AssignRunnerError::RunnerNotFound);
            }
        }
        let agent = sqlx::query_as::<_, (Option<Uuid>,)>(
            "SELECT runner_id FROM agent_profiles WHERE id = $1 AND owner_user_id = $2 FOR UPDATE",
        )
        .bind(agent_id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AssignRunnerError::AgentNotFound)?
        .ok_or(AssignRunnerError::AgentNotFound)?;
        if agent.0 == runner_id {
            return Ok(());
        }
        let has_history = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_sessions s
               WHERE s.agent_id = $1 AND (s.cli_session_id IS NOT NULL OR s.status = 'active'
                 OR EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id))
             )",
        )
        .bind(agent_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AssignRunnerError::ActiveHistory)?;
        if has_history {
            return Err(AssignRunnerError::ActiveHistory);
        }
        sqlx::query("UPDATE agent_profiles SET runner_id = $2, updated_at = NOW() WHERE id = $1")
            .bind(agent_id)
            .bind(runner_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AssignRunnerError::AgentNotFound)?;
        tx.commit()
            .await
            .map_err(|_| AssignRunnerError::AgentNotFound)?;
        Ok(())
    }

    pub async fn handoff_agent(
        &self,
        owner: Uuid,
        agent_id: Uuid,
        request: &HandoffRunner,
    ) -> std::result::Result<(), HandoffRunnerError> {
        let target = AgentMove {
            runner_id: Some(request.runner_id),
            cli: request.cli.clone(),
            model: request.model.clone(),
            summary: request.summary.clone(),
            by: None,
        };
        self.move_agent(owner, agent_id, &target, false).await.map(|_| ())
    }

    pub async fn queue_move(&self, agent_id: Uuid, target: &AgentMove) -> Result<()> {
        sqlx::query("UPDATE agent_profiles SET pending_move = $2 WHERE id = $1")
            .bind(agent_id)
            .bind(serde_json::to_value(target)?)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn apply_pending_move(&self, agent_id: Uuid) -> Result<()> {
        let row = sqlx::query_as::<_, (Option<Uuid>, Option<Value>)>(
            "SELECT owner_user_id, pending_move FROM agent_profiles WHERE id = $1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((Some(owner), Some(pending))) = row else {
            return Ok(());
        };
        let Ok(target) = serde_json::from_value::<AgentMove>(pending) else {
            self.clear_pending_move(agent_id).await?;
            return Ok(());
        };
        match self.move_agent(owner, agent_id, &target, true).await {
            Ok(_) | Err(HandoffRunnerError::Busy) => Ok(()),
            Err(HandoffRunnerError::Internal(error)) => Err(error),
            Err(error) => {
                self.clear_pending_move(agent_id).await?;
                let reason = match error {
                    HandoffRunnerError::AgentNotFound => "the agent is gone".to_string(),
                    HandoffRunnerError::RunnerNotFound => "that device is gone".to_string(),
                    HandoffRunnerError::InvalidRequest(message) => message.to_string(),
                    _ => "unknown".to_string(),
                };
                let mut tx = self.pool.begin().await?;
                emit_in(
                    &mut tx,
                    &NewEvent::new("agent.move_failed")
                        .agent(agent_id)
                        .payload(serde_json::json!({ "error": reason, "runner_id": target.runner_id })),
                    Some((agent_id, "agent.move_failed")),
                )
                .await?;
                tx.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn apply_pending_moves(&self) -> Result<()> {
        let agents = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_profiles WHERE pending_move IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        for agent_id in agents {
            self.apply_pending_move(agent_id).await?;
        }
        Ok(())
    }

    pub async fn cloud_online(&self) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name IN ($1, $2) AND datname = current_database())",
        )
        .bind(WORLD_WORKER_APP)
        .bind(LEGACY_WORLD_WORKER_APP)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn has_pending_move(&self, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT pending_move IS NOT NULL FROM agent_profiles WHERE id = $1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false))
    }

    pub async fn in_place_switch(&self, agent_id: Uuid) -> Result<Option<(String, String)>> {
        Ok(sqlx::query_as::<_, (String, String)>(
            "SELECT pending_move->>'cli', pending_move->>'model' FROM agent_profiles
             WHERE id = $1 AND pending_move IS NOT NULL
               AND (pending_move->>'runner_id')::uuid IS NOT DISTINCT FROM runner_id",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn in_place_switches(&self, agent_ids: &[Uuid]) -> Result<Vec<(Uuid, String, String)>> {
        if agent_ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, (Uuid, String, String)>(
            "SELECT id, pending_move->>'cli', pending_move->>'model' FROM agent_profiles
             WHERE id = ANY($1) AND pending_move IS NOT NULL
               AND (pending_move->>'runner_id')::uuid IS NOT DISTINCT FROM runner_id",
        )
        .bind(agent_ids)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn move_queued_elsewhere(&self, agent_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT COALESCE(r.name, 'the cloud') FROM agent_profiles a
             LEFT JOIN world_runners r ON r.id = (a.pending_move->>'runner_id')::uuid
             WHERE a.id = $1 AND a.pending_move IS NOT NULL
               AND (a.pending_move->>'runner_id')::uuid IS DISTINCT FROM a.runner_id",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn drop_in_place_move(&self, agent_id: Uuid) -> Result<bool> {
        let dropped = sqlx::query(
            "UPDATE agent_profiles SET pending_move = NULL
             WHERE id = $1 AND pending_move IS NOT NULL
               AND (pending_move->>'runner_id')::uuid IS NOT DISTINCT FROM runner_id",
        )
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(dropped.rows_affected() > 0)
    }

    async fn clear_pending_move(&self, agent_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_profiles SET pending_move = NULL WHERE id = $1")
            .bind(agent_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn move_agent(
        &self,
        owner: Uuid,
        agent_id: Uuid,
        target: &AgentMove,
        pending: bool,
    ) -> std::result::Result<bool, HandoffRunnerError> {
        let cli = target.cli.trim();
        let model = target.model.trim();
        let summary = target.summary.trim();
        if !crate::backends::is_agent_cli(cli) {
            return Err(HandoffRunnerError::InvalidRequest(
                "CLI must be claude, codex, cursor, opencode or omp",
            ));
        }
        if model.is_empty() || model.chars().count() > 200 {
            return Err(HandoffRunnerError::InvalidRequest(
                "Model must be between 1 and 200 characters",
            ));
        }
        if summary.is_empty() || summary.chars().count() > 50_000 {
            return Err(HandoffRunnerError::InvalidRequest(
                "Carry-over memory must be between 1 and 50000 characters",
            ));
        }

        if target.runner_id.is_none() && !self.cloud_online().await.map_err(HandoffRunnerError::Internal)? {
            return Err(HandoffRunnerError::InvalidRequest(CLOUD_OFFLINE));
        }
        if target.runner_id.is_none() && !crate::backends::allowed_on(cli, None) {
            return Err(HandoffRunnerError::InvalidRequest(crate::backends::OPENCODE_DISABLED));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(runner_id) = target.runner_id {
            let capabilities = sqlx::query_scalar::<_, Value>(
                "SELECT w.capabilities FROM world_runners w
                 WHERE w.id = $1 AND w.status <> 'revoked'
                   AND w.owner_user_id = $2 FOR SHARE",
            )
            .bind(runner_id)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(HandoffRunnerError::RunnerNotFound)?;
            if capabilities.get("backends").and_then(|b| b.get(cli)).and_then(Value::as_bool) == Some(false) {
                return Err(HandoffRunnerError::InvalidRequest(
                    "That device does not have this CLI installed",
                ));
            }
            if !crate::backends::allowed_on(cli, Some(&capabilities)) {
                return Err(HandoffRunnerError::InvalidRequest(crate::backends::OPENCODE_DISABLED));
            }
        }
        let agent = sqlx::query_as::<_, (Option<Uuid>, String, String, Option<String>, Option<Value>)>(
            "SELECT runner_id, cli, model, inbox_claimed_by, pending_move FROM agent_profiles
             WHERE id = $1 AND owner_user_id = $2 FOR UPDATE",
        )
        .bind(agent_id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(HandoffRunnerError::AgentNotFound)?;
        if pending && agent.4.as_ref() != Some(&serde_json::to_value(target).map_err(|e| HandoffRunnerError::Internal(e.into()))?) {
            return Ok(false);
        }
        if agent.0 == target.runner_id && agent.1 == cli && agent.2 == model {
            return Err(HandoffRunnerError::InvalidRequest(
                "This agent already runs there with that CLI and model",
            ));
        }
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_sessions WHERE agent_id = $1 ORDER BY id FOR UPDATE",
        )
        .bind(agent_id)
        .fetch_all(&mut *tx)
        .await?;
        let busy = agent.3.is_some()
            || sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_inbox i
                   WHERE i.agent_id = $1 AND i.status = 'processing'
                   UNION ALL
                   SELECT 1 FROM session_runs r
                   WHERE r.agent_id = $1 AND r.status IN ('queued', 'running') AND r.kind <> 'direct'
                   UNION ALL
                   SELECT 1 FROM runner_leases l
                   JOIN session_runs r ON r.id = l.run_id
                   WHERE r.agent_id = $1 AND l.status IN ('active', 'finishing') AND r.kind <> 'direct'
                 )",
            )
            .bind(agent_id)
            .fetch_one(&mut *tx)
            .await?;
        if busy {
            return Err(HandoffRunnerError::Busy);
        }

        sqlx::query(
            "UPDATE agent_sessions SET status = 'archived', updated_at = NOW()
             WHERE agent_id = $1 AND status <> 'archived' AND role <> 'direct'",
        )
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_profiles SET runner_id = $2, cli = $3, model = $4, pending_move = NULL,
               status = 'idle', status_detail = NULL, updated_at = NOW()
             WHERE id = $1",
        )
        .bind(agent_id)
        .bind(target.runner_id)
        .bind(cli)
        .bind(model)
        .execute(&mut *tx)
        .await?;
        let session_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, working_dir, usage_baseline, summary)
             VALUES ($1, 'main', $2, $3, $4, '{}'::jsonb, $5)
             RETURNING id",
        )
        .bind(agent_id)
        .bind(cli)
        .bind(model)
        .bind(format!("agents/{agent_id}"))
        .bind(summary)
        .fetch_one(&mut *tx)
        .await?;
        emit_in(
            &mut tx,
            &NewEvent::new("agent.updated")
                .agent(agent_id)
                .session(session_id)
                .actor(Some(owner))
                .payload(serde_json::json!({
                    "cli": cli,
                    "model": model,
                    "runner_id": target.runner_id,
                    "handoff": true,
                    "by": target.by,
                    "previous": {
                        "cli": agent.1,
                        "model": agent.2,
                        "runner_id": agent.0,
                    },
                })),
            None,
        )
        .await
        .map_err(HandoffRunnerError::Internal)?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn authenticate(&self, id: Uuid, token: &str) -> Result<Option<RunnerView>> {
        let hash = token_hash(token);
        Ok(sqlx::query_as::<_, RunnerView>(
            "SELECT r.id, r.name, r.platform, r.status, r.last_seen_at, r.owner_user_id, r.capabilities, r.created_at, r.shared
             FROM world_runners r JOIN users u ON u.id = r.owner_user_id
             WHERE r.id = $1 AND r.token_hash = $2 AND r.status <> 'revoked'
               AND u.world_role IN ('write', 'admin') AND u.disabled_at IS NULL",
        )
        .bind(id)
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn authenticate_token(&self, token: &str) -> Result<Option<RunnerView>> {
        let hash = token_hash(token);
        Ok(sqlx::query_as::<_, RunnerView>(
            "SELECT r.id, r.name, r.platform, r.status, r.last_seen_at, r.owner_user_id, r.capabilities, r.created_at, r.shared
             FROM world_runners r JOIN users u ON u.id = r.owner_user_id
             WHERE r.token_hash = $1 AND r.status <> 'revoked'
               AND u.world_role IN ('write', 'admin') AND u.disabled_at IS NULL",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn heartbeat(&self, id: Uuid, capabilities: &Value) -> Result<Option<RunnerView>> {
        Ok(sqlx::query_as::<_, RunnerView>(
            "WITH beat AS (
               UPDATE world_runners r SET status = CASE WHEN r.status = 'paused' THEN r.status ELSE 'online' END,
                 capabilities = CASE WHEN r.capabilities = $2 THEN r.capabilities ELSE $2 END, last_seen_at = NOW(), updated_at = NOW()
               FROM users u
               WHERE r.id = $1 AND r.status <> 'revoked' AND u.id = r.owner_user_id
                 AND u.world_role IN ('write', 'admin')
                 AND (r.status NOT IN ('online', 'paused') OR r.capabilities IS DISTINCT FROM $2
                      OR r.last_seen_at IS NULL OR r.last_seen_at < NOW() - INTERVAL '20 seconds')
               RETURNING r.id, r.name, r.platform, r.status, r.last_seen_at, r.owner_user_id, r.capabilities, r.created_at, r.shared
             )
             SELECT * FROM beat
             UNION ALL
             SELECT r.id, r.name, r.platform, r.status, r.last_seen_at, r.owner_user_id, r.capabilities, r.created_at, r.shared
               FROM world_runners r JOIN users u ON u.id = r.owner_user_id
              WHERE r.id = $1 AND r.status <> 'revoked' AND u.world_role IN ('write', 'admin')
                AND NOT EXISTS (SELECT 1 FROM beat)",
        )
        .bind(id)
        .bind(capabilities)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn closed_tasks(&self, runner_id: Uuid, task_ids: &[Uuid], pressure: bool) -> Result<Vec<ClosedTask>> {
        Ok(sqlx::query_as::<_, ClosedTask>(
            "WITH me AS (SELECT id, owner_user_id FROM world_runners WHERE id = $2),
             state AS (
               SELECT i.id, COALESCE(t.status, 'deleted') AS status,
                      GREATEST(t.updated_at, (SELECT MAX(COALESCE(r.finished_at, r.created_at)) FROM session_runs r WHERE r.task_id = i.id)) AS idle_since
                 FROM unnest($1::uuid[]) AS i(id)
                 LEFT JOIN agent_tasks t ON t.id = i.id
                 LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
                 JOIN me ON t.id IS NULL
                         OR a.owner_user_id = me.owner_user_id OR a.runner_id = me.id
                         OR (t.assigned_agent_id IS NULL AND t.user_id = me.owner_user_id)
                WHERE NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.task_id = i.id AND r.status IN ('queued', 'running'))
             )
             SELECT id, status, idle_since FROM state
              WHERE status IN ('deleted', 'cancelled', 'done', 'failed')
                AND COALESCE(idle_since, '-infinity'::timestamptz) < NOW() - make_interval(secs => CASE
                      WHEN status IN ('deleted', 'cancelled') OR $3::boolean THEN $4::double precision
                      WHEN status = 'done' THEN $5::double precision
                      ELSE $6::double precision END)
              ORDER BY idle_since ASC NULLS FIRST",
        )
        .bind(task_ids)
        .bind(runner_id)
        .bind(pressure)
        .bind(WORKSPACE_SETTLE_SECONDS)
        .bind(WORKSPACE_DONE_GRACE_SECONDS)
        .bind(WORKSPACE_FAILED_GRACE_SECONDS)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn quests_by_prefix(&self, runner_id: Uuid, prefixes: &[String]) -> Result<Vec<(String, Uuid)>> {
        let (mut names, mut lows, mut highs) = (Vec::new(), Vec::new(), Vec::new());
        for prefix in prefixes.iter().filter(|prefix| prefix.len() == 12 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())).take(PREFIXES_PER_LOOKUP) {
            let (Ok(low), Ok(high)) = (Uuid::parse_str(&format!("{prefix}{}", "0".repeat(20))), Uuid::parse_str(&format!("{prefix}{}", "f".repeat(20)))) else {
                continue;
            };
            names.push(prefix.to_ascii_lowercase());
            lows.push(low);
            highs.push(high);
        }
        if names.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, (String, Uuid)>(
            "WITH me AS (SELECT id, owner_user_id FROM world_runners WHERE id = $4),
             wanted AS (SELECT * FROM unnest($1::text[], $2::uuid[], $3::uuid[]) AS w(prefix, low, high)),
             hits AS (
               SELECT w.prefix, t.id
                 FROM wanted w
                 JOIN agent_tasks t ON t.id BETWEEN w.low AND w.high
                 LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
                 JOIN me ON a.owner_user_id = me.owner_user_id OR a.runner_id = me.id
                         OR (t.assigned_agent_id IS NULL AND t.user_id = me.owner_user_id)
             )
             SELECT prefix, (array_agg(id))[1] FROM hits GROUP BY prefix HAVING count(*) = 1",
        )
        .bind(&names)
        .bind(&lows)
        .bind(&highs)
        .bind(runner_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn closed_sessions(&self, runner_id: Uuid, session_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "WITH me AS (SELECT id, owner_user_id FROM world_runners WHERE id = $2),
             state AS (
               SELECT i.id, s.role, s.status,
                      COALESCE(GREATEST(s.updated_at, (SELECT MAX(COALESCE(r.finished_at, r.created_at)) FROM session_runs r WHERE r.session_id = i.id)), '-infinity'::timestamptz) AS idle_since
                 FROM unnest($1::uuid[]) AS i(id)
                 LEFT JOIN agent_sessions s ON s.id = i.id
                 LEFT JOIN agent_profiles a ON a.id = s.agent_id
                 JOIN me ON s.id IS NULL OR a.owner_user_id = me.owner_user_id OR s.runner_id = me.id
                WHERE NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = i.id AND r.status IN ('queued', 'running'))
             )
             SELECT id FROM state
              WHERE idle_since < NOW() - make_interval(secs => $3::double precision)
                AND (role IS NULL
                     OR role IN ('direct', 'quick') AND status = 'archived'
                     OR role = 'direct' AND idle_since < NOW() - make_interval(secs => $4::double precision))",
        )
        .bind(session_ids)
        .bind(runner_id)
        .bind(WORKSPACE_SETTLE_SECONDS)
        .bind(FOLDER_SESSION_IDLE_SECONDS)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn idle_tasks(&self, runner_id: Uuid, task_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "WITH me AS (SELECT id, owner_user_id FROM world_runners WHERE id = $2)
             SELECT t.id
               FROM agent_tasks t
               LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
               JOIN me ON a.owner_user_id = me.owner_user_id OR a.runner_id = me.id
                       OR (t.assigned_agent_id IS NULL AND t.user_id = me.owner_user_id)
              WHERE t.id = ANY($1::uuid[])
                AND t.status NOT IN ('cancelled', 'done', 'failed')
                AND NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.task_id = t.id AND r.status IN ('queued', 'running'))
                AND GREATEST(t.updated_at, (SELECT MAX(COALESCE(r.finished_at, r.created_at)) FROM session_runs r WHERE r.task_id = t.id))
                      < NOW() - make_interval(secs => $3::double precision)",
        )
        .bind(task_ids)
        .bind(runner_id)
        .bind(WORKSPACE_IDLE_BUILD_SECONDS)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn take_workspace_cleanup(&self, runner_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "UPDATE world_runners SET clean_workspaces_at = NULL WHERE id = $1 AND clean_workspaces_at IS NOT NULL RETURNING true",
        )
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false))
    }

    pub async fn take_cache_cleanup(&self, runner_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "UPDATE world_runners SET clean_caches_at = NULL WHERE id = $1 AND clean_caches_at IS NOT NULL RETURNING true",
        )
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false))
    }

    pub async fn warn_disk_low(&self, runner_id: Uuid, notice: &DiskNotice) -> Result<bool> {
        let Some((owner, name)) = sqlx::query_as::<_, (Uuid, String)>("SELECT owner_user_id, name FROM world_runners WHERE id = $1")
            .bind(runner_id)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(false);
        };
        let recent: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM world_events WHERE type = 'runner.disk_low' AND payload->>'runner_id' = $1
                              AND created_at > NOW() - INTERVAL '1 hour')",
        )
        .bind(runner_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        if recent {
            return Ok(false);
        }
        let mut notice = notice.clone();
        let quests: Vec<Uuid> = notice.kept.iter().filter_map(|folder| folder.quest).collect();
        if !quests.is_empty() {
            let titles: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT t.id, t.title FROM agent_tasks t LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
                  WHERE t.id = ANY($1) AND (a.owner_user_id = $2 OR t.user_id = $2)",
            )
            .bind(&quests)
            .bind(owner)
            .fetch_all(&self.pool)
            .await?;
            for folder in &mut notice.kept {
                if let Some((_, title)) = titles.iter().find(|(id, _)| Some(*id) == folder.quest) {
                    folder.name = format!("\"{title}\"");
                }
            }
        }
        let message = notice.message(&name);
        let mut tx = self.pool.begin().await?;
        emit_in(
            &mut tx,
            &NewEvent::new("runner.disk_low").payload(serde_json::json!({
                "runner_id": runner_id,
                "runner_name": name,
                "owner_user_id": owner,
                "free_bytes": notice.free_bytes,
                "freed_bytes": notice.freed_bytes,
                "removed": notice.removed,
                "still_low": notice.still_low,
                "folders": notice.folders,
                "message": message,
            })),
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn set_disk_wait(&self, runner_id: Uuid, low: bool) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let changed = if low {
            sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<Uuid>, String)>(
                "UPDATE session_runs r SET waiting_for = 'disk'
                 WHERE r.runner_id = $1 AND r.status = 'queued' AND r.waiting_for IS NULL AND r.kind = ANY($2)
                   AND NOT EXISTS (SELECT 1 FROM session_runs active WHERE active.session_id = r.session_id AND active.status = 'running')
                 RETURNING r.id, r.session_id, r.agent_id, r.task_id, r.kind",
            )
        } else {
            sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<Uuid>, String)>(
                "UPDATE session_runs r SET waiting_for = NULL
                 WHERE r.runner_id = $1 AND r.status = 'queued' AND r.waiting_for = 'disk' AND r.kind = ANY($2)
                 RETURNING r.id, r.session_id, r.agent_id, r.task_id, r.kind",
            )
        }
        .bind(runner_id)
        .bind(WORK_RUN_KINDS.map(String::from).to_vec())
        .fetch_all(&mut *tx)
        .await?;
        for (run_id, session_id, agent_id, task_id, kind) in &changed {
            emit_waiting(&mut tx, *run_id, *session_id, *agent_id, *task_id, kind, low).await?;
        }
        tx.commit().await?;
        Ok(changed.len())
    }

    pub async fn requeue_for_space(&self, lease: &LeaseContext, error: &str) -> Result<bool> {
        if !crate::run_recording::waits_for_space(&lease.kind, error) {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        if !lock_finishing_lease(&mut tx, lease).await? {
            return Ok(false);
        }
        if !requeue_for_space(&mut tx, lease.run_id, Some(lease.lease_id)).await? {
            return Ok(false);
        }
        sqlx::query("UPDATE runner_leases SET status = 'completed', finished_at = NOW(), completion_payload = NULL WHERE id = $1 AND status = 'finishing'")
            .bind(lease.lease_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn claim(&self, runner_id: Uuid, mcp_token: &str) -> Result<Option<ClaimedRun>> {
        self.claim_kinds(runner_id, mcp_token, &[]).await
    }

    pub async fn claim_kinds(
        &self,
        runner_id: Uuid,
        mcp_token: &str,
        kinds: &[String],
    ) -> Result<Option<ClaimedRun>> {
        let lease_id = Uuid::new_v4();
        let mcp_hash = token_hash(mcp_token);
        let expires = Utc::now() + Duration::seconds(RUNNER_LEASE_SECONDS);
        let mut tx = self.pool.begin().await?;
        let runner_ready = sqlx::query_scalar::<_, Uuid>(
            "SELECT w.id FROM world_runners w JOIN users u ON u.id = w.owner_user_id
             WHERE w.id = $1 AND w.status IN ('online', 'offline')
               AND u.world_role IN ('write', 'admin') FOR UPDATE OF w",
        )
        .bind(runner_id)
        .fetch_optional(&mut *tx)
        .await?;
        if runner_ready.is_none() {
            tx.rollback().await?;
            return Ok(None);
        }
        let run_id = sqlx::query_scalar::<_, Uuid>(
            &serving("SELECT q.id FROM session_runs q
             JOIN agent_sessions s ON s.id = q.session_id
             JOIN agent_profiles a ON a.id = q.agent_id
             JOIN users u ON u.id = a.owner_user_id
             JOIN world_runners w ON w.id = $1
             WHERE q.status = 'queued' AND q.runner_id = $1 AND s.runner_id = $1
               AND (a.runner_id = $1 OR (q.kind = 'direct' AND s.role = 'direct' AND a.owner_user_id = w.owner_user_id))
               AND (q.kind <> 'quick' OR (a.owner_user_id = w.owner_user_id AND COALESCE((w.capabilities->>'quick')::boolean, FALSE)))
               AND (q.kind <> 'action' OR (a.owner_user_id = w.owner_user_id AND COALESCE((w.capabilities->>'pr_actions')::boolean, FALSE)))
               AND __SERVES__
               AND u.world_role IN ('read', 'write', 'admin')
               AND a.paused_reason IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs active
                 WHERE active.session_id = q.session_id AND active.status = 'running'
               )
               AND NOT EXISTS (
                 SELECT 1 FROM runner_leases lease
                 JOIN session_runs leased ON leased.id = lease.run_id
                 WHERE leased.session_id = q.session_id AND lease.status IN ('active', 'finishing')
               )
               AND (q.kind <> 'followup' OR s.cli_session_id IS NOT NULL)
               AND (q.not_before IS NULL OR q.not_before <= NOW())
               AND __FOLDER_FREE_Q__
               AND (cardinality($2::text[]) = 0 OR q.kind = ANY($2))
             ORDER BY __URGENT__ DESC, q.created_at
             FOR UPDATE OF q, s SKIP LOCKED LIMIT 1"),
        )
        .bind(runner_id)
        .bind(kinds)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(run_id) = run_id else {
            tx.rollback().await?;
            return Ok(None);
        };
        sqlx::query("DELETE FROM runner_leases WHERE run_id = $1 AND status IN ('completed', 'failed', 'cancelled', 'expired')")
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO runner_leases (id, run_id, runner_id, mcp_token_hash, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(lease_id)
        .bind(run_id)
        .bind(runner_id)
        .bind(mcp_hash)
        .bind(expires)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE session_runs SET status = 'running', claimed_by = $2, started_at = NOW(), heartbeat_at = NOW(),
               lease_id = $3, lease_expires_at = $4, waiting_for = NULL WHERE id = $1",
        )
        .bind(run_id)
        .bind(format!("runner:{runner_id}"))
        .bind(lease_id)
        .bind(expires)
        .execute(&mut *tx)
        .await?;
        let claimed = sqlx::query_as::<_, ClaimedRun>(
            "SELECT l.id AS lease_id, r.id AS run_id, r.session_id, r.agent_id, r.runner_id, r.task_id,
               r.kind, r.prompt, r.timeout_secs, s.backend, s.model, s.cli_session_id, r.fork_from, s.role,
               t.repo AS task_repo, NULL::text AS task_branch, t.target_branch AS task_base_branch,
               a.name AS agent_name, a.slug AS agent_slug, a.persona AS agent_persona,
               a.system_prompt_addendum AS agent_addendum, a.owner_user_id
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_sessions s ON s.id = r.session_id
             JOIN agent_profiles a ON a.id = r.agent_id
             LEFT JOIN agent_tasks t ON t.id = r.task_id
             WHERE l.id = $1",
        )
        .bind(lease_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(claimed))
    }

    pub async fn heartbeat_lease(&self, runner_id: Uuid, lease_id: Uuid) -> Result<Option<RenewedLease>> {
        Ok(self.heartbeat_leases(runner_id, &[lease_id]).await?.into_iter().next())
    }

    pub async fn heartbeat_leases(&self, runner_id: Uuid, lease_ids: &[Uuid]) -> Result<Vec<RenewedLease>> {
        if lease_ids.is_empty() {
            return Ok(Vec::new());
        }
        let expires = Utc::now() + Duration::seconds(RUNNER_LEASE_SECONDS);
        Ok(sqlx::query_as::<_, RenewedLease>(
            &serving("WITH renewed AS (
               UPDATE runner_leases l SET heartbeat_at = NOW(), expires_at = $3
               FROM session_runs r, world_runners w, users u, agent_profiles a
               WHERE l.id = ANY($1) AND l.runner_id = $2 AND l.status = 'active'
                 AND r.id = l.run_id AND r.status = 'running' AND r.lease_id = l.id
                 AND r.started_at + make_interval(secs => r.timeout_secs::double precision) > NOW()
                 AND w.id = l.runner_id AND w.status NOT IN ('paused', 'revoked')
                 AND u.id = w.owner_user_id AND u.world_role IN ('write', 'admin')
                 AND a.id = r.agent_id AND __HOME__
                 AND __SERVES__
               RETURNING l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id,
                 r.kind, r.timeout_secs
             ), runs AS (
               UPDATE session_runs s SET heartbeat_at = NOW(), lease_expires_at = $3
               FROM renewed
               WHERE s.lease_id = renewed.lease_id AND s.status = 'running'
             )
             SELECT renewed.*, sess.role AS session_role
             FROM renewed LEFT JOIN agent_sessions sess ON sess.id = renewed.session_id"),
        )
        .bind(lease_ids)
        .bind(runner_id)
        .bind(expires)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn fail_claim(&self, runner_id: Uuid, lease_id: Uuid, error: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "UPDATE runner_leases SET status = 'failed', finished_at = NOW()
             WHERE id = $1 AND runner_id = $2 AND status = 'active' RETURNING run_id",
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(run_id) = run_id {
            sqlx::query(
                "UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW()
                 WHERE id = $1 AND status = 'running'",
            )
            .bind(run_id)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_inbox SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END,
                   error = $2
                 WHERE id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = $1)",
            )
            .bind(run_id)
            .bind(error)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn lease_for_runner(
        &self,
        runner_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<LeaseContext>> {
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             JOIN users u ON u.id = w.owner_user_id
             WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'active'
               AND r.status = 'running' AND r.lease_id = l.id AND __HOME__
               AND __SERVES__ AND w.status NOT IN ('paused', 'revoked')
               AND u.world_role IN ('write', 'admin')"),
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn lease_state(&self, runner_id: Uuid, lease_id: Uuid) -> Result<Option<LeaseState>> {
        Ok(sqlx::query_as::<_, LeaseState>(
            "SELECT l.status AS lease_status, r.status AS run_status, r.kind, r.cause, r.stop_requested_by,
                    left(r.error, 300) AS error, EXTRACT(EPOCH FROM (NOW() - l.finished_at))::bigint AS settled_secs_ago
             FROM runner_leases l JOIN session_runs r ON r.id = l.run_id
             WHERE l.id = $1 AND l.runner_id = $2",
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn lease_for_submission(
        &self,
        runner_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<LeaseContext>> {
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             JOIN users u ON u.id = w.owner_user_id
             WHERE l.id = $1 AND l.runner_id = $2
               AND (
                 (l.status = 'active' AND r.status = 'running'
                   AND w.status NOT IN ('paused', 'revoked') AND u.world_role IN ('write', 'admin'))
                 OR (l.status IN ('finishing', 'completed')
                   AND (r.status IN ('completed', 'failed')
                     OR (r.status = 'running' AND w.status NOT IN ('paused', 'revoked')
                       AND u.world_role IN ('write', 'admin'))))
               )
               AND r.lease_id = l.id AND __HOME__
               AND __SERVES__"),
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn late_lease(&self, runner_id: Uuid, lease_id: Uuid) -> Result<Option<LeaseContext>> {
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             WHERE l.id = $1 AND l.runner_id = $2
               AND l.status IN ('cancelled', 'expired', 'failed')
               AND r.status IN ('failed', 'cancelled', 'timed_out') AND r.lease_id = l.id
               AND __HOME__ AND __SERVES__"),
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn keep_late_spend(&self, lease: &LeaseContext, reported: &Reported) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let kept = crate::run_recording::keep_spend_of_closed_run(&mut tx, lease.run_id, lease.session_id, reported).await?;
        tx.commit().await?;
        Ok(kept)
    }

    pub async fn lease_for_completion(
        &self,
        runner_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<LeaseContext>> {
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             JOIN users u ON u.id = w.owner_user_id
             WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'finishing'
               AND r.status IN ('running', 'completed', 'failed') AND r.lease_id = l.id AND __HOME__
               AND __SERVES__
               AND (r.status IN ('completed', 'failed')
                 OR (w.status NOT IN ('paused', 'revoked') AND u.world_role IN ('write', 'admin'))) "),
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn lease_for_mcp(&self, token: &str) -> Result<Option<LeaseContext>> {
        let hash = token_hash(token);
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_sessions s ON s.id = r.session_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             JOIN users u ON u.id = w.owner_user_id
             WHERE l.mcp_token_hash = $1 AND l.status = 'active'
               AND r.status = 'running' AND r.lease_id = l.id AND __HOME__
               AND (r.kind IN ('turn', 'task', 'followup', 'quick') OR (r.kind = 'aside' AND COALESCE((s.asked_by->>'guest')::boolean, FALSE) = FALSE AND COALESCE((s.asked_by->>'lean')::boolean, FALSE) = FALSE))
               AND r.started_at + make_interval(secs => r.timeout_secs::double precision) > NOW()
               AND __SERVES__ AND w.status NOT IN ('paused', 'revoked')
               AND u.world_role IN ('write', 'admin')"),
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn record_events(&self, lease: &LeaseContext, events: &[RunnerEvent]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let active = sqlx::query_scalar::<_, bool>(
            &serving("SELECT EXISTS(
               SELECT 1 FROM runner_leases l
               JOIN world_runners w ON w.id = l.runner_id
               JOIN users u ON u.id = w.owner_user_id
               JOIN session_runs r ON r.id = l.run_id
               JOIN agent_profiles a ON a.id = r.agent_id
               WHERE l.id = $1 AND l.status = 'active'
                 AND r.status = 'running' AND r.lease_id = l.id AND __HOME__
                 AND __SERVES__ AND w.status NOT IN ('paused', 'revoked')
                 AND u.world_role IN ('write', 'admin'))"),
        )
        .bind(lease.lease_id)
        .fetch_one(&mut *tx)
        .await?;
        if !active {
            return Err(anyhow!("lease is no longer active"));
        }
        let batch = &events[..events.len().min(100)];
        if !batch.is_empty() {
            let kinds: Vec<&str> = batch.iter().map(|event| event.kind.as_str()).collect();
            let tools: Vec<Option<String>> = batch
                .iter()
                .map(|event| event.tool.as_ref().map(|tool| tool.chars().take(200).collect::<String>()))
                .collect();
            let bodies: Vec<String> = batch.iter().map(|event| event.text.chars().take(20_000).collect()).collect();
            sqlx::query(
                "INSERT INTO session_run_events (run_id, kind, tool, body)
                 SELECT $1, e.kind, e.tool, e.body
                 FROM UNNEST($2::text[], $3::text[], $4::text[]) WITH ORDINALITY AS e(kind, tool, body, ord)
                 ORDER BY e.ord",
            )
            .bind(lease.run_id)
            .bind(&kinds)
            .bind(&tools)
            .bind(&bodies)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn begin_completion(
        &self,
        runner_id: Uuid,
        lease_id: Uuid,
        completion_hash: &[u8],
        completion: &RunnerCompletion,
    ) -> Result<BeginCompletion> {
        let payload = serde_json::to_value(completion)?;
        let changed = sqlx::query(
            &serving("UPDATE runner_leases l SET status = 'finishing', completion_hash = $3,
               completion_payload = $4, completion_processing_at = NOW()
             FROM session_runs r, agent_profiles a, world_runners w, users u
             WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'active'
               AND r.id = l.run_id AND r.status = 'running' AND r.lease_id = l.id
               AND a.id = r.agent_id AND __HOME__
               AND w.id = l.runner_id AND w.status NOT IN ('paused', 'revoked')
               AND __SERVES__
               AND u.id = w.owner_user_id AND u.world_role IN ('write', 'admin')"),
        )
        .bind(lease_id)
        .bind(runner_id)
        .bind(completion_hash)
        .bind(&payload)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 1 {
            return Ok(BeginCompletion::Begun);
        }
        let status = sqlx::query_as::<_, (String, Option<Vec<u8>>, Option<DateTime<Utc>>)>(
            "SELECT status, completion_hash, completion_processing_at
             FROM runner_leases WHERE id = $1 AND runner_id = $2",
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?;
        match status {
            Some((status, Some(hash), _)) if status == "completed" && hash == completion_hash => {
                Ok(BeginCompletion::AlreadyCompleted)
            }
            Some((status, Some(hash), processing_at))
                if status == "finishing" && hash == completion_hash =>
            {
                let stale = processing_at.is_none_or(|at| at < Utc::now() - Duration::seconds(120));
                if !stale {
                    return Ok(BeginCompletion::Busy);
                }
                let reclaimed = sqlx::query(
                    "UPDATE runner_leases SET completion_processing_at = NOW()
                     WHERE id = $1 AND runner_id = $2 AND status = 'finishing'
                       AND completion_hash = $3
                       AND (completion_processing_at IS NULL OR completion_processing_at < NOW() - INTERVAL '120 seconds')",
                )
                .bind(lease_id)
                .bind(runner_id)
                .bind(completion_hash)
                .execute(&self.pool)
                .await?
                .rows_affected();
                Ok(if reclaimed == 1 {
                    BeginCompletion::Begun
                } else {
                    BeginCompletion::Busy
                })
            }
            _ => Ok(BeginCompletion::Conflict),
        }
    }

    pub async fn apply_success(
        &self,
        lease: &LeaseContext,
        output: &str,
        cli_session_id: Option<&str>,
        reported: Option<&Reported>,
        context_tokens: Option<i64>,
    ) -> Result<ApplyCompletion> {
        let mut tx = self.pool.begin().await?;
        if !lock_finishing_lease(&mut tx, lease).await? {
            return Ok(ApplyCompletion::Stopped);
        }
        let live = sqlx::query_scalar::<_, bool>("SELECT status = 'running' AND lease_id = $2 FROM session_runs WHERE id = $1 FOR UPDATE")
            .bind(lease.run_id)
            .bind(lease.lease_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
        if live {
            let billed = match reported {
                Some(reported) => bill_session(&mut tx, lease.session_id, reported).await?,
                None => None,
            };
            sqlx::query(
                "UPDATE session_runs SET status = 'completed', output = $2, error = NULL,
                   cost_usd = $3, input_tokens = $4, cached_input_tokens = $5,
                   output_tokens = $6, finished_at = NOW()
                 WHERE id = $1",
            )
            .bind(lease.run_id)
            .bind(output)
            .bind(billed.as_ref().map_or(0.0, |billed| billed.cost_usd))
            .bind(billed.as_ref().map_or(0, |billed| billed.input_tokens))
            .bind(billed.as_ref().map_or(0, |billed| billed.cached_input_tokens))
            .bind(billed.as_ref().map_or(0, |billed| billed.output_tokens))
            .execute(&mut *tx)
            .await?;
            write_session_after_run(&mut tx, lease.session_id, cli_session_id, context_tokens).await?;
            tx.commit().await?;
            return Ok(ApplyCompletion::Applied);
        }
        let status =
            sqlx::query_scalar::<_, String>("SELECT status FROM session_runs WHERE id = $1")
                .bind(lease.run_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(if status == "completed" {
            ApplyCompletion::AlreadyApplied
        } else {
            ApplyCompletion::Stopped
        })
    }

    pub async fn apply_failure(
        &self,
        lease: &LeaseContext,
        error: &str,
        cli_session_id: Option<&str>,
    ) -> Result<ApplyCompletion> {
        let mut tx = self.pool.begin().await?;
        if !lock_finishing_lease(&mut tx, lease).await? {
            return Ok(ApplyCompletion::Stopped);
        }
        let changed = sqlx::query(
            "UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW()
             WHERE id = $1 AND status = 'running' AND lease_id = $3",
        )
        .bind(lease.run_id)
        .bind(error)
        .bind(lease.lease_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let status = if changed == 1 {
            sqlx::query(
                "UPDATE agent_sessions SET cli_session_id = COALESCE($2, cli_session_id),
                   updated_at = NOW() WHERE id = $1",
            )
            .bind(lease.session_id)
            .bind(cli_session_id)
            .execute(&mut *tx)
            .await?;
            ApplyCompletion::Applied
        } else {
            let status =
                sqlx::query_scalar::<_, String>("SELECT status FROM session_runs WHERE id = $1")
                    .bind(lease.run_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if status == "failed" {
                ApplyCompletion::AlreadyApplied
            } else {
                ApplyCompletion::Stopped
            }
        };
        tx.commit().await?;
        Ok(status)
    }

    pub async fn pending_completions(
        &self,
        limit: i64,
    ) -> Result<Vec<(LeaseContext, RunnerCompletion)>> {
        let rows = sqlx::query_as::<_, (Uuid, Value)>(
            "UPDATE runner_leases l SET completion_processing_at = NOW()
             FROM world_runners w, users u, session_runs r
             WHERE l.id IN (
               SELECT pending.id FROM runner_leases pending
               JOIN world_runners runner ON runner.id = pending.runner_id
               JOIN users owner ON owner.id = runner.owner_user_id
               JOIN session_runs run ON run.id = pending.run_id
               WHERE pending.status = 'finishing' AND pending.completion_payload IS NOT NULL
                 AND pending.completion_processing_at < NOW() - INTERVAL '120 seconds'
                 AND (run.status IN ('completed', 'failed')
                   OR (runner.status NOT IN ('paused', 'revoked')
                     AND owner.world_role IN ('write', 'admin')))
               ORDER BY pending.completion_processing_at
               FOR UPDATE OF pending SKIP LOCKED LIMIT $1
             ) AND w.id = l.runner_id AND u.id = w.owner_user_id AND r.id = l.run_id
             RETURNING l.id, l.completion_payload",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut pending = Vec::new();
        for (lease_id, payload) in rows {
            let completion = serde_json::from_value(payload)?;
            if let Some(lease) = self.lease_for_completion_by_id(lease_id).await? {
                pending.push((lease, completion));
            }
        }
        Ok(pending)
    }

    async fn lease_for_completion_by_id(&self, lease_id: Uuid) -> Result<Option<LeaseContext>> {
        Ok(sqlx::query_as::<_, LeaseContext>(
            &serving("SELECT l.id AS lease_id, l.run_id, l.runner_id, r.agent_id, r.session_id, a.owner_user_id, r.kind
             FROM runner_leases l
             JOIN session_runs r ON r.id = l.run_id
             JOIN agent_profiles a ON a.id = r.agent_id
             JOIN world_runners w ON w.id = l.runner_id
             JOIN users u ON u.id = w.owner_user_id
             WHERE l.id = $1 AND l.status = 'finishing'
               AND r.status IN ('running', 'completed', 'failed') AND r.lease_id = l.id
               AND __HOME__ AND __SERVES__
               AND (r.status IN ('completed', 'failed')
                 OR (w.status NOT IN ('paused', 'revoked') AND u.world_role IN ('write', 'admin'))) "),
        )
        .bind(lease_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn finish_completion(&self, runner_id: Uuid, lease_id: Uuid) -> Result<bool> {
        let changed = sqlx::query(
            "UPDATE runner_leases l SET status = 'completed', finished_at = NOW(), completion_payload = NULL
             FROM world_runners w, users u, session_runs r
             WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'finishing'
               AND w.id = l.runner_id AND u.id = w.owner_user_id AND r.id = l.run_id
               AND (r.status IN ('completed', 'failed') OR (r.status = 'queued' AND r.waiting_for IS NOT NULL))",
        )
        .bind(lease_id)
        .bind(runner_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 1 {
            return Ok(true);
        }
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM runner_leases l JOIN session_runs r ON r.id = l.run_id
                            WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'completed'
                              AND r.status = 'queued' AND r.lease_id IS NULL AND r.claimed_by IS NULL)",
        )
        .bind(lease_id)
        .bind(runner_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn mark_run_inbox(&self, run_id: Uuid, inbox_ids: &[i64]) -> Result<()> {
        for inbox_id in inbox_ids {
            sqlx::query(
                "INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(run_id)
            .bind(inbox_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn set_run_rules_hash(&self, run_id: Uuid, rules_hash: &str) -> Result<()> {
        sqlx::query("UPDATE session_runs SET runner_rules_hash = $2 WHERE id = $1")
            .bind(run_id)
            .bind(rules_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn run_rules_hash(&self, run_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT runner_rules_hash FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    pub async fn finish_run_inbox(
        &self,
        run_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE agent_inbox SET status = $2, error = $3, done_at = CASE WHEN $2 = 'done' THEN NOW() ELSE done_at END
             WHERE id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = $1)
               AND (status = 'processing' OR NOT (payload ? 'clarification_continuation'))",
        )
        .bind(run_id)
        .bind(status)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_run_inbox_now(&self, run_id: Uuid, error: &str) -> Result<()> {
        sqlx::query(&crate::world::requeue_inbox_sql(
            "id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = $1)",
        ))
        .bind(run_id)
        .bind(error)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_run_inbox(&self, run_id: Uuid, error: &str) -> Result<()> {
        sqlx::query(&crate::world::requeue_inbox_sql(
            "id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = $1)",
        ))
        .bind(run_id)
        .bind(error)
        .bind(crate::world::limit_retry_at(error))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_inbox_agent(&self, runner_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "UPDATE agent_profiles SET inbox_claimed_by = $2, inbox_claimed_at = NOW()
             WHERE id = (
               SELECT a.id FROM agent_profiles a
               JOIN world_runners w ON w.id = a.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE a.runner_id = $1 AND a.paused_reason IS NULL
                 AND {serves} AND w.status NOT IN ('paused', 'revoked')
                 AND u.world_role IN ('write', 'admin')
                 AND (a.inbox_claimed_by IS NULL OR a.inbox_claimed_at < NOW() - INTERVAL '2 minutes')
                 AND EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending' AND (i.retry_at IS NULL OR i.retry_at <= NOW()))
                 AND {ready}
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
                   WHERE r.agent_id = a.id AND s.role = 'main' AND r.status IN ('queued', 'running'))
               ORDER BY {urgent} DESC, (SELECT MIN(i.id) FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending')
               FOR UPDATE SKIP LOCKED LIMIT 1
             ) RETURNING id",
            ready = crate::world::inbox_ready_sql("a"),
            urgent = crate::world::urgent_inbox_sql("a"),
            serves = serves_sql("a", "w"),
        ))
        .bind(runner_id)
        .bind(format!("runner:{runner_id}"))
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn stalled_on_offline_device(&self) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as::<_, (Uuid, String)>(
            "SELECT a.id, w.name FROM agent_profiles a
             JOIN world_runners w ON w.id = a.runner_id
             WHERE a.paused_reason IS NULL
               AND a.status IS DISTINCT FROM 'error'
               AND w.status IN ('online', 'offline')
               AND (w.last_seen_at IS NULL OR w.last_seen_at < NOW() - INTERVAL '5 minutes')
               AND (EXISTS (
                      SELECT 1 FROM agent_inbox i
                      WHERE i.agent_id = a.id AND i.status = 'pending'
                        AND (i.retry_at IS NULL OR i.retry_at <= NOW())
                        AND i.created_at < NOW() - INTERVAL '5 minutes'
                        AND (i.payload->>'from_owner' = 'true' OR i.kind = 'task.assigned'))
                    OR EXISTS (
                      SELECT 1 FROM session_runs r
                      WHERE r.agent_id = a.id AND r.status = 'queued'
                        AND GREATEST(r.created_at, COALESCE(r.not_before, r.created_at)) < NOW() - INTERVAL '5 minutes'))",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn adopt_stranded(&self, runner_id: Uuid, agent_id: Option<Uuid>) -> Result<Vec<Uuid>> {
        if self.cloud_online().await? {
            return Ok(Vec::new());
        }
        let adopted = sqlx::query_scalar::<_, Uuid>(&format!(
            "UPDATE agent_profiles a SET runner_id = w.id, updated_at = NOW()
             FROM world_runners w
             WHERE w.id = $1 AND w.status NOT IN ('paused', 'revoked')
               AND {serves} AND a.runner_id IS NULL AND a.pending_move IS NULL
               AND ($2::uuid IS NULL OR a.id = $2)
               AND COALESCE((w.capabilities->'backends'->>a.cli)::boolean, false)
               AND EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending')
               AND {fresh}
               AND (a.owner_user_id = w.owner_user_id OR NOT {own})
             RETURNING a.id",
            serves = serves_sql("a", "w"),
            fresh = FRESH_AGENT_SQL,
            own = own_device_sql("a"),
        ))
        .bind(runner_id)
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        for id in &adopted {
            crate::world::WorldStore::new(self.pool.clone())
                .emit(
                    crate::world::NewEvent::new("agent.updated").agent(*id).payload(serde_json::json!({ "runner_id": runner_id, "adopted": true })),
                    None,
                )
                .await?;
        }
        Ok(adopted)
    }

    pub async fn runner_for_stranded(&self, agent_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT w.id FROM agent_profiles a
             JOIN world_runners w ON {serves}
             JOIN users u ON u.id = w.owner_user_id
             WHERE a.id = $1 AND a.runner_id IS NULL
               AND u.world_role IN ('write', 'admin')
               AND COALESCE((w.capabilities->'backends'->>a.cli)::boolean, false)
               AND CASE WHEN w.owner_user_id = a.owner_user_id THEN w.status <> 'revoked'
                        ELSE {live} AND NOT {own} END
             ORDER BY (w.owner_user_id = a.owner_user_id) DESC, ({live}) DESC, w.last_seen_at DESC NULLS LAST LIMIT 1",
            serves = serves_sql("a", "w"),
            live = live_runner_sql("w"),
            own = own_device_sql("a"),
        ))
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn realign_session_devices(&self) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT a.id FROM agent_profiles a
             WHERE a.runner_id IS NOT NULL
               AND EXISTS (
                 SELECT 1 FROM agent_sessions s
                 WHERE s.agent_id = a.id AND s.runner_id IS NULL AND s.cli_session_id IS NULL
                   AND s.status <> 'archived' AND s.role <> 'direct')
               AND world_hand_device_to_sessions(a.id) > 0",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn assign_default_runner(&self, agent_id: Uuid) -> Result<Option<Uuid>> {
        if self.cloud_online().await? {
            return Ok(None);
        }
        let Some(runner_id) = self.runner_for_stranded(agent_id).await? else {
            return Ok(None);
        };
        let placed = sqlx::query_scalar::<_, Uuid>(&format!(
            "UPDATE agent_profiles a SET runner_id = $2, updated_at = NOW()
             WHERE a.id = $1 AND a.runner_id IS NULL AND a.pending_move IS NULL AND {FRESH_AGENT_SQL}
             RETURNING a.runner_id"
        ))
        .bind(agent_id)
        .bind(runner_id)
        .fetch_optional(&self.pool)
        .await?;
        if placed.is_some() {
            crate::world::WorldStore::new(self.pool.clone())
                .emit(
                    crate::world::NewEvent::new("agent.updated").agent(agent_id).payload(serde_json::json!({ "runner_id": runner_id, "default_device": true })),
                    None,
                )
                .await?;
        }
        Ok(placed)
    }

    pub async fn live_cli_sessions(&self, runner_id: Uuid) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT s.cli_session_id FROM agent_sessions s JOIN agent_profiles a ON a.id = s.agent_id
             WHERE (a.owner_user_id = (SELECT owner_user_id FROM world_runners WHERE id = $1) OR a.runner_id = $1)
               AND s.status <> 'archived' AND s.role <> 'aside'
               AND s.cli_session_id IS NOT NULL
               AND NOT (s.role = 'quick' AND s.updated_at < NOW() - INTERVAL '7 days'
                 AND NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')))
               AND NOT (
                 (s.role = 'review' AND s.status IN ('completed', 'failed')
                   OR s.role = 'task' AND EXISTS (
                     SELECT 1 FROM agent_tasks t WHERE t.id = s.task_id
                       AND (t.status IN ('done', 'cancelled') OR t.status = 'failed' AND t.updated_at < NOW() - INTERVAL '1 day')
                       AND NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.task_id = t.id AND r.status IN ('queued', 'running'))))
                 AND GREATEST(s.updated_at, COALESCE(
                   (SELECT MAX(COALESCE(r.finished_at, r.created_at)) FROM session_runs r WHERE r.session_id = s.id),
                   s.updated_at)) < NOW() - INTERVAL '24 hours'
                 AND NOT EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')))
             UNION
             SELECT s.rewind->>'from' FROM agent_sessions s JOIN agent_profiles a ON a.id = s.agent_id
             WHERE (a.owner_user_id = (SELECT owner_user_id FROM world_runners WHERE id = $1) OR a.runner_id = $1)
               AND s.rewind->>'from' IS NOT NULL",
        )
        .bind(runner_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn has_work(&self, runner_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            &serving(&"SELECT EXISTS(
               SELECT 1 FROM session_runs r
               JOIN agent_sessions s ON s.id = r.session_id
               JOIN agent_profiles a ON a.id = r.agent_id
               JOIN users o ON o.id = a.owner_user_id
               JOIN world_runners w ON w.id = r.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE r.runner_id = $1 AND r.status = 'queued'
                 AND (r.not_before IS NULL OR r.not_before <= NOW())
                 AND s.runner_id = $1 AND __SERVES__
                 AND (a.runner_id = $1 OR (r.kind = 'direct' AND s.role = 'direct' AND a.owner_user_id = w.owner_user_id))
                 AND (r.kind <> 'quick' OR (a.owner_user_id = w.owner_user_id AND COALESCE((w.capabilities->>'quick')::boolean, FALSE)))
                 AND (r.kind <> 'action' OR (a.owner_user_id = w.owner_user_id AND COALESCE((w.capabilities->>'pr_actions')::boolean, FALSE)))
                 AND o.world_role IN ('read', 'write', 'admin') AND a.paused_reason IS NULL
                 AND (r.kind <> 'followup' OR s.cli_session_id IS NOT NULL)
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs active
                   WHERE active.session_id = r.session_id AND active.status = 'running')
                 AND NOT EXISTS (
                   SELECT 1 FROM runner_leases lease
                   JOIN session_runs leased ON leased.id = lease.run_id
                   WHERE leased.session_id = r.session_id AND lease.status IN ('active', 'finishing'))
                 AND __FOLDER_FREE_R__
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin')
               UNION ALL
               SELECT 1 FROM agent_profiles a
               JOIN world_runners w ON w.id = a.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE a.runner_id = $1 AND a.paused_reason IS NULL
                 AND __SERVES__ AND w.status IN ('online', 'offline')
                 AND u.world_role IN ('write', 'admin')
                 AND (a.inbox_claimed_by IS NULL OR a.inbox_claimed_at < NOW() - INTERVAL '2 minutes')
                 AND EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = a.id AND i.status = 'pending' AND (i.retry_at IS NULL OR i.retry_at <= NOW()))
                 AND {ready}
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
                   WHERE r.agent_id = a.id AND s.role = 'main' AND r.status IN ('queued', 'running'))
               UNION ALL
               SELECT 1 FROM shell_runs s
               JOIN agent_profiles a ON a.id = s.agent_id
               JOIN world_runners w ON w.id = s.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE s.status = 'queued' AND s.runner_id = $1
                 AND a.owner_user_id = w.owner_user_id AND s.owner_user_id = w.owner_user_id
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin')
               UNION ALL
               SELECT 1 FROM terminal_sessions t
               JOIN world_runners w ON w.id = t.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE t.status = 'pending' AND t.closed_at IS NULL AND t.runner_id = $1
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin') AND {serves_kind})"
                .replace("{ready}", &crate::world::inbox_ready_sql("a"))
                .replace("{serves_kind}", crate::terminal::SERVES_KIND)),
        )
        .bind(runner_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn release_inbox_agent(&self, runner_id: Uuid, agent_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE agent_profiles SET inbox_claimed_by = NULL, inbox_claimed_at = NULL
             WHERE id = $1 AND inbox_claimed_by = $2",
        )
        .bind(agent_id)
        .bind(format!("runner:{runner_id}"))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn owner_macs(&self, owner: Uuid) -> Result<Vec<RunnerView>> {
        Ok(self.list(owner).await?.into_iter().filter(is_mac).collect())
    }

    pub async fn folder_device(&self, owner: Uuid, folder: Uuid) -> Result<Option<(RunnerView, String)>> {
        Ok(self.owner_macs(owner).await?.into_iter().find_map(|runner| {
            let name = shared_folders(&runner.capabilities).into_iter().find(|(id, _)| *id == folder)?.1;
            Some((runner, name))
        }))
    }

    pub async fn agent_runner(&self, owner: Uuid, agent_id: Uuid) -> Result<Option<RunnerView>> {
        Ok(sqlx::query_as::<_, RunnerView>(
            "SELECT r.id, r.name, r.platform,
               CASE WHEN r.status = 'online' AND r.last_seen_at < NOW() - INTERVAL '75 seconds' THEN 'offline' ELSE r.status END AS status,
               r.last_seen_at, r.owner_user_id, r.capabilities, r.created_at, r.shared
             FROM agent_profiles a
             JOIN world_runners r ON r.id = a.runner_id
             WHERE a.id = $1 AND a.owner_user_id = $2 AND r.status <> 'revoked' AND r.owner_user_id = $2",
        )
        .bind(agent_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn backend_change_requires_handoff(
        &self,
        agent_id: Uuid,
        backend: &str,
    ) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_profiles a
               JOIN agent_sessions s ON s.agent_id = a.id
               WHERE a.id = $1 AND a.runner_id IS NOT NULL AND s.role = 'main'
                 AND s.status = 'active' AND s.backend <> $2)",
        )
        .bind(agent_id)
        .bind(backend)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn expire_stale(&self) -> Result<Vec<Uuid>> {
        let mut tx = self.pool.begin().await?;
        let timed_out = sqlx::query_scalar::<_, Uuid>(&format!(
            "UPDATE runner_leases l SET status = 'expired', finished_at = NOW()
             FROM session_runs r, world_runners w
             WHERE l.status = 'active' AND r.id = l.run_id AND r.status = 'running' AND w.id = l.runner_id
               AND r.started_at + make_interval(secs => r.timeout_secs::double precision) <= NOW()
               AND NOT {}
             RETURNING l.run_id",
            runner_away()
        ))
        .bind(RUNNER_SLEEP_GRACE_SECONDS as f64)
        .bind(RUNNER_OUTAGE_GRACE_SECONDS as f64)
        .fetch_all(&mut *tx)
        .await?;
        let expired = sqlx::query_as::<_, (Uuid, String)>(&format!(
            "UPDATE runner_leases l SET status = 'expired', finished_at = NOW()
             FROM world_runners w
             WHERE l.status = 'active' AND l.expires_at <= NOW() AND w.id = l.runner_id
               AND NOT {}
             RETURNING l.run_id,
               CASE WHEN w.last_seen_at IS NOT NULL AND w.last_seen_at >= l.expires_at THEN 'runner lease expired'
                 WHEN w.platform = 'macos' THEN $3 ELSE $4 END",
            runner_away()
        ))
        .bind(RUNNER_SLEEP_GRACE_SECONDS as f64)
        .bind(RUNNER_OUTAGE_GRACE_SECONDS as f64)
        .bind(RUNNER_ASLEEP_ERROR)
        .bind(RUNNER_UNREACHABLE_ERROR)
        .fetch_all(&mut *tx)
        .await?;
        if !timed_out.is_empty() {
            sqlx::query(
                "UPDATE session_runs SET status = 'timed_out', error = 'runner job timed out', finished_at = NOW()
                 WHERE id = ANY($1) AND status = 'running'",
            )
            .bind(&timed_out)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_inbox SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END,
                   error = 'runner job timed out'
                 WHERE id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = ANY($1))",
            )
            .bind(&timed_out)
            .execute(&mut *tx)
            .await?;
        }
        let unauthorized = sqlx::query_as::<_, (Uuid, String)>(
            &serving("UPDATE runner_leases l SET status = 'cancelled', finished_at = NOW()
             FROM world_runners w, users u, session_runs r, agent_profiles a
             WHERE l.status IN ('active', 'finishing') AND w.id = l.runner_id
               AND u.id = w.owner_user_id AND r.id = l.run_id AND r.status = 'running'
               AND a.id = r.agent_id
               AND (w.status IN ('paused', 'revoked') OR u.world_role NOT IN ('write', 'admin')
                 OR NOT __HOME__
                 OR NOT COALESCE(__SERVES__, false))
             RETURNING l.run_id,
               CASE WHEN w.status NOT IN ('paused', 'revoked') AND u.world_role IN ('write', 'admin')
                   AND NOT __HOME__
                 THEN $1 ELSE 'runner authorization was revoked' END"),
        )
        .bind(RUNNER_MOVED_ERROR)
        .fetch_all(&mut *tx)
        .await?;
        let stopped: Vec<(Uuid, String)> = expired.into_iter().chain(unauthorized).collect();
        let mut run_ids: Vec<Uuid> = stopped.iter().map(|(id, _)| *id).collect();
        if !stopped.is_empty() {
            let errors: Vec<String> = stopped.into_iter().map(|(_, error)| error).collect();
            sqlx::query(
                "UPDATE session_runs r SET status = 'failed', error = e.error, finished_at = NOW()
                 FROM unnest($1::uuid[], $2::text[]) AS e(id, error)
                 WHERE r.id = e.id AND r.status = 'running'",
            )
            .bind(&run_ids)
            .bind(&errors)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_inbox i SET status = CASE WHEN i.attempts >= 3 THEN 'failed' ELSE 'pending' END,
                   error = e.error
                 FROM runner_run_inbox x, unnest($1::uuid[], $2::text[]) AS e(id, error)
                 WHERE x.run_id = e.id AND i.id = x.inbox_id",
            )
            .bind(&run_ids)
            .bind(&errors)
            .execute(&mut *tx)
            .await?;
        }
        run_ids.extend(timed_out);
        tx.commit().await?;
        Ok(run_ids)
    }
}

async fn lock_finishing_lease(
    tx: &mut Transaction<'_, Postgres>,
    lease: &LeaseContext,
) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        &serving("SELECT l.id FROM runner_leases l
         JOIN world_runners w ON w.id = l.runner_id
         JOIN users u ON u.id = w.owner_user_id
         JOIN session_runs r ON r.id = l.run_id
         JOIN agent_profiles a ON a.id = r.agent_id
         WHERE l.id = $1 AND l.runner_id = $2 AND l.status = 'finishing'
           AND r.id = $3 AND r.lease_id = l.id AND a.id = r.agent_id
           AND __HOME__ AND __SERVES__
           AND (r.status IN ('completed', 'failed')
             OR (r.status = 'running' AND w.status NOT IN ('paused', 'revoked')
               AND u.world_role IN ('write', 'admin')))
         FOR UPDATE OF l"),
    )
    .bind(lease.lease_id)
    .bind(lease.runner_id)
    .bind(lease.run_id)
    .fetch_optional(&mut **tx)
    .await?
    .is_some())
}

pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("secure random generation failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn token_hash(token: &str) -> Vec<u8> {
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .to_vec()
}

fn default_platform() -> String {
    "macos".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    fn usage(input: u64, cached: u64, output: u64) -> RunnerUsage {
        RunnerUsage { input_tokens: input, cached_input_tokens: cached, output_tokens: output, context_tokens: 0 }
    }

    #[test]
    fn a_disk_notice_names_only_what_is_left_for_the_person() {
        let gb = 1_000_000_000;
        let folder = |name: &str, bytes: u64| DiskFolder { name: name.into(), bytes, quest: None };
        let waiting = DiskNotice { free_bytes: gb / 10, freed_bytes: 9 * gb, caches_bytes: 9 * gb, still_low: true, held_bytes: 6 * gb, ..Default::default() };
        let message = waiting.message("Mac");
        assert!(message.starts_with("Mac has only 0.1 GB of disk space left, so new quest runs there wait and start by themselves once there is room."), "{message}");
        assert!(message.contains("cleared build output and test scratch of quests that were not running (9.0 GB)") && message.contains("6.0 GB is build output of quests running now"), "{message}");
        for nothing_to_do in ["quit", "delete", "outside Loka", "Biggest folders", "nothing Loka could"] {
            assert!(!message.contains(nothing_to_do), "running quests clear on their own, so nothing is asked: {message}");
        }

        let stuck = DiskNotice {
            still_low: true,
            free_bytes: gb / 10,
            kept: vec![DiskFolder { quest: Some(Uuid::new_v4()), ..folder("\"Terminals\"", 3 * gb) }],
            unowned: vec![folder("/tmp/loka-q-0123456789ab", 5 * gb)],
            outside_bytes: 2 * gb,
            ..Default::default()
        };
        let message = stuck.message("Mac");
        assert!(message.contains("nothing Loka could safely clear"), "{message}");
        assert!(message.contains("still uses the build folders of \"Terminals\" (3.0 GB)") && message.contains("quit it"), "{message}");
        assert!(message.contains("belong to no quest on this device") && message.contains("/tmp/loka-q-0123456789ab (5.0 GB)"), "{message}");
        assert!(message.contains("free up at least 2.0 GB outside Loka"), "{message}");
        assert!(!message.contains('\u{2014}'));

        let older_runner: DiskNotice = serde_json::from_value(json!({ "free_bytes": gb, "freed_bytes": 0, "removed": 0, "still_low": true, "folders": [{ "name": "temp/loka-q-x", "bytes": gb }] })).unwrap();
        assert!(older_runner.message("Mac").contains("Biggest folders: temp/loka-q-x (1.0 GB)"));
        let recovered = DiskNotice { free_bytes: 20 * gb, removed: 2, freed_bytes: 12 * gb, ..Default::default() };
        assert_eq!(recovered.message("Mac"), "Mac was running low on disk space (20.0 GB free now). Loka removed 2 closed quest folders (12.0 GB).");
    }

    #[test]
    fn a_followup_needs_a_review_only_when_it_changed_the_pull_request() {
        assert_eq!(pr_sync_from(Some("MERGED"), false, true, true), PrSync::Merged, "commits the merged PR already holds");
        assert_eq!(pr_sync_from(Some("MERGED"), false, false, false), PrSync::Changed, "older commits the merged PR never got");
        assert_eq!(pr_sync_from(Some("MERGED"), false, true, false), PrSync::Changed, "new commits after the merge");
        assert_eq!(pr_sync_from(Some("MERGED"), true, false, true), PrSync::Changed, "uncommitted work after the merge");
        assert_eq!(pr_sync_from(Some("OPEN"), false, false, true), PrSync::Unchanged);
        assert_eq!(pr_sync_from(Some("OPEN"), true, false, true), PrSync::Changed);
        assert_eq!(pr_sync_from(Some("OPEN"), false, true, false), PrSync::Changed);
        assert_eq!(pr_sync_from(Some("CLOSED"), false, false, false), PrSync::Unchanged);
        assert_eq!(pr_sync_from(None, false, false, false), PrSync::Unchanged);
        assert_eq!(pr_sync_from(None, false, true, false), PrSync::Changed);
        for sync in [PrSync::Merged, PrSync::Unchanged, PrSync::Changed] {
            assert_eq!(PrSync::from_report(sync.report()), sync);
        }
        assert_eq!(PrSync::from_report(Some(PUBLISH_ELSEWHERE)), PrSync::Changed);
        assert!(PrSync::Changed.needs_review() && !PrSync::Merged.needs_review() && !PrSync::Unchanged.needs_review());
    }

    #[test]
    fn runner_reports_must_look_real() {
        assert!(plausible_cost(1.5));
        assert!(!plausible_cost(-1.0));
        assert!(!plausible_cost(f64::NAN));
        assert!(!plausible_cost(1e300));
        assert!(usage(10, 0, 5).plausible());
        assert!(!usage(u64::MAX, 0, 0).plausible());
        assert!(pr_url_matches("https://github.com/Acme/App/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://evil.example/acme/app/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/prefix/acme/app/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://evil.example/acme/app/pull/12x", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/acme/other/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("http://github.com/acme/app/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/acme/app/pull/13", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/acme/app/pull/0", "acme/app", 0));
        assert!(!plausible_pr_url("https://x.example/\"<script>/pull/1"));
    }

    #[test]
    fn codex_usage_is_a_delta_against_the_inherited_baseline() {
        let main = json!({ "input_tokens": 15821, "cached_input_tokens": 24704, "output_tokens": 10, "cost_usd": 0.5 });
        let fork = usage(33985, 31872, 26);
        assert_eq!(fork.since(&main), usage(18164, 7168, 16));
        assert_eq!(fork.since(&json!({})), fork);
        assert_eq!(fork.since(&Value::Null), fork);
        let mut baseline = main.clone();
        fork.record(&mut baseline);
        assert_eq!(baseline["input_tokens"], 33985);
        assert_eq!(baseline["cost_usd"], 0.5);
        assert_eq!(fork.since(&baseline), usage(0, 0, 0));
    }

    #[test]
    fn codex_usage_counts_a_new_thread_in_full() {
        let stale = json!({ "input_tokens": 900000, "cached_input_tokens": 4000000, "output_tokens": 30000 });
        assert_eq!(usage(12000, 8000, 300).since(&stale), usage(12000, 8000, 300));
        assert_eq!(usage(950000, 3000, 31000).since(&stale), usage(950000, 3000, 31000));
        assert_eq!(usage(905000, 4000000, 29990).since(&stale), usage(5000, 0, 0), "a lost tail on the same thread is not a new thread");
        let mut baseline = Value::Null;
        usage(1, 2, 3).record(&mut baseline);
        assert_eq!(baseline, json!({ "input_tokens": 1, "cached_input_tokens": 2, "output_tokens": 3 }));
    }

    fn token(name: &str) -> String {
        static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        format!("{name}-{}", RUN.get_or_init(|| Uuid::new_v4().simple().to_string()))
    }

    fn global_state() -> &'static tokio::sync::Mutex<()> {
        static GLOBAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn pool() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(
            PgPoolOptions::new()
                .max_connections(std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(8))
                .connect(&url)
                .await
                .unwrap(),
        )
    }

    async fn user(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, name, email, world_role) VALUES ($1, 'Runner test', $2, 'write')")
            .bind(id)
            .bind(format!("{id}@example.test"))
            .execute(pool)
            .await
            .unwrap();
        id
    }

    async fn agent(pool: &PgPool, owner: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_profiles (id, slug, name, cli, model, owner_user_id, visibility)
             VALUES ($1, $2, 'Runner agent', 'codex', 'test-model', $3, 'private')",
        )
        .bind(id)
        .bind(format!("runner-{id}"))
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn registration(store: &RunnerStore, owner: Uuid, name: &str) -> RunnerRegistration {
        let token = random_token().unwrap();
        let runner = sqlx::query_as::<_, RunnerView>(
            "INSERT INTO world_runners (owner_user_id, name, platform, token_hash)
             VALUES ($1, $2, 'macos', $3)
             RETURNING id, name, platform, status, last_seen_at, owner_user_id, capabilities, created_at, shared",
        )
        .bind(owner)
        .bind(name)
        .bind(token_hash(&token))
        .fetch_one(&store.pool)
        .await
        .unwrap();
        RunnerRegistration { runner, token }
    }

    #[tokio::test]
    async fn a_person_pairs_one_runner() {
        let Some(pool) = pool().await else { return };
        let store = RunnerStore::new(pool.clone());
        let owner = user(&pool).await;
        let request = CreateRunner { name: "Mac".into(), platform: "macos".into() };
        let first = store.register(owner, &request).await.unwrap();
        let second = store.register(owner, &request).await.unwrap_err();
        assert!(second.downcast_ref::<OneRunner>().is_some());
        sqlx::query("UPDATE world_runners SET status = 'revoked' WHERE id = $1").bind(first.runner.id).execute(&pool).await.unwrap();
        assert!(store.register(owner, &request).await.is_ok(), "revoking the old Mac frees the slot");
    }

    fn completion(workspace_key: String) -> RunnerCompletion {
        RunnerCompletion {
            output: "done".into(),
            session_id: Some("cli-session".into()),
            cost_usd: 0.1,
            exit_code: 0,
            usage: RunnerUsage {
                input_tokens: 10,
                cached_input_tokens: 2,
                output_tokens: 3,
                context_tokens: 0,
            },
            error: None,
            workspace_key,
            pr_url: None,
            pr_number: None,
            publish_pr_state: None,
            checked_pr_url: None,
            publish_pr_checked_at: None,
            checkpoint: None,
            steered: Vec::new(),
            retryable: false,
            work_summary: None,
            context_sent: None,
            published_head: None,
        }
    }

    async fn queued_run(pool: &PgPool, owner: Uuid, runner: Uuid) -> (Uuid, Uuid, Uuid) {
        let agent_id = agent(pool, owner).await;
        RunnerStore::new(pool.clone())
            .assign_agent(owner, agent_id, Some(runner))
            .await
            .unwrap();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions (id, agent_id, role, backend, model, working_dir)
             VALUES ($1, $2, 'main', 'codex', 'test-model', $3)",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(format!("agents/{agent_id}"))
        .execute(pool)
        .await
        .unwrap();
        let run_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'turn', 'queued', 'test')",
        )
        .bind(run_id)
        .bind(session_id)
        .bind(agent_id)
        .execute(pool)
        .await
        .unwrap();
        (agent_id, session_id, run_id)
    }

    #[tokio::test]
    async fn claims_only_the_requested_kinds_of_work() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Lanes").await;
        let runner = registration.runner.id;
        let (_, chat_session, chat_run) = queued_run(&pool, owner, runner).await;
        let (_, _, work_run) = queued_run(&pool, owner, runner).await;
        sqlx::query("UPDATE session_runs SET kind = 'task' WHERE id = $1")
            .bind(work_run)
            .execute(&pool)
            .await
            .unwrap();
        let earlier = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt, created_at)
             SELECT $1, session_id, agent_id, 'turn', 'completed', 'before', created_at - INTERVAL '1 minute'
             FROM session_runs WHERE id = $2",
        )
        .bind(earlier)
        .bind(chat_run)
        .execute(&pool)
        .await
        .unwrap();
        let world = crate::world::WorldStore::new(pool.clone());
        assert_eq!(world.previous_run_id(chat_session, chat_run).await.unwrap(), Some(earlier));
        assert_eq!(world.previous_run_id(chat_session, earlier).await.unwrap(), None);

        let work_kinds = ["task", "followup", "review", "publish"].map(String::from);
        let claimed = store
            .claim_kinds(runner, &token("mcp-work"), &work_kinds)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.run_id, work_run);
        assert!(store
            .claim_kinds(runner, &token("mcp-work-2"), &work_kinds)
            .await
            .unwrap()
            .is_none());
        let claimed = store
            .claim_kinds(runner, &token("mcp-chat"), &["turn".to_string(), "aside".to_string()])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.run_id, chat_run);
    }

    #[tokio::test]
    async fn has_work_skips_what_claims_would_skip() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Idle").await.runner.id;
        let (agent_id, session_id, run_id) = queued_run(&pool, owner, runner).await;
        let run = |sql: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query(sql).bind(run_id).execute(&pool).await.unwrap() }
        };
        assert!(store.has_work(runner).await.unwrap());

        sqlx::query("UPDATE agent_profiles SET paused_reason = 'test' WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!store.has_work(runner).await.unwrap(), "paused agents are not claimed");
        sqlx::query("UPDATE agent_profiles SET paused_reason = NULL WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();

        run("UPDATE session_runs SET kind = 'followup' WHERE id = $1").await;
        assert!(!store.has_work(runner).await.unwrap(), "follow-ups need a CLI session");
        run("UPDATE session_runs SET kind = 'quick' WHERE id = $1").await;
        assert!(!store.has_work(runner).await.unwrap(), "older apps never see quick chats they cannot run");
        assert!(store.claim_kinds(runner, &token("quick-old"), &[]).await.unwrap().is_none());
        store.heartbeat(runner, &serde_json::json!({ "quick": true })).await.unwrap();
        assert!(store.has_work(runner).await.unwrap(), "quick chats wake runners that advertise them");
        run("UPDATE session_runs SET kind = 'turn' WHERE id = $1").await;

        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'turn', 'running', 'busy')",
        )
        .bind(Uuid::new_v4())
        .bind(session_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(!store.has_work(runner).await.unwrap(), "a busy session waits for its run");

        run("DELETE FROM session_runs WHERE id = $1").await;
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', '{}')")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!store.has_work(runner).await.unwrap(), "inbox waits while the main turn runs");
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE session_id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.has_work(runner).await.unwrap(), "inbox wakes once the main turn is done");
    }

    #[tokio::test]
    async fn pr_checks_follow_the_assigned_agents_owner() {
        let Some(pool) = pool().await else { return };
        let _state = global_state().lock().await;
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "PR checks").await.runner.id;
        store.heartbeat(runner, &json!({})).await.unwrap();
        let agent = agent(&pool, owner).await;
        store.assign_agent(owner, agent, Some(runner)).await.unwrap();
        let other_owner = user(&pool).await;
        let other = registration(&store, other_owner, "Other PR checks").await.runner.id;
        store.heartbeat(other, &json!({})).await.unwrap();
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, repo, status, assigned_agent_id, pr_url, pr_number)
             VALUES ($1, 'Watch PR', 'acme/app', 'failed', $2, 'https://github.com/acme/app/pull/12', 12)
             RETURNING id",
        )
        .bind(other_owner)
        .bind(agent)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1")
            .bind(task)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO task_milestones (task_id, title, kind, status, data, next_check_at)
             VALUES ($1, 'PR merged', 'merge', 'active', '{\"runner_next_check_at\": \"bad\"}', NOW() - INTERVAL '1 minute')",
        )
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
        assert!(store.claim_pr_check(other).await.unwrap().is_none());

        sqlx::query("UPDATE agent_profiles SET paused_reason = 'test' WHERE id = $1")
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.claim_pr_check(runner).await.unwrap().is_none());
        sqlx::query("UPDATE agent_profiles SET paused_reason = NULL WHERE id = $1")
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();

        let claimed = store.claim_pr_check(runner).await.unwrap().expect("owner runner claims the check");
        assert_eq!(claimed.task_id, task);
        assert_eq!(claimed.repo, "acme/app");
        assert_eq!(claimed.pr_url, "https://github.com/acme/app/pull/12");
        assert_eq!(claimed.pr_number, 12);
        let claimed_at = claimed.observed_at.expect("server claim time");
        let (local_due, runner_scheduled): (bool, bool) = sqlx::query_as(
            "SELECT next_check_at <= NOW(), jsonb_typeof(data->'runner_next_check_at') = 'number'
             FROM task_milestones WHERE task_id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(local_due, "runner claims do not advance the local merge watch");
        assert!(runner_scheduled, "runner claims keep their own schedule");
        assert!(store.claim_pr_check(runner).await.unwrap().is_none());

        let milestones = crate::milestones::MilestoneStore::new(pool.clone());
        assert!(milestones.arm_merge(task).await.unwrap());
        assert!(store.claim_pr_check(runner).await.unwrap().is_none(), "arming an active milestone preserves the runner schedule");
        assert!(milestones.check_now(task).await.unwrap());
        let runner_rearmed: bool = sqlx::query_scalar(
            "SELECT NOT (data ? 'runner_next_check_at') FROM task_milestones WHERE task_id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(runner_rearmed, "manual checks rearm the runner schedule");
        assert_eq!(store.claim_pr_check(runner).await.unwrap().expect("runner rearmed").task_id, task);

        sqlx::query("UPDATE task_milestones SET status = 'pending', next_check_at = NOW() - INTERVAL '1 minute' WHERE task_id = $1")
            .bind(task)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.claim_pr_check(runner).await.unwrap().is_none());
        assert!(milestones.arm_merge(task).await.unwrap());
        assert_eq!(store.claim_pr_check(runner).await.unwrap().expect("pending milestone rearmed").task_id, task);
        let world = crate::world::WorldStore::new(pool.clone());
        let pr_url = "https://github.com/acme/app/pull/12";
        assert!(world.settle_pr_from_runner(task, pr_url, false, runner).await.unwrap());
        let closed: (String, String) = sqlx::query_as(
            "SELECT t.status, m.status FROM agent_tasks t
             JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(closed, ("failed".into(), "failed".into()));
        assert!(milestones.retry_check_now(task).await.unwrap());
        assert!(!world
            .settle_pr_from_runner_observed(task, pr_url, false, runner, Some(claimed_at))
            .await
            .unwrap());
        assert!(!world
            .settle_pr_from_runner_observed(task, pr_url, false, runner, None)
            .await
            .unwrap());
        let active: String = sqlx::query_scalar("SELECT status FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(active, "active", "a delayed close from before the retry cannot stop the watch");
        let retried = store.claim_pr_check(runner).await.unwrap().expect("closed PR check retried");
        assert_eq!((retried.task_id, retried.repo.as_str(), retried.pr_url.as_str(), retried.pr_number), (task, "acme/app", pr_url, 12));
        assert!(world
            .settle_pr_from_runner_observed(task, pr_url, false, runner, None)
            .await
            .unwrap());
        let failed_again: String = sqlx::query_scalar("SELECT status FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(failed_again, "failed", "a fresh claim lets an old runner report the current close");
        assert!(milestones.retry_check_now(task).await.unwrap());
        let merged_claim = store.claim_pr_check(runner).await.unwrap().expect("merge check retried");
        assert_ne!(merged_claim.observed_at, Some(claimed_at));
        assert!(world
            .settle_pr_from_runner_observed(task, pr_url, true, runner, Some(claimed_at))
            .await
            .unwrap());
        let merged: (String, String) = sqlx::query_as(
            "SELECT t.status, m.status FROM agent_tasks t
             JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(merged, ("done".into(), "done".into()));
        sqlx::query("UPDATE task_milestones SET data = data - 'runner_next_check_at' WHERE task_id = $1")
            .bind(task)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE world_runners SET status = 'paused' WHERE id = $1")
            .bind(runner)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.claim_pr_check(runner).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn direct_runs_go_to_the_mac_that_holds_the_folder() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let stranger = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let home = registration(&store, owner, "Home").await.runner.id;
        let other = registration(&store, owner, "Studio").await.runner.id;
        let foreign = registration(&store, stranger, "Theirs").await.runner.id;
        let folder = Uuid::new_v4();
        store
            .heartbeat(other, &serde_json::json!({ "folders": [{ "id": folder, "name": "app", "path": "~/code/app" }] }))
            .await
            .unwrap();
        let agent_id = agent(&pool, owner).await;
        store.assign_agent(owner, agent_id, Some(home)).await.unwrap();
        let (found, name) = store.folder_device(owner, folder).await.unwrap().unwrap();
        assert_eq!((found.id, name.as_str()), (other, "app"));
        assert!(store.folder_device(stranger, folder).await.unwrap().is_none());

        let world = crate::world::WorldStore::new(pool.clone());
        let choice = crate::world::AsideChoice::default();
        let asked = serde_json::json!({ "kind": "user" });
        assert!(world.start_direct(agent_id, folder, foreign, "app", asked.clone(), &choice).await.is_err());
        let session = world.start_direct(agent_id, folder, other, "app", asked, &choice).await.unwrap();
        let run = world.start_followup(&session, "hello", "Owner", false).await.unwrap();
        let direct = ["direct".to_string()];
        assert!(store.claim_kinds(home, &token("mcp-home"), &direct).await.unwrap().is_none());
        let claimed = store.claim_kinds(other, &token("mcp-other"), &direct).await.unwrap().unwrap();
        assert_eq!(claimed.run_id, run.id);
        assert!(store.heartbeat_lease(other, claimed.lease_id).await.unwrap().is_some());
        assert!(store.lease_for_runner(other, claimed.lease_id).await.unwrap().is_some());
        store.expire_stale().await.unwrap();
        assert!(store.heartbeat_lease(other, claimed.lease_id).await.unwrap().is_some());
        let status: String = sqlx::query_scalar("SELECT status FROM agent_sessions WHERE id = $1")
            .bind(session.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn runner_tokens_are_hashed_and_owner_scoped() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Mac A").await;
        let stored =
            sqlx::query_scalar::<_, Vec<u8>>("SELECT token_hash FROM world_runners WHERE id = $1")
                .bind(registration.runner.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, token_hash(&registration.token));
        assert_ne!(stored, registration.token.as_bytes());
        assert!(store
            .authenticate(registration.runner.id, &registration.token)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .authenticate(registration.runner.id, "wrong")
            .await
            .unwrap()
            .is_none());
        assert!(store.list(other).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn affinity_rejects_cross_owner_and_history_moves() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let mine = registration(&store, owner, "Mine").await;
        let theirs = registration(&store, other, "Theirs").await;
        let agent_id = agent(&pool, owner).await;
        assert_eq!(
            store
                .assign_agent(owner, agent_id, Some(theirs.runner.id))
                .await,
            Err(AssignRunnerError::RunnerNotFound)
        );
        store
            .assign_agent(owner, agent_id, Some(mine.runner.id))
            .await
            .unwrap();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions (id, agent_id, role, backend, model, working_dir)
             VALUES ($1, $2, 'main', 'codex', 'test-model', $3)",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(format!("agents/{agent_id}"))
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            store.assign_agent(owner, agent_id, None).await,
            Err(AssignRunnerError::ActiveHistory)
        );
        let pinned =
            sqlx::query_scalar::<_, Uuid>("SELECT runner_id FROM agent_sessions WHERE id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pinned, mine.runner.id);
    }

    #[tokio::test]
    async fn handoff_archives_history_and_seeds_a_fresh_target_session() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let old = registration(&store, owner, "Old Mac").await;
        let target = registration(&store, owner, "Cloud").await;
        let agent_id = agent(&pool, owner).await;
        store
            .assign_agent(owner, agent_id, Some(old.runner.id))
            .await
            .unwrap();
        let old_session = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions (id, agent_id, role, backend, model, cli_session_id, working_dir)
             VALUES ($1, $2, 'main', 'codex', 'test-model', 'old-thread', $3)",
        )
        .bind(old_session)
        .bind(agent_id)
        .bind(format!("agents/{agent_id}"))
        .execute(&pool)
        .await
        .unwrap();
        let old_run = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'turn', 'completed', 'old prompt')",
        )
        .bind(old_run)
        .bind(old_session)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        let task_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, repo, status, assigned_agent_id)
             VALUES ($1, $2, 'Old work', 'owner/repo', 'done', $3)",
        )
        .bind(task_id)
        .bind(owner)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        let task_session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions
               (id, agent_id, role, backend, model, cli_session_id, task_id, working_dir, status)
             VALUES ($1, $2, 'task', 'codex', 'test-model', 'old-task', $3, '/tmp/old-work', 'completed')",
        )
        .bind(task_session_id)
        .bind(agent_id)
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
        let failed_run = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO session_runs
               (id, session_id, agent_id, task_id, kind, status, prompt, error)
             VALUES ($1, $2, $3, $4, 'task', 'failed', 'old task', 'failed')",
        )
        .bind(failed_run)
        .bind(task_session_id)
        .bind(agent_id)
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();

        store
            .handoff_agent(
                owner,
                agent_id,
                &HandoffRunner {
                    runner_id: target.runner.id,
                    cli: "claude".into(),
                    model: "claude-opus-5-5".into(),
                    summary: "Priya remembers the existing world.".into(),
                },
            )
            .await
            .unwrap();

        let old_row = sqlx::query_as::<_, (String, Uuid)>(
            "SELECT status, runner_id FROM agent_sessions WHERE id = $1",
        )
        .bind(old_session)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old_row, ("archived".into(), old.runner.id));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM session_runs WHERE id = $1")
                .bind(old_run)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "completed"
        );
        let fresh = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Uuid)>(
            "SELECT backend, model, cli_session_id, summary, runner_id FROM agent_sessions
             WHERE agent_id = $1 AND role = 'main' AND status = 'active'",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fresh.0, "claude");
        assert_eq!(fresh.1, "claude-opus-5-5");
        assert_eq!(fresh.2, None);
        assert_eq!(
            fresh.3.as_deref(),
            Some("Priya remembers the existing world.")
        );
        assert_eq!(fresh.4, target.runner.id);

        let world = crate::world::WorldStore::new(pool.clone());
        let task_session = world.get_session(task_session_id).await.unwrap().unwrap();
        assert!(world
            .start_followup(&task_session, "continue", "owner", false)
            .await
            .is_err());
        assert!(world
            .request_review(&task_session, None, None, None, owner)
            .await
            .is_err());
        assert_eq!(
            world
                .retry_run(failed_run, owner)
                .await
                .unwrap_err()
                .to_string(),
            "archived sessions cannot be retried"
        );
    }

    #[tokio::test]
    async fn pending_move_waits_for_the_turn_then_applies_and_can_return_to_cloud() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let mac = registration(&store, owner, "Desk Mac").await;
        let agent_id = agent(&pool, owner).await;
        sqlx::query(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, working_dir)
             VALUES ($1, 'main', 'codex', 'test-model', 'thread', 'agents/x')",
        )
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE agent_profiles SET inbox_claimed_by = 'worker-1' WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        let to_mac = AgentMove {
            runner_id: Some(mac.runner.id),
            cli: "claude".into(),
            model: "opus".into(),
            summary: "I remember everything.".into(),
            by: Some("Priya".into()),
        };
        store.queue_move(agent_id, &to_mac).await.unwrap();
        store.apply_pending_move(agent_id).await.unwrap();
        assert!(store.has_pending_move(agent_id).await.unwrap());
        assert!(store.agent_runner(owner, agent_id).await.unwrap().is_none());

        sqlx::query("UPDATE agent_profiles SET inbox_claimed_by = NULL WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        store.apply_pending_move(agent_id).await.unwrap();
        assert!(!store.has_pending_move(agent_id).await.unwrap());
        assert_eq!(store.agent_runner(owner, agent_id).await.unwrap().map(|r| r.id), Some(mac.runner.id));
        let (cli, model) = sqlx::query_as::<_, (String, String)>("SELECT cli, model FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((cli.as_str(), model.as_str()), ("claude", "opus"));

        let to_cloud = AgentMove { runner_id: None, cli: "codex".into(), ..to_mac.clone() };
        assert!(matches!(
            store.move_agent(owner, agent_id, &to_cloud, false).await,
            Err(HandoffRunnerError::InvalidRequest(CLOUD_OFFLINE))
        ));
        assert_eq!(store.agent_runner(owner, agent_id).await.unwrap().map(|r| r.id), Some(mac.runner.id));
        let worker = {
            use sqlx::ConnectOptions;
            let url = std::env::var("TEST_DATABASE_URL").unwrap();
            url.parse::<sqlx::postgres::PgConnectOptions>().unwrap().application_name(WORLD_WORKER_APP).connect().await.unwrap()
        };
        assert!(store.cloud_online().await.unwrap());
        assert!(store.move_agent(owner, agent_id, &to_cloud, false).await.unwrap());
        drop(worker);
        assert!(store.agent_runner(owner, agent_id).await.unwrap().is_none());
        let (active_runner, summary) = sqlx::query_as::<_, (Option<Uuid>, Option<String>)>(
            "SELECT runner_id, summary FROM agent_sessions WHERE agent_id = $1 AND status = 'active' AND role = 'main'",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_runner, None);
        assert_eq!(summary.as_deref(), Some("I remember everything."));

        let bad = AgentMove { runner_id: Some(mac.runner.id), cli: "opencode".into(), ..to_mac };
        store.queue_move(agent_id, &bad).await.unwrap();
        store.apply_pending_move(agent_id).await.unwrap();
        assert!(!store.has_pending_move(agent_id).await.unwrap());
        let failed = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'agent.move_failed'",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(failed, 1);
    }

    #[tokio::test]
    async fn handoff_serializes_against_followup_enqueues() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let old = registration(&store, owner, "Old worker").await;
        let target = registration(&store, owner, "New worker").await;
        let agent_id = agent(&pool, owner).await;
        store
            .assign_agent(owner, agent_id, Some(old.runner.id))
            .await
            .unwrap();
        let task_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_tasks (id, user_id, title, repo, status, assigned_agent_id)
             VALUES ($1, $2, 'Finished work', 'owner/repo', 'done', $3)",
        )
        .bind(task_id)
        .bind(owner)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions
               (id, agent_id, role, backend, model, cli_session_id, task_id, working_dir, status)
             VALUES ($1, $2, 'task', 'codex', 'test-model', 'task-thread', $3, '/tmp/work', 'completed')",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
        let session = crate::world::WorldStore::new(pool.clone())
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(session_id)
            .execute(&mut *blocker)
            .await
            .unwrap();

        let handoff_store = store.clone();
        let handoff = tokio::spawn(async move {
            handoff_store
                .handoff_agent(
                    owner,
                    agent_id,
                    &HandoffRunner {
                        runner_id: target.runner.id,
                        cli: "claude".into(),
                        model: "claude-opus-5-5".into(),
                        summary: "carry over".into(),
                    },
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let followup_pool = pool.clone();
        let followup = tokio::spawn(async move {
            crate::world::WorldStore::new(followup_pool)
                .start_followup(&session, "continue", "owner", false)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        blocker.rollback().await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), handoff)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let followup_error = tokio::time::timeout(std::time::Duration::from_secs(5), followup)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            followup_error
                .to_string()
                .starts_with("archived sessions cannot receive follow-ups. Resume the quest instead: start_task on task_id"),
            "{followup_error}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM session_runs WHERE session_id = $1 AND status IN ('queued', 'running')",
            )
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn handoff_rejects_busy_foreign_and_revoked_targets_without_changes() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let old = registration(&store, owner, "Old").await;
        let target = registration(&store, owner, "Target").await;
        let foreign = registration(&store, other, "Foreign").await;
        let revoked = registration(&store, owner, "Revoked").await;
        assert!(matches!(
            store.revoke(owner, revoked.runner.id).await.unwrap(),
            RevokeRunner::Revoked(_)
        ));
        let agent_id = agent(&pool, owner).await;
        store
            .assign_agent(owner, agent_id, Some(old.runner.id))
            .await
            .unwrap();
        sqlx::query("UPDATE agent_profiles SET inbox_claimed_by = 'worker' WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        let request = HandoffRunner {
            runner_id: target.runner.id,
            cli: "claude".into(),
            model: "opus".into(),
            summary: "memory".into(),
        };
        assert!(matches!(
            store.handoff_agent(owner, agent_id, &request).await,
            Err(HandoffRunnerError::Busy)
        ));
        sqlx::query("UPDATE agent_profiles SET inbox_claimed_by = NULL WHERE id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO agent_inbox (agent_id, kind, status) VALUES ($1, 'message', 'processing')",
        )
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            store.handoff_agent(owner, agent_id, &request).await,
            Err(HandoffRunnerError::Busy)
        ));
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .unwrap();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_sessions (id, agent_id, role, backend, model, working_dir)
             VALUES ($1, $2, 'main', 'codex', 'test-model', $3)",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(format!("agents/{agent_id}"))
        .execute(&pool)
        .await
        .unwrap();
        let run_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'turn', 'queued', 'waiting')",
        )
        .bind(run_id)
        .bind(session_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        for status in ["queued", "running"] {
            sqlx::query("UPDATE session_runs SET status = $2 WHERE id = $1")
                .bind(run_id)
                .bind(status)
                .execute(&pool)
                .await
                .unwrap();
            assert!(matches!(
                store.handoff_agent(owner, agent_id, &request).await,
                Err(HandoffRunnerError::Busy)
            ));
        }
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        for runner_id in [foreign.runner.id, revoked.runner.id] {
            let denied = HandoffRunner {
                runner_id,
                ..request.clone()
            };
            assert!(matches!(
                store.handoff_agent(owner, agent_id, &denied).await,
                Err(HandoffRunnerError::RunnerNotFound)
            ));
        }
        assert_eq!(
            sqlx::query_scalar::<_, Uuid>("SELECT runner_id FROM agent_profiles WHERE id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            old.runner.id
        );
    }

    #[tokio::test]
    async fn concurrent_claim_has_one_winner_and_binds_lease() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let primary = registration(&store, owner, "Claim Mac").await;
        let runner = primary.runner.id;
        let other = registration(&store, owner, "Other Mac").await;
        let (_, _, expected_run) = queued_run(&pool, owner, runner).await;
        assert!(store
            .claim(other.runner.id, &token("mcp-other"))
            .await
            .unwrap()
            .is_none());
        let first = store.clone();
        let second = store.clone();
        let (token_a, token_b) = (token("mcp-a"), token("mcp-b"));
        let (a, b) = tokio::join!(first.claim(runner, &token_a), second.claim(runner, &token_b));
        let (claim, mcp_token) = match (a.unwrap(), b.unwrap()) {
            (Some(claim), None) => (claim, &token("mcp-a")),
            (None, Some(claim)) => (claim, &token("mcp-b")),
            _ => panic!("exactly one claim must win"),
        };
        assert_eq!(claim.run_id, expected_run);
        assert!(store
            .heartbeat_lease(Uuid::new_v4(), claim.lease_id)
            .await
            .unwrap()
            .is_none());
        assert!(store.heartbeat_lease(runner, claim.lease_id).await.unwrap().is_some());
        let active = store.lease_for_mcp(mcp_token).await.unwrap();
        assert!(active.is_some());
    }

    #[tokio::test]
    async fn run_events_keep_order_and_limits() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Events Mac").await.runner.id;
        queued_run(&pool, owner, runner).await;
        let claim = store.claim(runner, &token("mcp-events")).await.unwrap().expect("claim");
        let lease = store.lease_for_runner(runner, claim.lease_id).await.unwrap().expect("lease");
        store.record_events(&lease, &[]).await.unwrap();
        let mut events: Vec<RunnerEvent> = (0..105)
            .map(|i| RunnerEvent { kind: if i % 2 == 0 { "tool".into() } else { "text".into() }, tool: (i % 2 == 0).then(|| format!("tool-{i}")), text: format!("step {i}") })
            .collect();
        events[1].text = "x".repeat(25_000);
        events[2].tool = Some("t".repeat(300));
        store.record_events(&lease, &events).await.unwrap();
        let rows = sqlx::query_as::<_, (String, Option<String>, String)>(
            "SELECT kind, tool, body FROM session_run_events WHERE run_id = $1 ORDER BY id",
        )
        .bind(lease.run_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0], ("tool".to_string(), Some("tool-0".to_string()), "step 0".to_string()));
        assert_eq!(rows[1].1, None);
        assert_eq!(rows[1].2.chars().count(), 20_000);
        assert_eq!(rows[2].1.as_deref().map(|t| t.chars().count()), Some(200));
        assert!(rows.iter().enumerate().skip(3).all(|(i, row)| row.2 == format!("step {i}")));
    }

    #[tokio::test]
    async fn lease_heartbeat_renews_and_returns_the_lease() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Beat Mac").await.runner.id;
        let other = registration(&store, owner, "Other Mac").await.runner.id;
        queued_run(&pool, owner, runner).await;
        let claim = store.claim(runner, &token("mcp-beat")).await.unwrap().expect("claim");
        sqlx::query("UPDATE runner_leases SET expires_at = NOW() + INTERVAL '5 seconds' WHERE id = $1")
            .bind(claim.lease_id)
            .execute(&pool)
            .await
            .unwrap();
        let expected = store.lease_for_runner(runner, claim.lease_id).await.unwrap().expect("lease");
        let renewed = store.heartbeat_lease(runner, claim.lease_id).await.unwrap().expect("renewed");
        assert_eq!(renewed.lease.run_id, expected.run_id);
        assert_eq!(renewed.lease.session_id, expected.session_id);
        assert_eq!(renewed.lease.agent_id, expected.agent_id);
        assert_eq!(renewed.lease.owner_user_id, expected.owner_user_id);
        assert_eq!(renewed.lease.kind, expected.kind);
        let (timeout, role) = sqlx::query_as::<_, (i32, String)>(
            "SELECT r.timeout_secs, s.role FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id WHERE r.id = $1",
        )
        .bind(expected.run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(renewed.timeout_secs, timeout);
        assert_eq!(renewed.session_role.as_deref(), Some(role.as_str()));
        let (lease_fresh, run_fresh) = sqlx::query_as::<_, (bool, bool)>(
            "SELECT l.expires_at > NOW() + interval '10 seconds', r.lease_expires_at > NOW() + interval '10 seconds'
             FROM runner_leases l JOIN session_runs r ON r.lease_id = l.id WHERE l.id = $1",
        )
        .bind(claim.lease_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(lease_fresh && run_fresh);
        assert!(store.heartbeat_lease(other, claim.lease_id).await.unwrap().is_none());
        assert_eq!(store.heartbeat_leases(runner, &[claim.lease_id, Uuid::new_v4()]).await.unwrap().len(), 1);
        assert!(store.heartbeat_leases(runner, &[]).await.unwrap().is_empty());
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1")
            .bind(expected.run_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.heartbeat_lease(runner, claim.lease_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn completion_is_idempotent_only_for_same_payload() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Complete Mac").await;
        let (_, session_id, _) = queued_run(&pool, owner, registration.runner.id).await;
        sqlx::query("UPDATE agent_sessions SET summary = 'handoff seed' WHERE id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .unwrap();
        let claim = store
            .claim(registration.runner.id, &token("mcp-complete"))
            .await
            .unwrap()
            .unwrap();
        let completion = completion(format!("agents/{}", claim.agent_id));
        let hash = token_hash("completion-a");
        assert_eq!(
            store
                .begin_completion(registration.runner.id, claim.lease_id, &hash, &completion)
                .await
                .unwrap(),
            BeginCompletion::Begun
        );
        let lease = store
            .lease_for_completion(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .apply_success(
                    &lease,
                    "done",
                    Some("cli-session"), Some(&Reported { cost_usd: 0.1, usage: RunnerUsage { input_tokens: 10, cached_input_tokens: 2, output_tokens: 3, context_tokens: 0 } }), Some(12))
                .await
                .unwrap(),
            ApplyCompletion::Applied
        );
        assert!(sqlx::query_scalar::<_, Option<String>>(
            "SELECT summary FROM agent_sessions WHERE id = $1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .is_none());
        assert!(store
            .finish_completion(registration.runner.id, claim.lease_id)
            .await
            .unwrap());
        assert_eq!(
            store
                .begin_completion(registration.runner.id, claim.lease_id, &hash, &completion)
                .await
                .unwrap(),
            BeginCompletion::AlreadyCompleted
        );
        assert_eq!(
            store
                .begin_completion(
                    registration.runner.id,
                    claim.lease_id,
                    &token_hash("completion-b"),
                    &completion,
                )
                .await
                .unwrap(),
            BeginCompletion::Conflict
        );
    }

    #[tokio::test]
    async fn agents_waiting_on_an_offline_device_are_reported_once() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Sleeping Mac").await;
        let runner = registration.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true } })).await.unwrap();
        let waiting = agent(&pool, owner).await;
        let quiet = agent(&pool, owner).await;
        let fresh = agent(&pool, owner).await;
        for a in [waiting, quiet, fresh] {
            sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(a).bind(runner).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload, created_at) VALUES ($1, 'task.assigned', '{}', NOW() - INTERVAL '10 minutes')")
            .bind(waiting).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload, created_at) VALUES ($1, 'feed.item', '{}', NOW() - INTERVAL '10 minutes')")
            .bind(quiet).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'task.assigned', '{}')")
            .bind(fresh).execute(&pool).await.unwrap();
        let mine = |rows: Vec<(Uuid, String)>| rows.into_iter().filter(|(id, _)| [waiting, quiet, fresh].contains(id)).collect::<Vec<_>>();
        assert!(mine(store.stalled_on_offline_device().await.unwrap()).is_empty(), "the device is online");
        sqlx::query("UPDATE world_runners SET last_seen_at = NOW() - INTERVAL '10 minutes' WHERE id = $1").bind(runner).execute(&pool).await.unwrap();
        assert_eq!(mine(store.stalled_on_offline_device().await.unwrap()), vec![(waiting, "Sleeping Mac".to_string())]);
        sqlx::query("UPDATE agent_profiles SET status = 'error' WHERE id = $1").bind(waiting).execute(&pool).await.unwrap();
        assert!(mine(store.stalled_on_offline_device().await.unwrap()).is_empty(), "already flagged");
    }

    #[tokio::test]
    async fn stranded_agents_move_to_an_online_runner() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Adopting Mac").await;
        let runner = registration.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true, "claude": false } })).await.unwrap();
        let fresh = agent(&pool, owner).await;
        let wrong_cli = agent(&pool, owner).await;
        sqlx::query("UPDATE agent_profiles SET cli = 'claude' WHERE id = $1").bind(wrong_cli).execute(&pool).await.unwrap();
        let busy = agent(&pool, owner).await;
        sqlx::query("INSERT INTO agent_sessions (agent_id, role, backend, model, status, cli_session_id) VALUES ($1, 'main', 'codex', 'test-model', 'active', 'old')")
            .bind(busy)
            .execute(&pool)
            .await
            .unwrap();
        let idle = agent(&pool, owner).await;
        for a in [fresh, wrong_cli, busy] {
            sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', '{}')").bind(a).execute(&pool).await.unwrap();
        }
        assert_eq!(store.runner_for_stranded(fresh).await.unwrap(), Some(runner));
        assert_eq!(store.runner_for_stranded(wrong_cli).await.unwrap(), None, "the Mac lacks that agent's CLI");
        let adopted = store.adopt_stranded(runner, None).await.unwrap();
        assert_eq!(adopted, vec![fresh], "only fresh agents with waiting messages and a matching CLI move");
        let placed: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT id, runner_id FROM agent_profiles WHERE id = ANY($1)")
            .bind(vec![fresh, wrong_cli, busy, idle])
            .fetch_all(&pool)
            .await
            .unwrap();
        for (id, runner_id) in placed {
            assert_eq!(runner_id, (id == fresh).then_some(runner), "agent {id}");
        }
        use sqlx::ConnectOptions;
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        let worker = url.parse::<sqlx::postgres::PgConnectOptions>().unwrap().application_name(WORLD_WORKER_APP).connect().await.unwrap();
        sqlx::query("UPDATE agent_profiles SET runner_id = NULL WHERE id = $1").bind(fresh).execute(&pool).await.unwrap();
        assert!(store.cloud_online().await.unwrap());
        assert!(store.adopt_stranded(runner, None).await.unwrap().is_empty(), "nothing moves while the cloud worker is online");
        drop(worker);
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![fresh, wrong_cli, busy, idle]).execute(&pool).await.unwrap();
    }

    async fn first_turn_is_claimed(pool: &PgPool, store: &RunnerStore, runner: Uuid, agent_id: Uuid, label: &str) {
        let session: Uuid = match sqlx::query_scalar("SELECT id FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active'")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .unwrap()
        {
            Some(existing) => existing,
            None => sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir) VALUES ($1, 'main', 'codex', 'test-model', $2) RETURNING id",
            )
            .bind(agent_id)
            .bind(format!("agents/{agent_id}"))
            .fetch_one(pool)
            .await
            .unwrap(),
        };
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'queued', '[memory handoff] first turn') RETURNING id",
        )
        .bind(session)
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(store.has_work(runner).await.unwrap(), "{label}: the device is woken for the first turn");
        let claimed = store.claim_kinds(runner, &token(label), &["turn".to_string()]).await.unwrap();
        assert_eq!(claimed.map(|c| c.run_id), Some(run), "{label}: the device claims the first turn");
    }

    #[tokio::test]
    async fn the_first_turn_of_a_new_or_forked_agent_runs_on_the_device_that_adopts_it() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "First turn Mac").await.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true } })).await.unwrap();
        let source = agent(&pool, owner).await;
        let world = crate::world::WorldStore::new(pool.clone());
        let forked = world.fork_agent(source, owner, &format!("fork-{}", Uuid::new_v4().simple()), "Fork", "Ana").await.unwrap().expect("fork").agent_id;
        sqlx::query("UPDATE agent_profiles SET runner_id = NULL WHERE id = $1").bind(forked).execute(&pool).await.unwrap();
        let with_session = agent(&pool, owner).await;
        sqlx::query("INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'codex', 'test-model', 'active')")
            .bind(with_session)
            .execute(&pool)
            .await
            .unwrap();
        let brand_new = agent(&pool, owner).await;
        let mut adopted = Vec::new();
        for id in [forked, with_session, brand_new] {
            sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', '{}')").bind(id).execute(&pool).await.unwrap();
            adopted.extend(store.adopt_stranded(runner, Some(id)).await.unwrap());
        }
        assert_eq!(adopted, vec![forked, with_session, brand_new], "all three are adopted by the device");
        first_turn_is_claimed(&pool, &store, runner, forked, "fork").await;
        first_turn_is_claimed(&pool, &store, runner, with_session, "session-first").await;
        first_turn_is_claimed(&pool, &store, runner, brand_new, "brand-new").await;
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![source, forked, with_session, brand_new]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn a_fork_gets_a_device_when_it_is_made() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Fork Mac").await.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true } })).await.unwrap();
        let source = agent(&pool, owner).await;
        let world = crate::world::WorldStore::new(pool.clone());
        let forked = world.fork_agent(source, owner, &format!("fork-{}", Uuid::new_v4().simple()), "Fork", "Ana").await.unwrap().expect("fork").agent_id;
        let placed: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM agent_profiles WHERE id = $1").bind(forked).fetch_one(&pool).await.unwrap();
        assert_eq!(placed, Some(runner), "a fork is placed like every other new agent");
        let session_device: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM agent_sessions WHERE agent_id = $1 AND role = 'main'").bind(forked).fetch_one(&pool).await.unwrap();
        assert_eq!(session_device, Some(runner), "its memory session lives on that device");
        let folder: Option<String> = sqlx::query_scalar("SELECT working_dir FROM agent_sessions WHERE agent_id = $1 AND role = 'main'").bind(forked).fetch_one(&pool).await.unwrap();
        assert_eq!(folder, Some(format!("agents/{forked}")), "and has the same home folder as every other main session");
        first_turn_is_claimed(&pool, &store, runner, forked, "placed-fork").await;
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![source, forked]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn the_migration_frees_first_turns_that_were_stranded_without_a_device() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Stranded Mac").await.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true } })).await.unwrap();
        let stranded = agent(&pool, owner).await;
        let started = agent(&pool, owner).await;
        for id in [stranded, started] {
            sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(id).bind(runner).execute(&pool).await.unwrap();
        }
        let mut sessions = Vec::new();
        for (id, cli_session) in [(stranded, None), (started, Some("cli-1"))] {
            let session: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, status, cli_session_id) VALUES ($1, 'main', 'codex', 'test-model', 'active', $2) RETURNING id",
            )
            .bind(id)
            .bind(cli_session)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE agent_sessions SET runner_id = NULL WHERE id = $1").bind(session).execute(&pool).await.unwrap();
            let run: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'queued', 'first turn') RETURNING id")
                .bind(session)
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            sessions.push((session, run));
        }
        assert!(store.claim_kinds(runner, &token("before"), &["turn".to_string()]).await.unwrap().is_none(), "stranded before the repair");
        sqlx::raw_sql(include_str!("../../migrations/149_agent_device_reaches_sessions.sql")).execute(&pool).await.unwrap();
        let claimed = store.claim_kinds(runner, &token("after"), &["turn".to_string()]).await.unwrap();
        assert_eq!(claimed.map(|c| c.run_id), Some(sessions[0].1), "the never-started session got its device");
        let untouched: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM agent_sessions WHERE id = $1").bind(sessions[1].0).fetch_one(&pool).await.unwrap();
        assert_eq!(untouched, None, "a session that already ran a CLI keeps its own history and device");
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![stranded, started]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn the_sweep_hands_the_device_to_first_turns_that_no_trigger_reached() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Sweep Mac").await.runner.id;
        store.heartbeat(runner, &json!({ "backends": { "codex": true } })).await.unwrap();
        let stranded = agent(&pool, owner).await;
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(stranded).bind(runner).execute(&pool).await.unwrap();
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'codex', 'test-model', 'active') RETURNING id",
        )
        .bind(stranded)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE agent_sessions SET runner_id = NULL WHERE id = $1").bind(session).execute(&pool).await.unwrap();
        let run: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'queued', 'first turn') RETURNING id")
            .bind(session)
            .bind(stranded)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(store.claim_kinds(runner, &token("before-sweep"), &["turn".to_string()]).await.unwrap().is_none());
        assert!(store.realign_session_devices().await.unwrap().contains(&stranded), "the sweep names the agent it repaired");
        let claimed = store.claim_kinds(runner, &token("after-sweep"), &["turn".to_string()]).await.unwrap();
        assert_eq!(claimed.map(|c| c.run_id), Some(run), "the first turn starts after the sweep");
        assert!(!store.realign_session_devices().await.unwrap().contains(&stranded), "a repaired agent is not touched again");
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(stranded).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn handing_a_device_to_queued_first_turns_wakes_that_device() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        if std::env::var("DATABASE_MAX_CONNECTIONS").as_deref() == Ok("1") {
            eprintln!("SKIPPED: listening for the wake needs a second connection, run on a real Postgres");
            return;
        }
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Wake Mac").await.runner.id;
        let waiting = agent(&pool, owner).await;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status) VALUES ($1, 'main', 'codex', 'test-model', 'active') RETURNING id",
        )
        .bind(waiting)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'queued', 'first turn')")
            .bind(session)
            .bind(waiting)
            .execute(&pool)
            .await
            .unwrap();
        let mut listener = sqlx::postgres::PgListener::connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
        listener.listen("runner_jobs").await.unwrap();
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(waiting).bind(runner).execute(&pool).await.unwrap();
        let wanted = runner.to_string();
        let woke = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if listener.recv().await.unwrap().payload() == wanted {
                    break;
                }
            }
        })
        .await;
        assert!(woke.is_ok(), "the device is woken the moment its agent's first turn becomes claimable");
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(waiting).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn closed_sessions_are_archived_folder_and_quick_chats_that_nothing_is_running() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let mine = agent(&pool, owner).await;
        let theirs = agent(&pool, other).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Session Mac").await.runner.id;
        let session = |agent: Uuid, role: &'static str, status: &'static str, secs: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, status, updated_at)
                     VALUES ($1, $2, 'claude', 'test-model', $3, NOW() - make_interval(secs => $4::double precision)) RETURNING id",
                )
                .bind(agent)
                .bind(role)
                .bind(status)
                .bind(secs as f64)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let day = 86_400;
        let archived_direct = session(mine, "direct", "archived", 2 * day).await;
        let archived_quick = session(mine, "quick", "archived", 2 * day).await;
        let archived_fresh = session(mine, "direct", "archived", 300).await;
        let active = session(mine, "direct", "active", day).await;
        let idle_direct = session(mine, "direct", "active", 4 * day).await;
        let idle_quick = session(mine, "quick", "active", 30 * day).await;
        let busy = session(mine, "quick", "archived", 30 * day).await;
        let archived_task = session(mine, "task", "archived", 30 * day).await;
        let foreign = session(theirs, "direct", "archived", 30 * day).await;
        let deleted = Uuid::new_v4();
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'quick', 'running', 'go')")
            .bind(busy)
            .bind(mine)
            .execute(&pool)
            .await
            .unwrap();
        let mut closed = store.closed_sessions(runner, &[archived_direct, archived_quick, archived_fresh, active, idle_direct, idle_quick, busy, archived_task, foreign, deleted]).await.unwrap();
        closed.sort();
        let mut expected = vec![archived_direct, archived_quick, idle_direct, deleted];
        expected.sort();
        assert_eq!(closed, expected, "archived folder and quick chats and unknown ones close after an hour, a folder session also after 3 idle days; never active recent ones, idle quick chats, running, fresh, task sessions or someone else's");
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![mine, theirs]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn an_old_temp_folder_name_resolves_only_to_one_quest_of_the_runner_owner() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let (mine, theirs) = (agent(&pool, owner).await, agent(&pool, other).await);
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Prefix Mac").await.runner.id;
        let with_prefix = |prefix: &str| Uuid::parse_str(&format!("{prefix}{}", Uuid::new_v4().simple().to_string().get(12..).unwrap())).unwrap();
        let task = |id: Uuid, agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO agent_tasks (id, user_id, title, assigned_agent_id, status) VALUES ($1, $2, 'Quest', $3, 'done')")
                    .bind(id)
                    .bind(owner)
                    .bind(agent)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let unique = Uuid::new_v4().simple().to_string()[..12].to_string();
        let shared = Uuid::new_v4().simple().to_string()[..12].to_string();
        let foreign = Uuid::new_v4().simple().to_string()[..12].to_string();
        let found = with_prefix(&unique);
        task(found, mine).await;
        task(with_prefix(&shared), mine).await;
        task(with_prefix(&shared), mine).await;
        task(with_prefix(&foreign), theirs).await;
        let asked = vec![unique.clone(), shared, foreign, "not-hex-at-all".into(), "abc".into()];
        assert_eq!(store.quests_by_prefix(runner, &asked).await.unwrap(), vec![(unique, found)], "two quests behind one name, another owner's quest and junk names match nothing");
        assert!(store.quests_by_prefix(runner, &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn closed_tasks_follow_grace_rules_and_disk_pressure() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let other = user(&pool).await;
        let mine = agent(&pool, owner).await;
        let theirs = agent(&pool, other).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Workspace Mac").await.runner.id;
        let task = |agent: Uuid, status: &'static str, secs: i64| {
            let pool = pool.clone();
            async move {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO agent_tasks (id, user_id, title, assigned_agent_id, status, updated_at)
                     VALUES ($1, $2, 'Quest', $3, $4, NOW() - make_interval(secs => $5::double precision))",
                )
                .bind(id)
                .bind(owner)
                .bind(agent)
                .bind(status)
                .bind(secs as f64)
                .execute(&pool)
                .await
                .unwrap();
                id
            }
        };
        let day = 86_400;
        let open = task(mine, "in_progress", 30 * day).await;
        let in_review = task(mine, "review", 30 * day).await;
        let done_fresh = task(mine, "done", day).await;
        let done_old = task(mine, "done", 4 * day).await;
        let failed_mid = task(mine, "failed", 5 * day).await;
        let failed_old = task(mine, "failed", 8 * day).await;
        let cancelled = task(mine, "cancelled", day).await;
        let cancelled_fresh = task(mine, "cancelled", 300).await;
        let busy = task(mine, "done", 30 * day).await;
        let foreign = task(theirs, "done", 30 * day).await;
        let deleted = Uuid::new_v4();
        let session: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'codex', 'test-model', $2) RETURNING id")
            .bind(mine)
            .bind(busy)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'followup', 'running', 'go')")
            .bind(session)
            .bind(mine)
            .bind(busy)
            .execute(&pool)
            .await
            .unwrap();
        let open_fresh = task(mine, "in_progress", 300).await;
        let open_busy = task(mine, "in_progress", 30 * day).await;
        let foreign_open = task(theirs, "in_progress", 30 * day).await;
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'followup', 'queued', 'go')")
            .bind(session)
            .bind(mine)
            .bind(open_busy)
            .execute(&pool)
            .await
            .unwrap();
        let mut idle = store.idle_tasks(runner, &[open, in_review, open_fresh, open_busy, foreign_open, done_old, cancelled, Uuid::new_v4()]).await.unwrap();
        idle.sort();
        let mut expected_idle = vec![open, in_review];
        expected_idle.sort();
        assert_eq!(idle, expected_idle, "open quests idle for hours qualify; fresh, running, queued, someone else's, closed and unknown ones do not");

        let all = [open, in_review, done_fresh, done_old, failed_mid, failed_old, cancelled, cancelled_fresh, busy, foreign, deleted];

        let got_ids = |tasks: &[ClosedTask]| tasks.iter().map(|task| task.id).collect::<Vec<Uuid>>();
        let routine: Vec<Uuid> = store.closed_tasks(runner, &all, false).await.unwrap().into_iter().map(|t| t.id).collect();
        let mut expected = vec![done_old, failed_old, cancelled, deleted];
        expected.sort();
        let mut got = routine.clone();
        got.sort();
        assert_eq!(got, expected, "done after 3 days, failed after 7, cancelled and deleted after an hour; never open, running, someone else's or closed minutes ago");

        let pressure = store.closed_tasks(runner, &all, true).await.unwrap();
        let mut got = got_ids(&pressure);
        got.sort();
        let mut expected = vec![done_fresh, done_old, failed_mid, failed_old, cancelled, deleted];
        expected.sort();
        assert_eq!(got, expected, "disk pressure drops the grace but not the open, running and foreign quests");
        let order: Vec<Uuid> = pressure.iter().take(4).map(|t| t.id).collect();
        assert_eq!(order, vec![deleted, failed_old, failed_mid, done_old], "unknown quests first, then the longest idle");

        assert!(!got_ids(&pressure).contains(&cancelled_fresh), "a quest that closed minutes ago is left alone even under disk pressure");

        assert!(!store.take_workspace_cleanup(runner).await.unwrap());
        sqlx::query("UPDATE world_runners SET clean_workspaces_at = NOW() WHERE id = $1").bind(runner).execute(&pool).await.unwrap();
        assert!(store.take_workspace_cleanup(runner).await.unwrap());
        assert!(!store.take_workspace_cleanup(runner).await.unwrap(), "a request is delivered once");

        let notice = DiskNotice { free_bytes: 3_000_000_000, freed_bytes: 40_000_000_000, removed: 12, still_low: false, ..Default::default() };
        assert!(store.warn_disk_low(runner, &notice).await.unwrap());
        assert!(!store.warn_disk_low(runner, &notice).await.unwrap(), "one notice per hour");
        let bell: (String, String) = sqlx::query_as(
            "SELECT n.kind, e.payload->>'message' FROM world_notifications n JOIN world_events e ON e.seq = n.event_seq
              WHERE n.user_id = $1 AND e.type = 'runner.disk_low'",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(bell.0, "alert");
        assert!(bell.1.contains("removed 12 closed quest folders (40.0 GB)"), "{}", bell.1);
        let second = registration(&store, owner, "Second Mac").await.runner.id;
        let kept = DiskNotice { still_low: true, kept: vec![DiskFolder { name: "quest abc".into(), bytes: 2_000_000_000, quest: Some(open) }], ..Default::default() };
        assert!(store.warn_disk_low(second, &kept).await.unwrap());
        let message: String = sqlx::query_scalar("SELECT payload->>'message' FROM world_events WHERE type = 'runner.disk_low' AND payload->>'runner_id' = $1")
            .bind(second.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(message.contains("build folders of \"Quest\" (2.0 GB)"), "a kept folder is named by its quest title: {message}");

        let report = crate::world::WorldStore::new(pool.clone());
        sqlx::query("UPDATE world_runners SET capabilities = jsonb_build_object('workspaces', jsonb_build_object('bytes', 9000000000, 'tasks', 3, 'free_gb', 2, 'total_gb', 460, 'low', true, 'largest', jsonb_build_array(jsonb_build_object('id', $2::text, 'bytes', 5000000000, 'temp_bytes', 4000000000), jsonb_build_object('id', $3::text, 'bytes', 3000000000, 'temp_bytes', 2000000000, 'build_bytes', 1000000000)), 'temp_bytes', 6000000000, 'build_bytes', 4000000000, 'seed_bytes', 1500000000)) WHERE id = $1")
            .bind(runner)
            .bind(done_old.to_string())
            .bind(open.to_string())
            .execute(&pool)
            .await
            .unwrap();
        let usage = report.workspace_use(owner).await.unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!((usage[0].bytes, usage[0].tasks, usage[0].reclaimable_bytes, usage[0].reclaimable_tasks, usage[0].low), (9_000_000_000, 3, 5_000_000_000, 1, true));
        assert_eq!((usage[0].temp_bytes, usage[0].reclaimable_temp_bytes), (6_000_000_000, 4_000_000_000), "only the closed quest's temp counts as reclaimable");
        let request = crate::cleanup::CleanupRequest { owner, org: None, agent: None, kind: crate::cleanup::CleanupKind::QuestFolders, older_than_days: 0, by_agent: None };
        let preview = report.clean_up(request, false).await.unwrap();
        assert_eq!((preview.rows, preview.bytes, preview.applied), (1, 5_000_000_000, false), "workspaces count closed quests only, never build caches");
        assert_eq!((usage[0].build_bytes, usage[0].seed_bytes, usage[0].reclaimable_cache_bytes), (4_000_000_000, 1_500_000_000, 5_500_000_000), "build output and the shared dependency caches are separate rebuildable caches");
        assert!(!store.take_workspace_cleanup(runner).await.unwrap(), "a preview asks nothing of the device");
        assert!(report.clean_up(request, true).await.unwrap().applied);
        assert!(store.take_workspace_cleanup(runner).await.unwrap());
        assert!(!store.take_cache_cleanup(runner).await.unwrap(), "clearing workspaces does not clear caches");
        let caches = crate::cleanup::CleanupRequest { kind: crate::cleanup::CleanupKind::Caches, ..request };
        let preview = report.clean_up(caches, false).await.unwrap();
        assert_eq!((preview.rows, preview.bytes, preview.applied), (1, 5_500_000_000, false), "one device, its build output plus shared caches");
        assert!(!store.take_cache_cleanup(runner).await.unwrap(), "a preview asks nothing of the device");
        assert!(report.clean_up(caches, true).await.unwrap().applied);
        assert!(store.take_cache_cleanup(runner).await.unwrap());
        assert!(!store.take_cache_cleanup(runner).await.unwrap(), "a request is delivered once");
        assert!(!store.take_workspace_cleanup(runner).await.unwrap(), "clearing caches does not remove workspaces");
        assert!(report.clean_up(crate::cleanup::CleanupRequest { agent: Some(mine), ..caches }, false).await.is_err());
        assert!(report.clean_up(crate::cleanup::CleanupRequest { agent: Some(mine), ..request }, false).await.is_err());

        sqlx::query("DELETE FROM world_cleanups WHERE user_id = $1").bind(owner).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![mine, theirs]).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM agent_tasks WHERE user_id = $1").bind(owner).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn failed_first_launch_keeps_handoff_seed_until_success() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Retry seed").await;
        let (agent_id, session_id, _) = queued_run(&pool, owner, registration.runner.id).await;
        sqlx::query("UPDATE agent_sessions SET summary = 'handoff seed' WHERE id = $1")
            .bind(session_id)
            .execute(&pool)
            .await
            .unwrap();
        let claim = store
            .claim(registration.runner.id, &token("mcp-seed-failure"))
            .await
            .unwrap()
            .unwrap();
        let failure_completion = completion(format!("agents/{}", claim.agent_id));
        assert_eq!(
            store
                .begin_completion(
                    registration.runner.id,
                    claim.lease_id,
                    &token_hash("seed-failure"),
                    &failure_completion,
                )
                .await
                .unwrap(),
            BeginCompletion::Begun
        );
        let lease = store
            .lease_for_completion(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .apply_failure(&lease, "launch failed", Some("failed-session"))
                .await
                .unwrap(),
            ApplyCompletion::Applied
        );
        assert_eq!(
            store
                .apply_failure(&lease, "launch failed again", Some("other-session"))
                .await
                .unwrap(),
            ApplyCompletion::AlreadyApplied
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT cli_session_id FROM agent_sessions WHERE id = $1",
            )
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .as_deref(),
            Some("failed-session")
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT summary FROM agent_sessions WHERE id = $1",
            )
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .as_deref(),
            Some("handoff seed")
        );
        assert!(store
            .finish_completion(registration.runner.id, claim.lease_id)
            .await
            .unwrap());

        sqlx::query(
            "INSERT INTO session_runs (id, session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'turn', 'queued', 'retry')",
        )
        .bind(Uuid::new_v4())
        .bind(session_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
        let retry = store
            .claim(registration.runner.id, &token("mcp-seed-retry"))
            .await
            .unwrap()
            .unwrap();
        let success_completion = completion(format!("agents/{}", retry.agent_id));
        assert_eq!(
            store
                .begin_completion(
                    registration.runner.id,
                    retry.lease_id,
                    &token_hash("seed-success"),
                    &success_completion,
                )
                .await
                .unwrap(),
            BeginCompletion::Begun
        );
        let lease = store
            .lease_for_completion(registration.runner.id, retry.lease_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .apply_success(
                    &lease,
                    "done",
                    Some("failed-session"), Some(&Reported { cost_usd: 0.1, usage: RunnerUsage { input_tokens: 10, cached_input_tokens: 2, output_tokens: 3, context_tokens: 0 } }), Some(12))
                .await
                .unwrap(),
            ApplyCompletion::Applied
        );
        assert_eq!(
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT cli_session_id, summary FROM agent_sessions WHERE id = $1",
            )
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            (Some("failed-session".into()), None)
        );
    }

    #[tokio::test]
    async fn expired_lease_fails_run_instead_of_moving_it() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Expired Mac").await;
        let (_, _, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        let claim = store
            .claim(registration.runner.id, &token("mcp-expire"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "UPDATE runner_leases SET expires_at = NOW() - INTERVAL '1 second' WHERE id = $1",
        )
        .bind(claim.lease_id)
        .execute(&pool)
        .await
        .unwrap();
        store.heartbeat(registration.runner.id, &json!({})).await.unwrap();
        assert_eq!(store.expire_stale().await.unwrap(), vec![run_id]);
        let status = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, error FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status.0, "failed");
        assert_eq!(status.1.as_deref(), Some("runner lease expired"));
        assert!(store
            .claim(registration.runner.id, &token("mcp-other"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn sleeping_mac_reclaims_its_lapsed_lease() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let other = registration(&store, owner, "Other Mac").await;
        let registration = registration(&store, owner, "Sleepy Mac").await;
        let (_, _, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        store.heartbeat(registration.runner.id, &json!({})).await.unwrap();
        let claim = store
            .claim(registration.runner.id, &token("mcp-sleep"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "UPDATE runner_leases SET expires_at = NOW() - INTERVAL '30 minutes' WHERE id = $1",
        )
        .bind(claim.lease_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE world_runners SET last_seen_at = NOW() - INTERVAL '31 minutes' WHERE id = $1")
            .bind(registration.runner.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store.expire_stale().await.unwrap().is_empty(), "a sleeping Mac keeps its lease");
        assert!(store
            .heartbeat_lease(other.runner.id, claim.lease_id)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
        store.heartbeat(registration.runner.id, &json!({})).await.unwrap();
        assert!(store.expire_stale().await.unwrap().is_empty(), "the renewed lease stays active");
        assert!(store.lease_for_mcp(&token("mcp-sleep")).await.unwrap().is_some());
        sqlx::query(
            "UPDATE runner_leases SET expires_at = NOW() - make_interval(secs => $2::double precision) WHERE id = $1",
        )
        .bind(claim.lease_id)
        .bind((RUNNER_SLEEP_GRACE_SECONDS + 1) as f64)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE world_runners SET last_seen_at = NULL WHERE id = $1")
            .bind(registration.runner.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(store.expire_stale().await.unwrap(), vec![run_id], "past the grace the lease expires");
        let error = sqlx::query_scalar::<_, Option<String>>("SELECT error FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(error.as_deref(), Some(RUNNER_ASLEEP_ERROR));
        assert!(store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unreachable_runner_keeps_its_lease_until_it_returns() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = store
            .register(owner, &CreateRunner { name: "Cloud runner".into(), platform: "linux".into() })
            .await
            .unwrap();
        let runner = registration.runner.id;
        let (_, _, run_id) = queued_run(&pool, owner, runner).await;
        store.heartbeat(runner, &json!({})).await.unwrap();
        let claim = store.claim(runner, &token("mcp-away")).await.unwrap().unwrap();
        let away = |lapsed_secs: f64, deadline_passed: bool| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "UPDATE runner_leases SET expires_at = NOW() - make_interval(secs => $2::double precision) WHERE id = $1",
                )
                .bind(claim.lease_id)
                .bind(lapsed_secs)
                .execute(&pool)
                .await
                .unwrap();
                sqlx::query("UPDATE world_runners SET last_seen_at = NOW() - make_interval(secs => $2::double precision) WHERE id = $1")
                    .bind(runner)
                    .bind(lapsed_secs + 30.0)
                    .execute(&pool)
                    .await
                    .unwrap();
                if deadline_passed {
                    sqlx::query("UPDATE session_runs SET started_at = NOW() - INTERVAL '2 hours', timeout_secs = 3600 WHERE id = $1")
                        .bind(run_id)
                        .execute(&pool)
                        .await
                        .unwrap();
                }
            }
        };
        away(600.0, true).await;
        assert!(store.expire_stale().await.unwrap().is_empty(), "an unreachable runner keeps its lease past the run deadline");
        assert!(store.lease_for_runner(runner, claim.lease_id).await.unwrap().is_some(), "queued events still land");
        assert!(store.lease_for_submission(runner, claim.lease_id).await.unwrap().is_some(), "a completion queued before the deadline still lands");
        assert!(store.heartbeat_lease(runner, claim.lease_id).await.unwrap().is_none(), "no renewals past the deadline");
        store.heartbeat(runner, &json!({})).await.unwrap();
        assert_eq!(store.expire_stale().await.unwrap(), vec![run_id], "once the runner is back without finishing, the deadline applies");
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "timed_out");

        let (_, _, second) = queued_run(&pool, owner, runner).await;
        let claim = store.claim(runner, &token("mcp-away-2")).await.unwrap().unwrap();
        sqlx::query(
            "UPDATE runner_leases SET expires_at = NOW() - make_interval(secs => $2::double precision) WHERE id = $1",
        )
        .bind(claim.lease_id)
        .bind((RUNNER_OUTAGE_GRACE_SECONDS + 1) as f64)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE world_runners SET last_seen_at = NOW() - INTERVAL '2 hours' WHERE id = $1")
            .bind(runner)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(store.expire_stale().await.unwrap(), vec![second], "past the outage grace the lease expires");
        let error = sqlx::query_scalar::<_, Option<String>>("SELECT error FROM session_runs WHERE id = $1")
            .bind(second)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(error.as_deref(), Some(RUNNER_UNREACHABLE_ERROR));
    }

    #[tokio::test]
    async fn run_deadline_stops_heartbeats_and_marks_timeout() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Timed Mac").await;
        let (_, _, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        let claim = store
            .claim(registration.runner.id, &token("mcp-timeout"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "UPDATE session_runs SET started_at = NOW() - INTERVAL '2 seconds', timeout_secs = 1 WHERE id = $1",
        )
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .is_none());
        assert!(store.lease_for_mcp(&token("mcp-timeout")).await.unwrap().is_none());
        assert_eq!(store.expire_stale().await.unwrap(), vec![run_id]);
        let status = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, error FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status.0, "timed_out");
        assert_eq!(status.1.as_deref(), Some("runner job timed out"));
    }

    #[tokio::test]
    async fn pause_cancels_the_active_lease() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Paused Mac").await;
        let (_, _, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        let claim = store
            .claim(registration.runner.id, &token("mcp-pause"))
            .await
            .unwrap()
            .unwrap();
        store
            .update(
                owner,
                registration.runner.id,
                &UpdateRunner {
                    name: None,
                    paused: Some(true),
                    screen_access: None,
                    app_control: None,
                },
            )
            .await
            .unwrap();
        assert!(store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .is_none());
        let status =
            sqlx::query_scalar::<_, String>("SELECT status FROM session_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
        assert!(store.lease_for_mcp(&token("mcp-pause")).await.unwrap().is_none());
        assert_eq!(
            store
                .heartbeat(registration.runner.id, &json!({ "cli": ["codex"] }))
                .await
                .unwrap()
                .unwrap()
                .status,
            "paused"
        );
    }

    #[tokio::test]
    async fn owner_role_downgrade_stops_all_runner_credentials() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Downgraded Mac").await;
        let (_, _, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        let claim = store
            .claim(registration.runner.id, &token("mcp-role"))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE users SET world_role = 'read' WHERE id = $1")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        assert!(store
            .authenticate(registration.runner.id, &registration.token)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .is_none());
        assert!(store.lease_for_mcp(&token("mcp-role")).await.unwrap().is_none());
        assert_eq!(store.expire_stale().await.unwrap(), vec![run_id]);
        let status = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, error FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status.0, "failed");
        assert_eq!(
            status.1.as_deref(),
            Some("runner authorization was revoked")
        );
    }

    #[tokio::test]
    async fn completion_and_pause_cannot_double_apply_accounting() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Race Mac").await;
        let (_, session_id, run_id) = queued_run(&pool, owner, registration.runner.id).await;
        let claim = store
            .claim(registration.runner.id, &token("mcp-race"))
            .await
            .unwrap()
            .unwrap();
        let completion = completion(format!("agents/{}", claim.agent_id));
        let hash = token_hash("completion-race");
        assert_eq!(
            store
                .begin_completion(registration.runner.id, claim.lease_id, &hash, &completion)
                .await
                .unwrap(),
            BeginCompletion::Begun
        );
        let lease = store
            .lease_for_completion(registration.runner.id, claim.lease_id)
            .await
            .unwrap()
            .unwrap();
        let apply = store.clone();
        let pause = store.clone();
        let (applied, paused) = tokio::join!(
            apply.apply_success(
                &lease,
                "done",
                Some("cli-session"), Some(&Reported { cost_usd: 0.1, usage: RunnerUsage { input_tokens: 10, cached_input_tokens: 2, output_tokens: 3, context_tokens: 0 } }), Some(12)),
            pause.update(
                owner,
                registration.runner.id,
                &UpdateRunner {
                    name: None,
                    paused: Some(true),
                    screen_access: None,
                    app_control: None,
                },
            ),
        );
        assert!(paused.unwrap().0.is_some());
        assert!(matches!(
            applied.unwrap(),
            ApplyCompletion::Applied | ApplyCompletion::Stopped
        ));
        let run = sqlx::query_as::<_, (String, f64, i64, i64, i64)>(
            "SELECT status, cost_usd::float8, input_tokens, cached_input_tokens, output_tokens
             FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(matches!(run.0.as_str(), "completed" | "failed"));
        if run.0 == "completed" {
            assert_eq!((run.1, run.2, run.3, run.4), (0.1, 10, 2, 3));
            let session_baseline = sqlx::query_scalar::<_, Value>(
                "SELECT usage_baseline FROM agent_sessions WHERE id = $1",
            )
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(session_baseline, json!({ "input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 3, "cost_usd": 0.1 }));
            assert!(store
                .lease_for_completion(registration.runner.id, claim.lease_id)
                .await
                .unwrap()
                .is_some());
            assert!(store
                .finish_completion(registration.runner.id, claim.lease_id)
                .await
                .unwrap());
        } else {
            assert_eq!((run.1, run.2, run.3, run.4), (0.0, 0, 0, 0));
        }
        let reapplied = store
            .apply_success(&lease, "done", None, Some(&Reported { cost_usd: 0.1, usage: RunnerUsage { input_tokens: 10, cached_input_tokens: 2, output_tokens: 3, context_tokens: 0 } }), Some(12))
            .await
            .unwrap();
        assert!(matches!(
            reapplied,
            ApplyCompletion::AlreadyApplied | ApplyCompletion::Stopped
        ));
    }

    #[tokio::test]
    async fn assignment_and_revoke_never_leave_a_revoked_runner_pinned() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let registration = registration(&store, owner, "Assignment race").await;
        let agent_id = agent(&pool, owner).await;
        let assign = store.clone();
        let revoke = store.clone();
        let (assigned, revoked) = tokio::join!(
            assign.assign_agent(owner, agent_id, Some(registration.runner.id)),
            revoke.revoke(owner, registration.runner.id),
        );
        assert!(matches!(
            (&assigned, &revoked),
            (Ok(()), Ok(RevokeRunner::InUse))
                | (Err(AssignRunnerError::RunnerNotFound), Ok(RevokeRunner::Revoked(_)))
        ));
        let invalid = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_profiles a
               JOIN world_runners r ON r.id = a.runner_id
               WHERE a.id = $1 AND r.status = 'revoked')",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!invalid);
    }

    const NO_SPACE: &str = "No space left on device: this device has only 1.0 GB free, so Loka did not start this run. Free up space on the device and retry.";

    async fn work_run(pool: &PgPool, owner: Uuid, runner: Uuid, kind: &str) -> (Uuid, Uuid, Uuid) {
        let (agent_id, session_id, run_id) = queued_run(pool, owner, runner).await;
        sqlx::query("UPDATE session_runs SET kind = $2 WHERE id = $1").bind(run_id).bind(kind).execute(pool).await.unwrap();
        sqlx::query("UPDATE agent_sessions SET cli_session_id = 'cli-session' WHERE id = $1").bind(session_id).execute(pool).await.unwrap();
        (agent_id, session_id, run_id)
    }

    async fn refuse_at_start(store: &RunnerStore, runner: Uuid, error: &str) -> (ClaimedRun, bool) {
        let claim = store.claim_kinds(runner, &token(&format!("mcp-space-{}", Uuid::new_v4())), &WORK_RUN_KINDS.map(String::from)).await.unwrap().unwrap();
        let mut refused = completion(format!("agents/{}", claim.agent_id));
        refused.exit_code = 1;
        refused.error = Some(error.to_string());
        let hash = token_hash(&format!("space-{}", claim.lease_id));
        assert_eq!(store.begin_completion(runner, claim.lease_id, &hash, &refused).await.unwrap(), BeginCompletion::Begun);
        let lease = store.lease_for_completion(runner, claim.lease_id).await.unwrap().unwrap();
        let requeued = store.requeue_for_space(&lease, error).await.unwrap();
        if requeued {
            assert!(store.finish_completion(runner, claim.lease_id).await.unwrap(), "the lease settles once the run waits");
        }
        (claim, requeued)
    }

    async fn run_state(pool: &PgPool, run: Uuid) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, waiting_for, error FROM session_runs WHERE id = $1").bind(run).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn every_run_kind_refused_for_lack_of_disk_waits_and_starts_when_there_is_room() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Full Mac").await.runner.id;
        for kind in WORK_RUN_KINDS {
            sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE runner_id = $1 AND status = 'queued'").bind(runner).execute(&pool).await.unwrap();
            let (_, _, run) = work_run(&pool, owner, runner, kind).await;
            let (_, requeued) = refuse_at_start(&store, runner, NO_SPACE).await;
            assert!(requeued, "{kind} waits instead of failing");
            assert_eq!(run_state(&pool, run).await, ("queued".to_string(), Some("disk".to_string()), None), "{kind}");
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE run_id = $1 AND type = 'run.waiting' AND payload->>'waiting' = 'true'")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(waiting, 1, "{kind} is announced once");
            assert!(
                store.claim_kinds(runner, &token(&format!("mcp-early-{}", Uuid::new_v4())), &WORK_RUN_KINDS.map(String::from)).await.unwrap().is_none(),
                "{kind} does not restart in a tight loop"
            );
            sqlx::query("UPDATE session_runs SET not_before = NOW() - INTERVAL '1 second' WHERE id = $1").bind(run).execute(&pool).await.unwrap();
            let again = store.claim_kinds(runner, &token(&format!("mcp-later-{}", Uuid::new_v4())), &WORK_RUN_KINDS.map(String::from)).await.unwrap().expect("starts once there is room");
            assert_eq!(again.run_id, run, "{kind} is the same run, not a copy");
            assert_eq!(run_state(&pool, run).await, ("running".to_string(), None, None), "{kind}");
        }
    }

    #[tokio::test]
    async fn a_waiting_run_still_starts_when_the_heartbeat_or_a_crash_comes_before_the_completion_settles() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Racy").await.runner.id;
        let kinds = WORK_RUN_KINDS.map(String::from);

        let (_, _, run) = work_run(&pool, owner, runner, "task").await;
        let claim = store.claim_kinds(runner, &token(&format!("mcp-race-{}", Uuid::new_v4())), &kinds).await.unwrap().unwrap();
        let mut refused = completion(format!("agents/{}", claim.agent_id));
        refused.exit_code = 1;
        refused.error = Some(NO_SPACE.to_string());
        let hash = token_hash(&format!("race-{}", claim.lease_id));
        assert_eq!(store.begin_completion(runner, claim.lease_id, &hash, &refused).await.unwrap(), BeginCompletion::Begun);
        let lease = store.lease_for_completion(runner, claim.lease_id).await.unwrap().unwrap();
        assert!(store.requeue_for_space(&lease, NO_SPACE).await.unwrap());
        let lease_status: String = sqlx::query_scalar("SELECT status FROM runner_leases WHERE id = $1").bind(claim.lease_id).fetch_one(&pool).await.unwrap();
        assert_eq!(lease_status, "completed", "the lease closes in the same transaction that queues the run again");

        assert_eq!(store.set_disk_wait(runner, false).await.unwrap(), 1, "a heartbeat with room clears the waiting mark before the completion settles");
        assert!(store.finish_completion(runner, claim.lease_id).await.unwrap(), "settling the completion afterwards is still fine");
        assert!(store.finish_completion(runner, claim.lease_id).await.unwrap(), "and so is settling it twice");
        sqlx::query("UPDATE session_runs SET not_before = NULL WHERE id = $1").bind(run).execute(&pool).await.unwrap();
        let again = store.claim_kinds(runner, &token(&format!("mcp-race-2-{}", Uuid::new_v4())), &kinds).await.unwrap().expect("the run starts");
        assert_eq!(again.run_id, run);

        let (_, _, crashed) = work_run(&pool, owner, runner, "followup").await;
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1").bind(run).execute(&pool).await.unwrap();
        let claim = store.claim_kinds(runner, &token(&format!("mcp-race-3-{}", Uuid::new_v4())), &kinds).await.unwrap().unwrap();
        assert_eq!(claim.run_id, crashed);
        let hash = token_hash(&format!("race-{}", claim.lease_id));
        assert_eq!(store.begin_completion(runner, claim.lease_id, &hash, &refused).await.unwrap(), BeginCompletion::Begun);
        let lease = store.lease_for_completion(runner, claim.lease_id).await.unwrap().unwrap();
        assert!(store.requeue_for_space(&lease, NO_SPACE).await.unwrap());
        sqlx::query("UPDATE session_runs SET not_before = NULL WHERE id = $1").bind(crashed).execute(&pool).await.unwrap();
        assert!(store.has_work(runner).await.unwrap(), "a crash before the completion settles leaves nothing blocking the session");
        let restarted = store.claim_kinds(runner, &token(&format!("mcp-race-4-{}", Uuid::new_v4())), &kinds).await.unwrap().expect("the run starts after the crash");
        assert_eq!(restarted.run_id, crashed);
        assert_eq!(store.begin_completion(runner, claim.lease_id, &hash, &refused).await.unwrap(), BeginCompletion::Conflict, "the runner's retry of the old completion is stale once the run started again");
    }

    #[tokio::test]
    async fn only_runs_that_never_started_and_ran_out_of_disk_wait() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Mixed").await.runner.id;

        let (_, _, other) = work_run(&pool, owner, runner, "task").await;
        let (_, requeued) = refuse_at_start(&store, runner, "Claude Code exited with code 1: spawn ENOENT").await;
        assert!(!requeued, "other errors fail as before");
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1").bind(other).execute(&pool).await.unwrap();

        let (_, _, midway) = work_run(&pool, owner, runner, "task").await;
        sqlx::query("INSERT INTO session_run_events (run_id, kind, body) VALUES ($1, 'tool', 'cargo build')").bind(midway).execute(&pool).await.unwrap();
        let (_, requeued) = refuse_at_start(&store, runner, NO_SPACE).await;
        assert!(!requeued, "a run that already took steps is not a start refusal");
        sqlx::query("UPDATE session_runs SET status = 'cancelled' WHERE id = $1").bind(midway).execute(&pool).await.unwrap();

        let (agent, _, cancelled) = work_run(&pool, owner, runner, "task").await;
        let task = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks (id, user_id, title, repo, status, assigned_agent_id) VALUES ($1, $2, 'Gone', 'owner/repo', 'cancelled', $3)")
            .bind(task)
            .bind(owner)
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE session_runs SET task_id = $2 WHERE id = $1").bind(cancelled).bind(task).execute(&pool).await.unwrap();
        let (_, requeued) = refuse_at_start(&store, runner, NO_SPACE).await;
        assert!(!requeued, "a cancelled quest never waits for room");
    }

    #[tokio::test]
    async fn the_heartbeat_marks_waiting_runs_once_and_releases_them_when_space_returns() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Low disk").await.runner.id;
        let mut work = Vec::new();
        for kind in WORK_RUN_KINDS {
            work.push(work_run(&pool, owner, runner, kind).await.2);
        }
        let (_, _, chat) = queued_run(&pool, owner, runner).await;

        assert_eq!(store.set_disk_wait(runner, true).await.unwrap(), WORK_RUN_KINDS.len(), "every work kind waits");
        assert_eq!(store.set_disk_wait(runner, true).await.unwrap(), 0, "each following heartbeat is one cheap check with nothing to announce");
        for run in &work {
            assert_eq!(run_state(&pool, *run).await.1.as_deref(), Some("disk"));
        }
        assert_eq!(run_state(&pool, chat).await.1, None, "chat turns still run on a full disk");
        let announced: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE run_id = ANY($1) AND type = 'run.waiting'")
            .bind(&work)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(announced, WORK_RUN_KINDS.len() as i64, "one announcement per run, not per heartbeat");

        assert_eq!(store.set_disk_wait(runner, false).await.unwrap(), WORK_RUN_KINDS.len(), "space is back, so all of them are released");
        for run in &work {
            assert_eq!(run_state(&pool, *run).await, ("queued".to_string(), None, None));
        }
        assert_eq!(store.set_disk_wait(runner, false).await.unwrap(), 0);

        let first = store.claim_kinds(runner, &token("mcp-slot-1"), &WORK_RUN_KINDS.map(String::from)).await.unwrap().unwrap();
        let second = store.claim_kinds(runner, &token("mcp-slot-2"), &WORK_RUN_KINDS.map(String::from)).await.unwrap().unwrap();
        assert_ne!(first.run_id, second.run_id, "one start per claim, one claim per free slot");
        assert_eq!(store.set_disk_wait(runner, true).await.unwrap(), 2, "the runs still queued wait again, the ones that started do not");
    }

    #[tokio::test]
    async fn cancelling_a_quest_that_waits_for_disk_space_drops_its_run() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Cancel").await.runner.id;
        let (agent, session, run) = work_run(&pool, owner, runner, "task").await;
        let task = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_tasks (id, user_id, title, repo, status, assigned_agent_id) VALUES ($1, $2, 'Waiting', 'owner/repo', 'in_progress', $3)")
            .bind(task)
            .bind(owner)
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE session_runs SET task_id = $2 WHERE id = $1").bind(run).bind(task).execute(&pool).await.unwrap();
        sqlx::query("UPDATE agent_sessions SET task_id = $2, role = 'task' WHERE id = $1").bind(session).bind(task).execute(&pool).await.unwrap();
        let (_, requeued) = refuse_at_start(&store, runner, NO_SPACE).await;
        assert!(requeued);
        crate::world::WorldStore::new(pool.clone()).cancel_task(task, "Test", None).await.unwrap();
        assert_eq!(run_state(&pool, run).await.0, "cancelled");
        assert!(store.claim_kinds(runner, &token("mcp-cancelled"), &WORK_RUN_KINDS.map(String::from)).await.unwrap().is_none());
        assert_eq!(store.set_disk_wait(runner, true).await.unwrap(), 0, "a cancelled run is never marked waiting again");
    }

    #[tokio::test]
    async fn runs_that_failed_on_a_full_disk_go_back_in_the_queue() {
        let _global = global_state().lock().await;
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let world = crate::world::WorldStore::new(pool.clone());
        let runner = registration(&store, owner, "Stranded").await.runner.id;
        let failed = |session_role: &'static str| {
            let pool = pool.clone();
            async move {
                let (agent, session, run) = work_run(&pool, owner, runner, "task").await;
                let task = Uuid::new_v4();
                sqlx::query("INSERT INTO agent_tasks (id, user_id, title, repo, status, assigned_agent_id, error_message) VALUES ($1, $2, 'Stranded', 'owner/repo', 'failed', $3, 'No space left on device')")
                    .bind(task)
                    .bind(owner)
                    .bind(agent)
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("UPDATE agent_sessions SET task_id = $2, role = $3, status = 'failed' WHERE id = $1").bind(session).bind(task).bind(session_role).execute(&pool).await.unwrap();
                sqlx::query(
                    "UPDATE session_runs SET task_id = $2, status = 'failed', error = 'No space left on device', finished_at = NOW(), cause = 'cli_error',
                       diagnosis = '{\"cause\":\"cli_error\",\"failure\":\"disk_full\",\"stage\":\"start\",\"reason\":\"disk\",\"turns\":0}'::jsonb
                     WHERE id = $1",
                )
                .bind(run)
                .bind(task)
                .execute(&pool)
                .await
                .unwrap();
                (task, session, run)
            }
        };
        let (task, session, run) = failed("task").await;
        let (_, _, midway) = failed("task").await;
        sqlx::query("UPDATE session_runs SET diagnosis = jsonb_set(diagnosis, '{turns}', '5') WHERE id = $1").bind(midway).execute(&pool).await.unwrap();
        let (_, _, stepped) = failed("task").await;
        sqlx::query("INSERT INTO session_run_events (run_id, kind, body) VALUES ($1, 'tool', 'cargo build')").bind(stepped).execute(&pool).await.unwrap();
        let (_, _, later_stage) = failed("task").await;
        sqlx::query("UPDATE session_runs SET diagnosis = jsonb_set(diagnosis, '{stage}', '\"run\"') WHERE id = $1").bind(later_stage).execute(&pool).await.unwrap();
        let (_, _, handled) = failed("task").await;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
             SELECT session_id, agent_id, task_id, 'followup', 'completed', 'later' FROM session_runs WHERE id = $1",
        )
        .bind(handled)
        .execute(&pool)
        .await
        .unwrap();

        let resumed = world.resume_disk_stranded().await.unwrap();
        assert!(resumed.contains(&run), "a run refused at start on a full disk is put back");
        assert!(!resumed.contains(&midway), "a run that took steps was not a start refusal");
        assert!(!resumed.contains(&stepped), "a run with tool steps is not restarted from scratch");
        assert!(!resumed.contains(&later_stage), "a run that got past the start stage is left alone");
        assert!(!resumed.contains(&handled), "a quest somebody already restarted is left alone");
        assert_eq!(run_state(&pool, run).await, ("queued".to_string(), Some("disk".to_string()), None));
        let (quest, error): (String, Option<String>) = sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await.unwrap();
        assert_eq!((quest.as_str(), error), ("in_progress", None));
        let status: String = sqlx::query_scalar("SELECT status FROM agent_sessions WHERE id = $1").bind(session).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "active");
        assert!(world.resume_disk_stranded().await.unwrap().is_empty(), "a second start finds nothing left to do");
    }

    #[tokio::test]
    async fn one_writer_holds_a_quest_folder_on_a_runner() {
        let Some(pool) = pool().await else { return };
        let owner = user(&pool).await;
        let store = RunnerStore::new(pool.clone());
        let runner = registration(&store, owner, "Folder Mac").await.runner.id;
        let (agent, _, turn) = queued_run(&pool, owner, runner).await;
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(turn).execute(&pool).await.unwrap();
        let tag = Uuid::new_v4();
        let session = |role: &'static str, dir: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, working_dir)
                     VALUES ($1, $2, 'codex', 'test-model', 'cli', $3) RETURNING id",
                )
                .bind(agent)
                .bind(role)
                .bind(dir)
                .fetch_one(&pool)
                .await
                .unwrap()
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
                .unwrap()
            }
        };
        let work_kinds = ["task", "followup", "review", "publish"].map(String::from);
        let claim_all = || async {
            let mut claimed = Vec::new();
            while let Some(run) = store.claim_kinds(runner, &token(&format!("folder-{}", Uuid::new_v4())), &work_kinds).await.unwrap() {
                claimed.push(run);
            }
            claimed
        };
        let writer = session("task", format!("tasks/{tag}/world")).await;
        let resumed = session("task", format!("tasks/{tag}/world")).await;
        let review = session("review", format!("tasks/{tag}/world")).await;
        let part = session("task", format!("tasks/{tag}-part/world")).await;

        let first = queue(writer, "task").await;
        let second = queue(resumed, "followup").await;
        let reviewing = queue(review, "review").await;
        let stacked = queue(part, "task").await;
        let started: Vec<Uuid> = claim_all().await.iter().map(|run| run.run_id).collect();
        assert!(started.contains(&first) && started.contains(&reviewing) && started.contains(&stacked), "{started:?}");
        assert!(!started.contains(&second), "a second writer waits for the folder on the device");
        assert!(!store.has_work(runner).await.unwrap(), "has_work skips the writer the claim skips");

        let mut listener = sqlx::postgres::PgListener::connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
        listener.listen(CHANNEL_RUNNER_JOBS).await.unwrap();
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(first).execute(&pool).await.unwrap();
        assert_eq!(next_wake(&mut listener).await, runner.to_string(), "the end of a writer wakes the device");
        assert!(!store.has_work(runner).await.unwrap(), "a lease that has not ended still holds the folder");
        sqlx::query("UPDATE runner_leases SET status = 'completed', finished_at = NOW() WHERE run_id = $1").bind(first).execute(&pool).await.unwrap();
        assert_eq!(next_wake(&mut listener).await, runner.to_string(), "the end of its lease wakes the device again");
        assert!(store.has_work(runner).await.unwrap());
        let next: Vec<Uuid> = claim_all().await.iter().map(|run| run.run_id).collect();
        assert_eq!(next, vec![second], "the waiting writer starts once the folder is free");

        let publish = queue(resumed, "publish").await;
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(second).execute(&pool).await.unwrap();
        sqlx::query("UPDATE runner_leases SET status = 'completed', finished_at = NOW() WHERE run_id = $1").bind(second).execute(&pool).await.unwrap();
        let late = queue(writer, "followup").await;
        let publishing: Vec<Uuid> = claim_all().await.iter().map(|run| run.run_id).collect();
        assert_eq!(publishing, vec![publish], "a publish holds the folder like any writer");
        assert!(!publishing.contains(&late));
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(publish).execute(&pool).await.unwrap();
        sqlx::query("UPDATE runner_leases SET status = 'completed', finished_at = NOW() WHERE run_id = $1").bind(publish).execute(&pool).await.unwrap();

        let checking: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir, parent_session_id)
             VALUES ($1, 'review', 'codex', 'test-model', $2, $3) RETURNING id",
        )
        .bind(agent)
        .bind(format!("tasks/{tag}/world"))
        .bind(resumed)
        .fetch_one(&pool)
        .await
        .unwrap();
        let checks = queue(checking, "review").await;
        let reviewing: Vec<Uuid> = claim_all().await.iter().map(|run| run.run_id).collect();
        assert_eq!(reviewing, vec![checks], "another session's writer waits while finished work is under its automatic review");
        sqlx::query("UPDATE session_runs SET status = 'completed', finished_at = NOW() WHERE id = $1").bind(checks).execute(&pool).await.unwrap();
        assert!(!store.has_work(runner).await.unwrap(), "a review whose lease is still finishing keeps the folder");
        sqlx::query("UPDATE runner_leases SET status = 'completed', finished_at = NOW() WHERE run_id = $1").bind(checks).execute(&pool).await.unwrap();
        let after: Vec<Uuid> = claim_all().await.iter().map(|run| run.run_id).collect();
        assert_eq!(after, vec![late], "the writer starts once the review is done");
    }

    async fn next_wake(listener: &mut sqlx::postgres::PgListener) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv()).await.unwrap().unwrap().payload().to_string()
    }
}
