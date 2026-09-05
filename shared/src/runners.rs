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

use crate::world::{emit_in, NewEvent};

pub const CHANNEL_RUNNER_JOBS: &str = "runner_jobs";
pub const RUNNER_LEASE_SECONDS: i64 = 60;
pub const RUNNER_SLEEP_GRACE_SECONDS: i64 = 3 * 3600;
pub const RUNNER_OUTAGE_GRACE_SECONDS: i64 = 3600;
pub const RUNNER_ASLEEP_ERROR: &str = "runner lease expired: the device was asleep or offline for over 3 hours";
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

fn live_runner_sql(runner: &str) -> String {
    format!("{runner}.status = 'online' AND {runner}.last_seen_at > NOW() - INTERVAL '75 seconds'")
}

const RUNS_HERE: &str = "COALESCE(a.runner_id = l.runner_id OR (r.kind = 'direct' AND r.runner_id = l.runner_id AND a.owner_user_id = w.owner_user_id), false)";

fn serving(sql: &str) -> String {
    sql.replace("__SERVES__", &serves_sql("a", "w"))
        .replace("__HOME__", RUNS_HERE)
        .replace("__URGENT__", &crate::world::urgent_run_sql("q"))
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
    #[serde(default)]
    pub retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_feed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps: Option<bool>,
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
pub struct RunnerPublish {
    pub repo: String,
    pub branch: String,
    pub base_branch: String,
    pub commit_message: String,
    pub pr_title: String,
    pub pr_body: String,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub pr_number: Option<i64>,
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
    plausible_pr_url(url)
        && url
            .to_ascii_lowercase()
            .ends_with(&format!("/{}/pull/{number}", repo.trim_matches('/').to_ascii_lowercase()))
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
        let new_thread = self.input_tokens < previous.input_tokens
            || self.cached_input_tokens < previous.cached_input_tokens
            || self.output_tokens < previous.output_tokens;
        if new_thread {
            return *self;
        }
        RunnerUsage {
            input_tokens: self.input_tokens - previous.input_tokens,
            cached_input_tokens: self.cached_input_tokens - previous.cached_input_tokens,
            output_tokens: self.output_tokens - previous.output_tokens,
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
    #[serde(default)]
    pub provider_keys: BTreeMap<String, String>,
    #[serde(default)]
    pub images: Vec<RunnerImage>,
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
    pub omp_models: Option<String>,
    #[serde(default)]
    pub omp_secrets: Vec<String>,
    #[serde(default)]
    pub command_rules: Option<crate::command_rules::Policy>,
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    #[serde(default)]
    pub keys_version: Option<String>,
    #[serde(default)]
    pub metered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_scan: Option<crate::setup_scan::ScanRequest>,
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
    #[serde(default)]
    pub checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steered: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_summary: Option<String>,
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

    pub async fn closed_tasks(&self, runner_id: Uuid, task_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT t.id FROM agent_tasks t JOIN agent_profiles a ON a.id = t.assigned_agent_id
             JOIN world_runners w ON w.id = $2
             WHERE t.id = ANY($1) AND (a.owner_user_id = w.owner_user_id OR a.runner_id = w.id)
               AND (t.status = 'cancelled' OR t.status IN ('done', 'failed')
                 AND t.updated_at < NOW() - INTERVAL '7 days'
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs r WHERE r.task_id = t.id
                     AND COALESCE(r.finished_at, r.created_at) > NOW() - INTERVAL '7 days'
                 ))
               AND NOT EXISTS (
                 SELECT 1 FROM session_runs r WHERE r.task_id = t.id AND r.status IN ('queued', 'running')
               )",
        )
        .bind(task_ids)
        .bind(runner_id)
        .fetch_all(&self.pool)
        .await?)
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
               lease_id = $3, lease_expires_at = $4 WHERE id = $1",
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

    #[allow(clippy::too_many_arguments)]
    pub async fn apply_success(
        &self,
        lease: &LeaseContext,
        output: &str,
        cli_session_id: Option<&str>,
        usage_baseline: &Value,
        context_tokens: i64,
        cost_usd: f64,
        input_tokens: i64,
        cached_input_tokens: i64,
        output_tokens: i64,
    ) -> Result<ApplyCompletion> {
        let mut tx = self.pool.begin().await?;
        if !lock_finishing_lease(&mut tx, lease).await? {
            return Ok(ApplyCompletion::Stopped);
        }
        let changed = sqlx::query(
            "UPDATE session_runs SET status = 'completed', output = $2, error = NULL,
               cost_usd = $3, input_tokens = $4, cached_input_tokens = $5,
               output_tokens = $6, finished_at = NOW()
             WHERE id = $1 AND status = 'running' AND lease_id = $7",
        )
        .bind(lease.run_id)
        .bind(output)
        .bind(cost_usd)
        .bind(input_tokens)
        .bind(cached_input_tokens)
        .bind(output_tokens)
        .bind(lease.lease_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 1 {
            sqlx::query(
                "UPDATE agent_sessions SET cli_session_id = COALESCE($2, cli_session_id),
                   summary = CASE WHEN role = 'main' AND $2 IS NOT NULL THEN NULL ELSE summary END,
                   usage_baseline = $3, context_tokens = $4, updated_at = NOW() WHERE id = $1",
            )
            .bind(lease.session_id)
            .bind(cli_session_id)
            .bind(usage_baseline)
            .bind(context_tokens)
            .execute(&mut *tx)
            .await?;
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
               AND r.status IN ('completed', 'failed')",
        )
        .bind(lease_id)
        .bind(runner_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed == 1)
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
             WHERE id IN (SELECT inbox_id FROM runner_run_inbox WHERE run_id = $1)",
        )
        .bind(run_id)
        .bind(status)
        .bind(error)
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
               AND (a.owner_user_id = w.owner_user_id OR NOT EXISTS (
                 SELECT 1 FROM world_runners own
                 WHERE own.owner_user_id = a.owner_user_id AND {live_own}
                   AND COALESCE((own.capabilities->'backends'->>a.cli)::boolean, false)))
             RETURNING a.id",
            serves = serves_sql("a", "w"),
            fresh = FRESH_AGENT_SQL,
            live_own = live_runner_sql("own"),
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
             WHERE a.id = $1 AND a.runner_id IS NULL AND {live}
               AND u.world_role IN ('write', 'admin')
               AND COALESCE((w.capabilities->'backends'->>a.cli)::boolean, false)
             ORDER BY (w.owner_user_id = a.owner_user_id) DESC, w.last_seen_at DESC LIMIT 1",
            serves = serves_sql("a", "w"),
            live = live_runner_sql("w"),
        ))
        .bind(agent_id)
        .fetch_optional(&self.pool)
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
                 AND o.world_role IN ('read', 'write', 'admin') AND a.paused_reason IS NULL
                 AND (r.kind <> 'followup' OR s.cli_session_id IS NOT NULL)
                 AND NOT EXISTS (
                   SELECT 1 FROM session_runs active
                   WHERE active.session_id = r.session_id AND active.status = 'running')
                 AND NOT EXISTS (
                   SELECT 1 FROM runner_leases lease
                   JOIN session_runs leased ON leased.id = lease.run_id
                   WHERE leased.session_id = r.session_id AND lease.status IN ('active', 'finishing'))
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
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin'))"
                .replace("{ready}", &crate::world::inbox_ready_sql("a"))),
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
    fn runner_reports_must_look_real() {
        assert!(plausible_cost(1.5));
        assert!(!plausible_cost(-1.0));
        assert!(!plausible_cost(f64::NAN));
        assert!(!plausible_cost(1e300));
        assert!(usage(10, 0, 5).plausible());
        assert!(!usage(u64::MAX, 0, 0).plausible());
        assert!(pr_url_matches("https://github.com/Acme/App/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://evil.example/acme/app/pull/12x", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/acme/other/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("http://github.com/acme/app/pull/12", "acme/app", 12));
        assert!(!pr_url_matches("https://github.com/acme/app/pull/13", "acme/app", 12));
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
                .max_connections(8)
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
            checkpoint: None,
            steered: Vec::new(),
            retryable: false,
            work_summary: None,
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
        assert_eq!(
            followup_error.to_string(),
            "archived sessions cannot receive follow-ups"
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
        assert!(!store
            .heartbeat_lease(Uuid::new_v4(), claim.lease_id)
            .await
            .unwrap()
            .is_some());
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
                    Some("cli-session"),
                    &json!({}),
                    12,
                    0.1,
                    10,
                    2,
                    3,
                )
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
                    Some("failed-session"),
                    &json!({}),
                    12,
                    0.1,
                    10,
                    2,
                    3,
                )
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
        assert!(!store
            .heartbeat_lease(other.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
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
        assert!(!store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
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
        assert!(!store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
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
        assert!(!store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
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
        assert!(!store
            .heartbeat_lease(registration.runner.id, claim.lease_id)
            .await
            .unwrap().is_some());
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
        let baseline = json!({ "input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 3 });
        let (applied, paused) = tokio::join!(
            apply.apply_success(
                &lease,
                "done",
                Some("cli-session"),
                &baseline,
                12,
                0.1,
                10,
                2,
                3,
            ),
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
            assert_eq!(session_baseline, baseline);
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
            .apply_success(&lease, "done", None, &baseline, 12, 0.1, 10, 2, 3)
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
}
