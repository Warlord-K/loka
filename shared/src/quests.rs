use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{AgentSession, NewEvent, NewMessage, ReviewerChoice, WorldStore};

pub const MAX_ATTEMPTS: i16 = 3;
pub const PLAN_OR_ATTEMPTS: &str = "plan_first and several attempts cannot be combined on one quest yet; pick one";
pub const PACES: [&str; 3] = ["normal", "quick", "urgent"];
pub const QUICK_ALONE: &str = "a quick quest runs once without a plan; drop plan_first and attempts, or use pace normal";
pub const PLAN_APPROVE: &str = "Approve";
pub const PLAN_REDIRECT: &str = "Redirect";

pub const PLAN_FIRST: &str = "[plan first] This first step is planning only. Read the code, docs and history you need and run read-only commands if they help, but do not edit, create or delete files, do not commit and do not install anything. Then end your final message with a plan block instead of the report block:\n<!-- plan\n{\"summary\": \"The approach in 2 to 4 sentences\", \"steps\": [\"A concrete step\"], \"checks\": [\"What will prove it works\"], \"risks\": [\"What could go wrong or needs a decision\"]}\nplan -->\nYour owner approves or redirects the plan. Only after approval do you change anything, and then you finish with the report block as described above.";

pub const PLAN_AGAIN: &str = "This is feedback on your plan, not approval. Do not change any files yet. Revise the plan with it and end with a new plan block.";

pub fn plan_approved_prompt(by: &str) -> String {
    format!(
        "[plan approved by {by}]\nYour plan was approved. Implement it now with your tools, following the task prompt above: you may edit files from here on. Check your work and end with the report block, status done or blocked."
    )
}

pub fn plan_redirect_prompt(by: &str, text: &str) -> String {
    let text = text.trim();
    let feedback = if text.is_empty() || text.eq_ignore_ascii_case(PLAN_REDIRECT) {
        "They did not approve this plan. Rethink the approach, and say what you changed and why."
    } else {
        text
    };
    format!("[plan redirected by {by}]\n{feedback}\n\n{PLAN_AGAIN}")
}

pub fn is_approval(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    matches!(t.as_str(), "approve" | "approved" | "yes" | "lgtm" | "go" | "go ahead" | "ship it" | "ok" | "okay")
        || t.starts_with("approve ")
}

pub fn ci_feedback_everywhere() -> bool {
    matches!(
        std::env::var("WORLD_CI_FEEDBACK").unwrap_or_default().to_lowercase().as_str(),
        "on" | "true" | "1" | "yes"
    )
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, FromRow)]
pub struct QuestOptions {
    pub plan_first: bool,
    pub attempts: i16,
    pub attempt_variants: Value,
    pub ci_feedback: bool,
    pub pace: String,
}

impl QuestOptions {
    pub fn quick(&self) -> bool {
        self.pace == "quick"
    }

    pub fn urgent(&self) -> bool {
        self.pace == "urgent"
    }

    pub fn reports_back(&self) -> bool {
        !self.quick() && !self.urgent()
    }

    pub fn effort_for(&self, backend: &str) -> Option<String> {
        (backend == "claude" && self.quick()).then(|| "medium".to_string())
    }

    pub fn fast_for(&self, backend: &str) -> bool {
        backend == "claude" && self.urgent()
    }

    pub fn variant(&self, attempt: i16) -> Option<ReviewerChoice> {
        self.attempt_variants
            .as_array()?
            .get(usize::try_from(attempt - 1).ok()?)
            .and_then(ReviewerChoice::from_value)
    }

    pub fn racing(&self) -> bool {
        self.attempts > 1
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct QuestChoice {
    #[serde(default)]
    pub plan_first: Option<bool>,
    #[serde(default)]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub attempt_variants: Option<Vec<Value>>,
    #[serde(default)]
    pub ci_feedback: Option<bool>,
    #[serde(default)]
    pub pace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValidChoice {
    pub plan_first: Option<bool>,
    pub attempts: Option<i16>,
    pub variants: Option<Vec<ReviewerChoice>>,
    pub ci_feedback: Option<bool>,
    pub pace: Option<String>,
}

impl ValidChoice {
    pub fn is_empty(&self) -> bool {
        self.plan_first.is_none()
            && self.attempts.is_none()
            && self.variants.is_none()
            && self.ci_feedback.is_none()
            && self.pace.is_none()
    }
}

pub fn parse_pace(value: &str) -> std::result::Result<String, String> {
    let pace = value.trim().to_lowercase();
    if PACES.contains(&pace.as_str()) {
        Ok(pace)
    } else {
        Err("pace must be normal, quick or urgent".into())
    }
}

impl QuestChoice {
    pub fn from_args(args: &Value) -> std::result::Result<Self, String> {
        let flag = |key: &str| match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("{key} must be true or false")),
        };
        let plan_first = flag("plan_first")?;
        let ci_feedback = flag("ci_feedback")?;
        let attempts = match args.get("attempts") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .ok_or("attempts must be a whole number from 1 to 3")?,
            ),
        };
        let attempt_variants = match args.get("attempt_variants") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(items.clone()),
            Some(_) => return Err("attempt_variants must be a list of {\"cli\", \"model\"}".into()),
        };
        let pace = match args.get("pace") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(p.clone()),
            Some(_) => return Err("pace must be normal, quick or urgent".into()),
        };
        Ok(Self { plan_first, attempts, attempt_variants, ci_feedback, pace })
    }

    pub fn validate(&self) -> std::result::Result<ValidChoice, String> {
        let variants = match &self.attempt_variants {
            None => None,
            Some(items) => {
                let mut out = Vec::new();
                for item in items {
                    let choice = match item {
                        Value::String(model) => ReviewerChoice::new(None, model),
                        Value::Object(o) => ReviewerChoice::new(
                            o.get("cli").and_then(Value::as_str),
                            o.get("model").and_then(Value::as_str).unwrap_or(""),
                        ),
                        _ => Err("each attempt variant needs a model".into()),
                    }
                    .map_err(|e| e.replace("a reviewer", "an attempt").replace("reviewer CLI", "CLI"))?;
                    out.push(choice);
                }
                Some(out)
            }
        };
        let count = variants.as_ref().map(Vec::len).unwrap_or(0) as i64;
        let attempts = match (self.attempts, count) {
            (Some(n), _) if !(1..=MAX_ATTEMPTS as i64).contains(&n) => {
                return Err(format!("attempts must be 1 to {MAX_ATTEMPTS}"))
            }
            (Some(n), c) if c > n => return Err(format!("{c} attempt variants for {n} attempts")),
            (Some(n), _) => Some(n as i16),
            (None, c) if c > MAX_ATTEMPTS as i64 => return Err(format!("at most {MAX_ATTEMPTS} attempts")),
            (None, c) if c > 0 => Some(c.max(2) as i16),
            (None, _) => None,
        };
        if self.plan_first == Some(true) && attempts.is_some_and(|n| n > 1) {
            return Err(PLAN_OR_ATTEMPTS.into());
        }
        let pace = self.pace.as_deref().map(parse_pace).transpose()?;
        if pace.as_deref() == Some("quick") && (self.plan_first == Some(true) || attempts.is_some_and(|n| n > 1)) {
            return Err(QUICK_ALONE.into());
        }
        Ok(ValidChoice { plan_first: self.plan_first, attempts, variants, ci_feedback: self.ci_feedback, pace })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanAnswer {
    NotAPlan,
    Stale,
    Approved { session_id: Uuid, run_id: Uuid },
    Redirected { session_id: Uuid, run_id: Uuid },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttemptsOutcome {
    Settled,
    Waiting,
    Winner(Uuid),
    Compare(Vec<Uuid>),
    NoneFinished,
}

pub fn attempts_outcome(rows: &[(Uuid, String)]) -> AttemptsOutcome {
    if rows.iter().any(|(_, state)| state == "running") {
        return AttemptsOutcome::Waiting;
    }
    let finished: Vec<Uuid> = rows.iter().filter(|(_, s)| s == "finished").map(|(id, _)| *id).collect();
    match finished.len() {
        0 => AttemptsOutcome::NoneFinished,
        1 => AttemptsOutcome::Winner(finished[0]),
        _ => AttemptsOutcome::Compare(finished),
    }
}

pub fn compare_candidates(review: &AgentSession) -> Option<Vec<Uuid>> {
    let ids = review.asked_by.as_ref()?.get("compare")?.as_array()?;
    Some(ids.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AttemptCost {
    pub session_id: Uuid,
    pub attempt: Option<i16>,
    pub backend: String,
    pub model: String,
    pub cost_usd: f64,
}

pub fn inbox_task_ids(items: &[crate::world::InboxItem]) -> Vec<Uuid> {
    items
        .iter()
        .filter(|item| item.payload.get("subscribed").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| item.payload.get("task_id").and_then(Value::as_str)?.parse().ok())
        .collect()
}

impl WorldStore {
    pub async fn any_urgent(&self, task_ids: &[Uuid]) -> Result<bool> {
        if task_ids.is_empty() {
            return Ok(false);
        }
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_tasks WHERE id = ANY($1) AND pace = 'urgent')")
            .bind(task_ids)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn quest_timing(&self, session_id: Uuid) -> Result<Value> {
        Ok(sqlx::query_scalar::<_, Value>(
            "WITH since AS (
               SELECT COALESCE(MAX(created_at), '-infinity'::timestamptz) AS at
               FROM world_events WHERE session_id = $1 AND type = 'task.finished'
             ), runs AS (
               SELECT r.kind, r.created_at, COALESCE(r.started_at, NOW()) AS started_at,
                      COALESCE(r.finished_at, NOW()) AS finished_at
               FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
               WHERE (s.id = $1 OR (s.parent_session_id = $1 AND s.role = 'review'))
                 AND r.created_at > (SELECT at FROM since) AND r.status <> 'cancelled'
                 AND r.steered_into IS NULL
             )
             SELECT jsonb_build_object(
               'queued_secs', COALESCE(ROUND(SUM(EXTRACT(EPOCH FROM started_at - created_at)) FILTER (WHERE kind IN ('task', 'followup'))), 0),
               'work_secs', COALESCE(ROUND(SUM(EXTRACT(EPOCH FROM finished_at - started_at)) FILTER (WHERE kind IN ('task', 'followup'))), 0),
               'review_secs', COALESCE(ROUND(SUM(EXTRACT(EPOCH FROM finished_at - created_at)) FILTER (WHERE kind = 'review')), 0),
               'publish_secs', COALESCE(ROUND(SUM(EXTRACT(EPOCH FROM finished_at - created_at)) FILTER (WHERE kind = 'publish')), 0),
               'total_secs', COALESCE(ROUND(EXTRACT(EPOCH FROM NOW() - MIN(created_at))), 0)
             ) FROM runs",
        )
        .bind(session_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn quest_options(&self, task_id: Uuid) -> Result<QuestOptions> {
        Ok(sqlx::query_as::<_, QuestOptions>(
            "SELECT plan_first, attempts, attempt_variants, ci_feedback, pace FROM agent_tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?
        .unwrap_or(QuestOptions { plan_first: false, attempts: 1, attempt_variants: json!([]), ci_feedback: false, pace: "normal".into() }))
    }

    pub async fn set_quest_options(&self, task_id: Uuid, choice: &ValidChoice) -> Result<()> {
        if choice.is_empty() {
            return Ok(());
        }
        let variants = choice
            .variants
            .as_ref()
            .map(|v| Value::Array(v.iter().map(ReviewerChoice::to_value).collect()));
        let current = self.quest_options(task_id).await?;
        let plan_first = choice.plan_first.unwrap_or(current.plan_first);
        let attempts = choice.attempts.unwrap_or(current.attempts);
        if plan_first && attempts > 1 {
            anyhow::bail!(PLAN_OR_ATTEMPTS);
        }
        let pace = choice.pace.as_deref().unwrap_or(&current.pace);
        if pace == "quick" && (plan_first || attempts > 1) {
            anyhow::bail!(QUICK_ALONE);
        }
        let found = sqlx::query(
            "UPDATE agent_tasks
             SET plan_first = COALESCE($2, plan_first), attempts = COALESCE($3, attempts),
                 attempt_variants = COALESCE($4, attempt_variants), ci_feedback = COALESCE($5, ci_feedback),
                 pace = COALESCE($6, pace), updated_at = NOW()
             WHERE id = $1",
        )
        .bind(task_id)
        .bind(choice.plan_first)
        .bind(choice.attempts)
        .bind(variants)
        .bind(choice.ci_feedback)
        .bind(choice.pace.as_deref())
        .execute(self.pool())
        .await?;
        if found.rows_affected() == 0 {
            anyhow::bail!("no such quest");
        }
        Ok(())
    }

    pub async fn begin_plan(&self, session_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_sessions SET plan_state = 'planning', updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn mark_attempt(&self, session_id: Uuid, attempt: i16) -> Result<()> {
        sqlx::query(
            "UPDATE agent_sessions SET attempt = $2, attempt_state = 'running', review = FALSE, updated_at = NOW() WHERE id = $1",
        )
        .bind(session_id)
        .bind(attempt)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn plan_ready(&self, session_id: Uuid, run_created_at: DateTime<Utc>) -> Result<bool> {
        let mut tx = self.pool().begin().await?;
        let state = sqlx::query_scalar::<_, Option<String>>(
            "SELECT plan_state FROM agent_sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        if state.as_deref() != Some("planning") {
            return Ok(false);
        }
        let newer = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM session_runs
               WHERE session_id = $1 AND kind = 'followup' AND created_at > $2 AND steered_into IS NULL)",
        )
        .bind(session_id)
        .bind(run_created_at)
        .fetch_one(&mut *tx)
        .await?;
        if newer {
            return Ok(false);
        }
        sqlx::query("UPDATE agent_sessions SET plan_state = 'awaiting', status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn post_plan(
        &self,
        agent_id: Uuid,
        agent_name: &str,
        task_id: Uuid,
        title: &str,
        session_id: Uuid,
        run_id: Uuid,
        body: &str,
    ) -> Result<()> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND run_id = $2 AND meta->'plan'->>'session_id' = $3)",
        )
        .bind(agent_id)
        .bind(run_id)
        .bind(session_id.to_string())
        .fetch_one(self.pool())
        .await?;
        if exists {
            return Ok(());
        }
        let main = self.main_session(agent_id).await?;
        let text = format!("Plan for \"{title}\", waiting for your approval:\n\n{}", body.trim());
        let choices = [PLAN_APPROVE, PLAN_REDIRECT];
        let message = self
            .insert_message(NewMessage {
                agent_id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent_id),
                body: &text,
                session_id: main.as_ref().map(|m| m.id),
                run_id: Some(run_id),
                meta: json!({
                    "kind": "ask",
                    "choices": choices,
                    "task_id": task_id,
                    "plan": { "task_id": task_id, "session_id": session_id },
                }),
            })
            .await?;
        let mut created = NewEvent::new("message.created")
            .agent(agent_id)
            .run(run_id)
            .task(Some(task_id))
            .payload(json!({ "message_id": message.id, "author": agent_name, "author_kind": "agent", "body": text, "ask": true }));
        if let Some(main) = &main {
            created = created.session(main.id);
        }
        self.emit(created, None).await?;
        self.emit(
            NewEvent::new("attention.requested")
                .agent(agent_id)
                .session(session_id)
                .run(run_id)
                .task(Some(task_id))
                .payload(json!({ "text": text, "agent": agent_name, "message_id": message.id, "choices": choices, "plan": true })),
            None,
        )
        .await?;
        self.emit(
            NewEvent::new("task.plan_ready")
                .agent(agent_id)
                .session(session_id)
                .run(run_id)
                .task(Some(task_id))
                .payload(json!({ "task_id": task_id, "title": title, "session_id": session_id, "message_id": message.id })),
            None,
        )
        .await?;
        let detail: String = format!("Plan for \"{title}\" needs your approval").chars().take(120).collect();
        self.set_agent_status(agent_id, "waiting", Some(&detail)).await?;
        Ok(())
    }

    pub async fn can_answer_card(&self, agent_id: Uuid, card_id: Uuid, user: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_messages m JOIN agent_profiles a ON a.id = m.agent_id
                             WHERE m.id = $2 AND m.agent_id = $1 AND m.author_kind <> 'user' AND a.owner_user_id = $3)",
        )
        .bind(agent_id)
        .bind(card_id)
        .bind(user)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn answer_plan(&self, agent_id: Uuid, ask_id: Uuid, text: &str, by: &str) -> Result<PlanAnswer> {
        let plan = sqlx::query_scalar::<_, Option<Value>>(
            "SELECT meta->'plan' FROM agent_messages WHERE id = $1 AND agent_id = $2",
        )
        .bind(ask_id)
        .bind(agent_id)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        let Some(session_id) = plan
            .as_ref()
            .and_then(|p| p.get("session_id"))
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Uuid>().ok())
        else {
            return Ok(PlanAnswer::NotAPlan);
        };
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, (Option<String>, Option<Uuid>)>(
            "SELECT plan_state, task_id FROM agent_sessions WHERE id = $1 AND agent_id = $2 AND status <> 'archived' FOR UPDATE",
        )
        .bind(session_id)
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((Some(state), task_id)) = row else {
            return Ok(PlanAnswer::Stale);
        };
        if state != "awaiting" {
            return Ok(PlanAnswer::Stale);
        }
        let approved = is_approval(text);
        let (next, prompt, activity) = if approved {
            ("approved", plan_approved_prompt(by), "Implementing the approved plan")
        } else {
            ("planning", plan_redirect_prompt(by, text), "Reworking the plan")
        };
        sqlx::query("UPDATE agent_sessions SET plan_state = $2, status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity)
             VALUES ($1, $2, $3, 'followup', 'queued', $4, $5, $6)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(&prompt)
        .bind(crate::world::default_task_timeout_secs())
        .bind(activity)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let event = if approved { "task.plan_approved" } else { "task.plan_redirected" };
        self.emit(
            NewEvent::new(event)
                .agent(agent_id)
                .session(session_id)
                .run(run_id)
                .task(task_id)
                .payload(json!({ "task_id": task_id, "session_id": session_id, "by": by, "text": text.trim() })),
            None,
        )
        .await?;
        if self.get_agent(agent_id).await?.is_some_and(|a| a.status == "waiting") {
            self.set_agent_status(agent_id, "idle", None).await?;
        }
        Ok(if approved {
            PlanAnswer::Approved { session_id, run_id }
        } else {
            PlanAnswer::Redirected { session_id, run_id }
        })
    }

    pub async fn finish_attempt(&self, session_id: Uuid, ok: bool) -> Result<AttemptsOutcome> {
        let mut tx = self.pool().begin().await?;
        let task_id = sqlx::query_scalar::<_, Option<Uuid>>("SELECT task_id FROM agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
        let Some(task_id) = task_id else {
            return Ok(AttemptsOutcome::Settled);
        };
        sqlx::query("SELECT id FROM agent_tasks WHERE id = $1 FOR UPDATE")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        let changed = sqlx::query(
            "UPDATE agent_sessions SET attempt_state = $2, updated_at = NOW()
             WHERE id = $1 AND attempt_state = 'running'",
        )
        .bind(session_id)
        .bind(if ok { "finished" } else { "failed" })
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Ok(AttemptsOutcome::Settled);
        }
        let rows = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, attempt_state FROM agent_sessions
             WHERE task_id = $1 AND attempt IS NOT NULL AND attempt_state IN ('running', 'finished', 'failed')
             ORDER BY attempt",
        )
        .bind(task_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(attempts_outcome(&rows))
    }

    pub async fn settle_attempts(&self, task_id: Uuid, winner: Uuid) -> Result<Vec<AttemptCost>> {
        let mut tx = self.pool().begin().await?;
        let lost = sqlx::query_as::<_, AttemptCost>(
            "UPDATE agent_sessions s
             SET attempt_state = 'lost', status = 'archived', updated_at = NOW()
             WHERE s.task_id = $1 AND s.id <> $2 AND s.attempt IS NOT NULL
               AND s.attempt_state IN ('running', 'finished', 'failed')
             RETURNING s.id AS session_id, s.attempt, s.backend, s.model,
               COALESCE((SELECT SUM(r.cost_usd) FROM session_runs r WHERE r.session_id = s.id), 0)::float8 AS cost_usd",
        )
        .bind(task_id)
        .bind(winner)
        .fetch_all(&mut *tx)
        .await?;
        let stopped: Vec<Uuid> = sqlx::query_scalar(
            "WITH stopped AS (
               UPDATE session_runs
               SET status = 'cancelled', error = 'Another attempt won', finished_at = NOW(),
                   stop_requested_by = CASE WHEN status = 'running' THEN 'another attempt' ELSE stop_requested_by END
               WHERE session_id = ANY($1) AND status IN ('queued', 'running')
               RETURNING id
             ), leases AS (
               UPDATE runner_leases SET status = 'cancelled', finished_at = NOW()
               WHERE run_id IN (SELECT id FROM stopped) AND status = 'active'
             )
             SELECT id FROM stopped",
        )
        .bind(lost.iter().map(|l| l.session_id).collect::<Vec<_>>())
        .fetch_all(&mut *tx)
        .await?;
        for id in &stopped {
            sqlx::query("SELECT pg_notify('session_runs', $1)").bind(id.to_string()).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE agent_sessions SET attempt_state = 'won', updated_at = NOW() WHERE id = $1 AND attempt IS NOT NULL")
            .bind(winner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(lost)
    }

    pub async fn announce_attempts(&self, agent_id: Uuid, task_id: Uuid, winner: &AgentSession, lost: &[AttemptCost], why: &str) -> Result<()> {
        if winner.attempt.is_none() {
            return Ok(());
        }
        self.emit(
            NewEvent::new("task.attempts_settled")
                .agent(agent_id)
                .session(winner.id)
                .task(Some(task_id))
                .payload(json!({
                    "task_id": task_id,
                    "winner_session_id": winner.id,
                    "winner_attempt": winner.attempt,
                    "winner_cli": winner.backend,
                    "winner_model": winner.model,
                    "lost": lost,
                    "lost_cost_usd": lost.iter().map(|l| l.cost_usd).sum::<f64>(),
                    "why": why,
                })),
            None,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milestones::{MilestoneStore, NewMilestone};
    use crate::world::NewSession;
    use sqlx::postgres::PgPoolOptions;

    async fn task_session(store: &WorldStore, agent: Uuid, main: Uuid, task: Uuid, dir: &str) -> Result<AgentSession> {
        let session = store
            .create_session(NewSession {
                agent_id: agent,
                role: "task",
                backend: "claude",
                model: "opus",
                parent_session_id: Some(main),
                task_id: Some(task),
                working_dir: Some(dir),
                usage_baseline: json!({}),
            })
            .await?;
        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1")
            .bind(session.id)
            .bind(Uuid::new_v4().to_string())
            .execute(store.pool())
            .await?;
        Ok(store.get_session(session.id).await?.expect("session"))
    }

    #[tokio::test]
    async fn quest_options_guard_closed_state() -> Result<()> {
        let Ok(url) = std::env::var("QUESTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let db = crate::db::Database::connect(&url).await?;
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@guards.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let slug = format!("g{}", &Uuid::new_v4().simple().to_string()[..10]);
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Guard', 'claude', 'opus', $2) RETURNING id",
        )
        .bind(&slug)
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let main = store
            .create_session(NewSession { agent_id: agent, role: "main", backend: "claude", model: "opus", parent_session_id: None, task_id: None, working_dir: None, usage_baseline: json!({}) })
            .await?;
        let outcome = |status| crate::world::RunOutcome { status, output: Some("late"), error: None, cost_usd: 2.0, input_tokens: 10, cached_input_tokens: 0, output_tokens: 5 };

        let race: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Race', 'in_progress') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let one = task_session(&store, agent, main.id, race, "tasks/g/repo").await?;
        let two = task_session(&store, agent, main.id, race, "tasks/g/repo-2").await?;
        store.mark_attempt(one.id, 1).await?;
        store.mark_attempt(two.id, 2).await?;
        let running = store
            .create_run(crate::world::NewRun { session_id: one.id, agent_id: agent, task_id: Some(race), kind: "task", status: "running", fork_from: None, prompt: "go", timeout_secs: 60 })
            .await?;
        let winning = store
            .create_run(crate::world::NewRun { session_id: two.id, agent_id: agent, task_id: Some(race), kind: "task", status: "running", fork_from: None, prompt: "go", timeout_secs: 60 })
            .await?;
        assert!(store.finish_run(winning.id, outcome("completed")).await?);
        store.settle_attempts(race, two.id).await?;
        let stopped = store.get_run(running.id).await?.unwrap();
        assert_eq!(stopped.status, "cancelled");
        assert_eq!(store.stop_requested(running.id).await?.as_deref(), Some("another attempt"));
        assert!(!store.finish_run(running.id, outcome("completed")).await?);
        let after: (String, Option<String>, f64) = sqlx::query_as("SELECT status, output, cost_usd::float8 FROM session_runs WHERE id = $1")
            .bind(running.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!((after.0.as_str(), after.1.as_deref()), ("cancelled", None));
        assert!((after.2 - 2.0).abs() < 1e-9);
        assert!(!store.finish_run(winning.id, outcome("failed")).await?);
        assert_eq!(store.get_run(winning.id).await?.unwrap().status, "completed");

        assert!(!store.set_session_status(one.id, "failed", Some("late")).await?);
        assert_eq!(store.get_session(one.id).await?.unwrap().status, "archived");
        assert!(!store.complete_task_run(one.id, running.created_at).await?);
        assert_eq!(store.get_session(one.id).await?.unwrap().status, "archived");
        assert!(store.set_session_status(two.id, "active", None).await?);

        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Stop me', 'cancelled') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;
        assert!(!db.update_status(quest, "done").await?);
        db.set_error(quest, "late failure").await?;
        db.set_pr(quest, "https://github.com/o/r/pull/1", 1).await?;
        let status: (String, Option<String>) = sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        assert_eq!((status.0.as_str(), status.1), ("cancelled", None));
        assert!(db.update_status(race, "review").await?);

        let parent: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Parent', 'cancelled') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let child: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status, parent_task_id) VALUES ($1, 'Child', 'in_progress', $2) RETURNING id")
            .bind(user)
            .bind(parent)
            .fetch_one(&pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(child).execute(&pool).await?;
        let parent_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(parent).fetch_one(&pool).await?;
        assert_eq!(parent_status, "cancelled");
        Ok(())
    }

    #[tokio::test]
    async fn quest_options_cards_answer_only_by_owner() -> Result<()> {
        let Ok(url) = std::env::var("QUESTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let person = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email) VALUES ($1) RETURNING id")
                    .bind(format!("{}@cards.test", Uuid::new_v4()))
                    .fetch_one(&pool)
                    .await
            }
        };
        let (owner, stranger) = (person().await?, person().await?);
        let public_agent = |who: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility) VALUES ($1, 'Asker', 'claude', 'opus', $2, 'public') RETURNING id",
                )
                .bind(format!("c{}", &Uuid::new_v4().simple().to_string()[..10]))
                .bind(who)
                .fetch_one(&pool)
                .await
            }
        };
        let (agent, elsewhere) = (public_agent(owner).await?, public_agent(stranger).await?);
        let card = |agent: Uuid, kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta) VALUES ($1, $2, CASE WHEN $2 = 'agent' THEN $1 END, 'Pick one', $3) RETURNING id",
                )
                .bind(agent)
                .bind(kind)
                .bind(json!({ "kind": "ask", "choices": ["Yes", "No"] }))
                .fetch_one(&pool)
                .await
            }
        };
        let ask = card(agent, "agent").await?;
        assert!(store.can_answer_card(agent, ask, owner).await?);
        assert!(!store.can_answer_card(agent, ask, stranger).await?, "other people never answer or lock the owner's cards");
        assert!(!store.can_answer_card(elsewhere, ask, stranger).await?, "a card is answered on its own agent");
        let own_message = card(agent, "user").await?;
        assert!(!store.can_answer_card(agent, own_message, owner).await?, "only agent cards take answers");
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![agent, elsewhere]).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn quest_options_plan_attempts_and_ci_dedupe() -> Result<()> {
        let Ok(url) = std::env::var("QUESTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@quests.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let slug = format!("q{}", &Uuid::new_v4().simple().to_string()[..10]);
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Quester', 'claude', 'opus', $2) RETURNING id",
        )
        .bind(&slug)
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let main = store
            .create_session(NewSession { agent_id: agent, role: "main", backend: "claude", model: "opus", parent_session_id: None, task_id: None, working_dir: None, usage_baseline: json!({}) })
            .await?;
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Plan me', 'in_progress') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;

        assert!(!store.quest_options(task).await?.ci_feedback);
        store.set_quest_options(task, &QuestChoice::from_args(&json!({ "ci_feedback": true })).map_err(anyhow::Error::msg)?.validate().map_err(anyhow::Error::msg)?).await?;
        assert!(store.quest_options(task).await?.ci_feedback);
        let both = ValidChoice { plan_first: Some(true), attempts: Some(2), ..Default::default() };
        assert!(store.set_quest_options(task, &both).await.is_err());
        store.set_quest_options(task, &ValidChoice { plan_first: Some(true), ..Default::default() }).await?;
        assert!(store.quest_options(task).await?.plan_first);

        let session = task_session(&store, agent, main.id, task, "tasks/x/repo").await?;
        store.begin_plan(session.id).await?;
        let run = store
            .create_run(crate::world::NewRun { session_id: session.id, agent_id: agent, task_id: Some(task), kind: "task", status: "completed", fork_from: None, prompt: "plan", timeout_secs: 60 })
            .await?;
        assert!(store.plan_ready(session.id, run.created_at).await?);
        assert!(!store.plan_ready(session.id, run.created_at).await?);
        store.post_plan(agent, "Quester", task, "Plan me", session.id, run.id, "1. Do it").await?;
        store.post_plan(agent, "Quester", task, "Plan me", session.id, run.id, "1. Do it").await?;
        let card: Uuid = sqlx::query_scalar("SELECT id FROM agent_messages WHERE agent_id = $1 AND meta->'plan'->>'session_id' = $2")
            .bind(agent)
            .bind(session.id.to_string())
            .fetch_one(&pool)
            .await?;
        assert_eq!(store.get_agent(agent).await?.unwrap().status, "waiting");

        let redirected = store.answer_plan(agent, card, "Redirect", "Ana").await?;
        let PlanAnswer::Redirected { run_id, .. } = redirected else { panic!("not redirected: {redirected:?}") };
        assert_eq!(store.get_session(session.id).await?.unwrap().plan_state.as_deref(), Some("planning"));
        assert_eq!(store.answer_plan(agent, card, "Approve", "Ana").await?, PlanAnswer::Stale);
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(run_id).execute(&pool).await?;
        let replan = store.get_run(run_id).await?.unwrap();
        assert!(store.plan_ready(session.id, replan.created_at).await?);
        let followup = store.start_followup(&store.get_session(session.id).await?.unwrap(), "use a view instead", "Ana", false).await?;
        assert!(followup.prompt.ends_with(PLAN_AGAIN));
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(followup.id).execute(&pool).await?;
        assert!(store.plan_ready(session.id, followup.created_at).await?);
        let approved = store.answer_plan(agent, card, "approve", "Ana").await?;
        let PlanAnswer::Approved { run_id, .. } = approved else { panic!("not approved: {approved:?}") };
        assert_eq!(store.get_session(session.id).await?.unwrap().plan_state.as_deref(), Some("approved"));
        assert!(store.get_run(run_id).await?.unwrap().prompt.starts_with("[plan approved by Ana]"));
        assert_eq!(store.answer_plan(agent, Uuid::new_v4(), "approve", "Ana").await?, PlanAnswer::NotAPlan);
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE session_id = $1").bind(session.id).execute(&pool).await?;

        let race: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Race me', 'in_progress') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let one = task_session(&store, agent, main.id, race, "tasks/r/repo").await?;
        let two = task_session(&store, agent, main.id, race, "tasks/r/repo-2").await?;
        store.mark_attempt(one.id, 1).await?;
        store.mark_attempt(two.id, 2).await?;
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, cost_usd) VALUES ($1, $2, $3, 'task', 'completed', 'x', 1.5)")
            .bind(one.id)
            .bind(agent)
            .bind(race)
            .execute(&pool)
            .await?;
        assert_eq!(store.finish_attempt(one.id, true).await?, AttemptsOutcome::Waiting);
        assert_eq!(store.finish_attempt(one.id, true).await?, AttemptsOutcome::Settled);
        assert_eq!(store.finish_attempt(two.id, true).await?, AttemptsOutcome::Compare(vec![one.id, two.id]));
        let lost = store.settle_attempts(race, two.id).await?;
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].session_id, one.id);
        assert!((lost[0].cost_usd - 1.5).abs() < 1e-9);
        let loser = store.get_session(one.id).await?.unwrap();
        assert_eq!((loser.status.as_str(), loser.attempt_state.as_deref()), ("archived", Some("lost")));
        assert!(!store.get_session(two.id).await?.unwrap().racing());

        let milestones = MilestoneStore::new(pool.clone());
        let merge = milestones.add(race, &[NewMilestone { title: "merge".into(), kind: "merge".into() }]).await?;
        let first = milestones.reopen_for_ci(race, merge[0].id, "abc123", &json!(["CI / test"]), "fix ci").await?;
        let first = first.expect("ci follow-up queued");
        let reopened = store.get_run(first).await?.unwrap();
        assert_eq!(reopened.session_id, two.id);
        assert!(milestones.reopen_for_ci(race, merge[0].id, "abc123", &json!([]), "fix ci").await?.is_none());
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(first).execute(&pool).await?;
        assert!(milestones.ci_seen(race, "abc123").await?);
        assert!(milestones.reopen_for_ci(race, merge[0].id, "abc123", &json!([]), "fix ci").await?.is_none());
        assert!(milestones.reopen_for_ci(race, merge[0].id, "def456", &json!([]), "fix ci").await?.is_some());
        assert!(milestones.ci_rounds(race).await? >= 1);
        assert!(milestones.ci_give_up(race, "gaveup1", &json!(["CI / test"])).await?);
        assert!(!milestones.ci_give_up(race, "gaveup1", &json!([])).await?);
        assert!(milestones.ci_seen(race, "gaveup1").await?);
        Ok(())
    }

    #[tokio::test]
    async fn quest_pace_claims_assignments_and_timing() -> Result<()> {
        let Ok(url) = std::env::var("QUESTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@pace.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let slug = format!("p{}", &Uuid::new_v4().simple().to_string()[..10]);
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Pacer', 'claude', 'opus', $2) RETURNING id",
        )
        .bind(&slug)
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let main = store
            .create_session(NewSession { agent_id: agent, role: "main", backend: "claude", model: "opus", parent_session_id: None, task_id: None, working_dir: None, usage_baseline: json!({}) })
            .await?;
        let quest = |title: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, $2, 'todo') RETURNING id")
                    .bind(user)
                    .bind(title)
                    .fetch_one(&pool)
                    .await
            }
        };
        let calm = quest("Calm").await?;
        let rush = quest("Rush").await?;
        assert_eq!(store.quest_options(calm).await?.pace, "normal");
        assert!(store.quest_options(calm).await?.reports_back());
        let urgent = QuestChoice { pace: Some("urgent".into()), ..Default::default() }.validate().map_err(anyhow::Error::msg)?;
        store.set_quest_options(rush, &urgent).await?;
        let options = store.quest_options(rush).await?;
        assert!(options.urgent() && !options.reports_back() && options.fast_for("claude") && !options.fast_for("codex"));
        store.set_quest_options(calm, &ValidChoice { pace: Some("quick".into()), ..Default::default() }).await?;
        assert_eq!(store.quest_options(calm).await?.effort_for("claude").as_deref(), Some("medium"));
        assert!(store.set_quest_options(calm, &ValidChoice { plan_first: Some(true), ..Default::default() }).await.is_err());
        assert!(sqlx::query("UPDATE agent_tasks SET attempts = 2 WHERE id = $1").bind(calm).execute(&pool).await.is_err());
        assert!(store.any_urgent(&[calm, rush]).await? && !store.any_urgent(&[calm]).await?);

        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = ANY($1)")
            .bind(vec![calm, rush])
            .bind(agent)
            .execute(&pool)
            .await?;
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.assigned' AND status = 'pending'")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(pending, 2);
        let calm_session = task_session(&store, agent, main.id, calm, "tasks/c/repo").await?;
        let rush_session = task_session(&store, agent, main.id, rush, "tasks/u/repo").await?;
        let new_run = |session: Uuid, task: Uuid| crate::world::NewRun { session_id: session, agent_id: agent, task_id: Some(task), kind: "task", status: "queued", fork_from: None, prompt: "go", timeout_secs: 60 };
        let calm_run = store.create_run(new_run(calm_session.id, calm)).await?;
        let rush_run = store.create_run(new_run(rush_session.id, rush)).await?;
        assert_eq!(store.drop_started_assignments(agent).await?, 2);
        assert!(store.take_pending_inbox(agent, 20).await?.is_empty());

        let mut order = Vec::new();
        for _ in 0..200 {
            let Some(run) = store.claim_queued_run("pace-test").await? else { break };
            if run.id == calm_run.id || run.id == rush_run.id {
                order.push(run.id);
            }
            if order.len() == 2 {
                break;
            }
        }
        assert_eq!(order, vec![rush_run.id, calm_run.id]);

        sqlx::query("UPDATE session_runs SET status = 'completed', created_at = NOW() - INTERVAL '100 seconds', started_at = NOW() - INTERVAL '90 seconds', finished_at = NOW() - INTERVAL '30 seconds' WHERE id = $1")
            .bind(rush_run.id)
            .execute(&pool)
            .await?;
        let timing = store.quest_timing(rush_session.id).await?;
        assert_eq!(timing["queued_secs"].as_f64(), Some(10.0));
        assert_eq!(timing["work_secs"].as_f64(), Some(60.0));
        assert!(timing["total_secs"].as_f64().unwrap_or(0.0) >= 100.0);
        assert_eq!(timing["review_secs"].as_f64(), Some(0.0));

        let notes = store.record_repo_notes(user, agent, "acme/app", Some(rush), &["Tests: cargo test -p app".into()]).await?.expect("notes created");
        assert_eq!(notes.title, "Repo notes: acme/app");
        assert!(store.record_repo_notes(user, agent, "acme/app", Some(rush), &["tests: cargo test -p app".into()]).await?.is_none());
        let more = store.record_repo_notes(user, agent, "acme/app", None, &["Docs live in docs/".into()]).await?.expect("notes updated");
        assert_eq!(more.id, notes.id);
        assert!(store.repo_notes(user, None, "acme/app").await?.unwrap().body.contains("Docs live in docs/"));
        Ok(())
    }

    #[test]
    fn approval_words() {
        for yes in ["Approve", "approve.", " LGTM ", "go ahead", "approve with the smaller migration"] {
            assert!(is_approval(yes), "{yes}");
        }
        for no in ["Redirect", "approve? not yet", "no", "use the other table", ""] {
            assert!(!is_approval(no), "{no}");
        }
        assert!(!is_approval("Redirect"));
        assert!(!is_approval("disapprove"));
    }

    #[test]
    fn redirect_without_text_still_explains() {
        assert!(plan_redirect_prompt("Ana", "Redirect").contains("did not approve"));
        assert!(plan_redirect_prompt("Ana", "keep the old API").contains("keep the old API"));
        assert!(plan_redirect_prompt("Ana", "x").ends_with(PLAN_AGAIN));
    }

    #[test]
    fn choices_validate() {
        let ok = QuestChoice::from_args(&json!({ "plan_first": false, "attempts": 3, "attempt_variants": [{"cli": "codex", "model": "gpt-5.5"}, "opus"] }))
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(ok.plan_first, Some(false));
        assert_eq!(ok.attempts, Some(3));
        assert_eq!(ok.variants.as_ref().unwrap()[1], ReviewerChoice { cli: None, model: "opus".into() });
        let inferred = QuestChoice { attempt_variants: Some(vec![json!("opus")]), ..Default::default() }.validate().unwrap();
        assert_eq!(inferred.attempts, Some(2));
        assert!(QuestChoice { attempts: Some(4), ..Default::default() }.validate().is_err());
        assert!(QuestChoice { attempts: Some(0), ..Default::default() }.validate().is_err());
        assert!(QuestChoice { attempts: Some(2), attempt_variants: Some(vec![json!("a"), json!("b"), json!("c")]), ..Default::default() }.validate().is_err());
        assert!(QuestChoice { attempt_variants: Some(vec![json!({"cli": "gemini", "model": "x"})]), ..Default::default() }.validate().is_err());
        assert!(QuestChoice::from_args(&json!({ "plan_first": "yes" })).is_err());
        assert!(QuestChoice { plan_first: Some(true), attempts: Some(2), ..Default::default() }.validate().is_err());
        assert!(QuestChoice::default().validate().unwrap().is_empty());
        assert_eq!(QuestChoice::from_args(&json!({ "pace": " Urgent " })).unwrap().validate().unwrap().pace.as_deref(), Some("urgent"));
        assert!(QuestChoice::from_args(&json!({ "pace": "slow" })).unwrap().validate().is_err());
        assert!(QuestChoice::from_args(&json!({ "pace": 3 })).is_err());
        assert!(QuestChoice { pace: Some("quick".into()), plan_first: Some(true), ..Default::default() }.validate().is_err());
        assert!(QuestChoice { pace: Some("quick".into()), attempts: Some(2), ..Default::default() }.validate().is_err());
        assert!(QuestChoice { pace: Some("urgent".into()), attempts: Some(2), ..Default::default() }.validate().is_ok());
    }

    #[test]
    fn variants_are_per_attempt() {
        let options = QuestOptions { plan_first: false, attempts: 3, attempt_variants: json!([{"model": "a"}, {"cli": "codex", "model": "b"}]), ci_feedback: false, pace: "normal".into() };
        assert_eq!(options.variant(1).unwrap().model, "a");
        assert_eq!(options.variant(2).unwrap().cli.as_deref(), Some("codex"));
        assert!(options.variant(3).is_none());
        assert!(options.racing());
    }

    #[test]
    fn attempts_settle_only_when_all_are_done() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let s = |id: Uuid, st: &str| (id, st.to_string());
        assert_eq!(attempts_outcome(&[s(a, "finished"), s(b, "running")]), AttemptsOutcome::Waiting);
        assert_eq!(attempts_outcome(&[s(a, "failed"), s(b, "finished")]), AttemptsOutcome::Winner(b));
        assert_eq!(attempts_outcome(&[s(a, "finished"), s(b, "failed"), s(c, "finished")]), AttemptsOutcome::Compare(vec![a, c]));
        assert_eq!(attempts_outcome(&[s(a, "failed"), s(b, "failed")]), AttemptsOutcome::NoneFinished);
    }
}
