use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::git_names::TrackedPr;

pub const KINDS: &[&str] = &["work", "merge"];

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Milestone {
    pub id: Uuid,
    pub task_id: Uuid,
    pub position: i32,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub detail: Option<String>,
    pub data: Value,
    pub next_check_at: Option<DateTime<Utc>>,
    pub done_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewMilestone {
    pub title: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "work".to_string()
}

#[derive(Debug, Clone, FromRow)]
pub struct DueMerge {
    pub id: Uuid,
    pub task_id: Uuid,
    pub data: Value,
    pub pr_url: Option<String>,
    pub repo: Option<String>,
    pub title: String,
    pub agent_id: Option<Uuid>,
}

pub const SEED_MAX: usize = 8;
pub const CI_ROUNDS_MAX: i64 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrObservationOrder {
    Older,
    Same,
    Newer,
}

pub(crate) fn pr_observation_time(data: &Value, pr: TrackedPr<'_>) -> Option<f64> {
    let recheck_pr = data
        .get("recheck_pr_url")
        .and_then(Value::as_str)
        .and_then(TrackedPr::parse);
    let closed_pr = data.get("closed_pr_url").and_then(Value::as_str).and_then(TrackedPr::parse);
    let recheck_matches = recheck_pr
        .map(|stored| stored.same_identity(pr))
        .unwrap_or(closed_pr.is_none_or(|stored| stored.same_identity(pr)));
    [
        data.get("recheck_after")
            .and_then(Value::as_f64)
            .filter(|_| recheck_matches),
        data.get("closed_at")
            .and_then(Value::as_f64)
            .filter(|_| closed_pr.is_some_and(|stored| stored.same_identity(pr))),
    ]
    .into_iter()
    .flatten()
    .max_by(f64::total_cmp)
}

pub(crate) fn pr_observation_order(data: &Value, pr: TrackedPr<'_>, observed_at: DateTime<Utc>) -> PrObservationOrder {
    let latest = pr_observation_time(data, pr);
    let observed = observed_at.timestamp_micros() as f64 / 1_000_000.0;
    match latest.and_then(|latest| latest.partial_cmp(&observed)) {
        Some(std::cmp::Ordering::Less) | None => PrObservationOrder::Newer,
        Some(std::cmp::Ordering::Equal) => PrObservationOrder::Same,
        Some(std::cmp::Ordering::Greater) => PrObservationOrder::Older,
    }
}

pub fn merge_title() -> &'static str {
    "Get the PR reviewed and merged"
}

pub fn check_every_secs() -> i64 {
    std::env::var("WORLD_MERGE_CHECK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &i64| *v >= 60)
        .unwrap_or(1800)
}

pub(crate) async fn arm_merge_in(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, task_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "WITH task AS MATERIALIZED (
           SELECT id, status, pr_url, merged_pr_url FROM agent_tasks WHERE id = $1 FOR UPDATE
         )
         UPDATE task_milestones m
         SET status = CASE WHEN t.pr_url IS NULL THEN 'skipped' ELSE 'active' END,
             detail = CASE WHEN t.pr_url IS NULL THEN 'No pull request was opened' ELSE 'Waiting for review' END,
             next_check_at = CASE
               WHEN t.pr_url IS NULL THEN NULL
               WHEN m.status = 'active' AND m.next_check_at IS NOT NULL
                 THEN LEAST(m.next_check_at, NOW() + make_interval(secs => $2))
               ELSE NOW() + make_interval(secs => $2)
             END,
             data = CASE WHEN t.pr_url IS NULL THEN m.data
               ELSE (CASE WHEN m.status = 'active' THEN m.data ELSE m.data - 'runner_next_check_at' END)
                 || jsonb_build_object('seen_at', COALESCE(m.data->'seen_at', to_jsonb(NOW())))
             END,
             done_at = CASE WHEN t.pr_url IS NULL THEN m.done_at ELSE NULL END,
             updated_at = NOW()
         FROM task t
         WHERE m.task_id = t.id AND m.kind = 'merge' AND t.status NOT IN ('done', 'cancelled')
           AND (
             t.pr_url IS NULL AND m.status IN ('pending', 'active')
             OR t.pr_url IS NOT NULL AND t.merged_pr_url IS DISTINCT FROM t.pr_url AND (
               m.status IN ('pending', 'active')
               OR m.status = 'skipped' AND m.detail = 'No pull request was opened'
             )
           )
         RETURNING t.pr_url IS NOT NULL",
    )
    .bind(task_id)
    .bind(check_every_secs() as f64)
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or(false))
}

pub(crate) async fn complete_work_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_id: Uuid,
    positions: &[i32],
    titles: &[String],
    all: bool,
) -> Result<()> {
    let titles: Vec<String> = titles
        .iter()
        .map(|title| title.trim().to_lowercase())
        .filter(|title| !title.is_empty())
        .collect();
    sqlx::query(
        "UPDATE task_milestones
         SET status = 'done', done_at = NOW(), updated_at = NOW()
         WHERE task_id = $1 AND kind = 'work' AND status IN ('pending', 'active')
           AND ($3 OR position + 1 = ANY($2) OR lower(btrim(title)) = ANY($4))",
    )
    .bind(task_id)
    .bind(positions)
    .bind(all)
    .bind(&titles)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Clone)]
pub struct MilestoneStore {
    pool: PgPool,
}

impl MilestoneStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn for_tasks(&self, task_ids: &[Uuid]) -> Result<Vec<Milestone>> {
        Ok(sqlx::query_as::<_, Milestone>(
            "SELECT * FROM task_milestones WHERE task_id = ANY($1) ORDER BY task_id, position, created_at",
        )
        .bind(task_ids)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn for_task(&self, task_id: Uuid) -> Result<Vec<Milestone>> {
        self.for_tasks(&[task_id]).await
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Milestone>> {
        Ok(sqlx::query_as::<_, Milestone>("SELECT * FROM task_milestones WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn add(&self, task_id: Uuid, items: &[NewMilestone]) -> Result<Vec<Milestone>> {
        let mut out = Vec::new();
        for item in items {
            let title = item.title.trim();
            if title.is_empty() {
                continue;
            }
            if !KINDS.contains(&item.kind.as_str()) {
                bail!("unknown milestone kind {}", item.kind);
            }
            let row = sqlx::query_as::<_, Milestone>(
                "INSERT INTO task_milestones (task_id, title, kind, position)
                 VALUES ($1, $2, $3, CASE WHEN $3 = 'merge' THEN 1000 ELSE 0 END
                   + COALESCE((SELECT MAX(position) + 1 FROM task_milestones WHERE task_id = $1 AND kind = $3), 0))
                 RETURNING *",
            )
            .bind(task_id)
            .bind(title.chars().take(200).collect::<String>())
            .bind(&item.kind)
            .fetch_one(&self.pool)
            .await?;
            out.push(row);
        }
        Ok(out)
    }

    pub async fn seed(&self, task_id: Uuid, agent_id: Uuid, titles: &[String]) -> Result<usize> {
        let items: Vec<NewMilestone> = titles
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .take(SEED_MAX)
            .map(|t| NewMilestone { title: t.to_string(), kind: "work".to_string() })
            .collect();
        if items.is_empty() {
            return Ok(0);
        }
        let open: bool = sqlx::query_scalar(
            "SELECT EXISTS (
               SELECT 1 FROM agent_tasks t
               WHERE t.id = $1 AND t.assigned_agent_id = $2
                 AND NOT EXISTS (SELECT 1 FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'work'))",
        )
        .bind(task_id)
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?;
        if !open {
            return Ok(0);
        }
        Ok(self.add(task_id, &items).await?.len())
    }

    pub async fn update(&self, id: Uuid, title: Option<&str>, status: Option<&str>) -> Result<Option<Milestone>> {
        if let Some(status) = status {
            if !matches!(status, "pending" | "done" | "skipped") {
                bail!("milestones can only be set to pending, done or skipped");
            }
        }
        let row = sqlx::query_as::<_, Milestone>(
            "UPDATE task_milestones
             SET title = COALESCE(NULLIF(BTRIM($2), ''), title),
                 status = COALESCE($3, status),
                 done_at = CASE WHEN $3 = 'done' THEN NOW() WHEN $3 IS NULL THEN done_at ELSE NULL END,
                 next_check_at = CASE WHEN $3 IS NULL THEN next_check_at ELSE NULL END,
                 detail = CASE WHEN $3 IS NULL THEN detail ELSE NULL END,
                 updated_at = NOW()
             WHERE id = $1
             RETURNING *",
        )
        .bind(id)
        .bind(title)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM task_milestones WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn complete_work(&self, task_id: Uuid, positions: &[i32], titles: &[String], all: bool) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        complete_work_in(&mut tx, task_id, positions, titles, all).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn arm_merge(&self, task_id: Uuid) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let watching = arm_merge_in(&mut tx, task_id).await?;
        tx.commit().await?;
        Ok(watching)
    }

    pub async fn rearm_open_pr(&self, task_id: Uuid, pr_url: &str, observed_at: DateTime<Utc>) -> Result<bool> {
        let Some(observed_pr) = TrackedPr::parse(pr_url) else {
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, String, Option<String>, Value)>(
            "SELECT m.id, t.status, t.pr_url, t.merged_pr_url, m.status, m.detail, m.data
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1 FOR UPDATE OF t, m",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((milestone_id, task_status, tracked_url, merged_url, status, detail, data)) = row else {
            return Ok(false);
        };
        let Some(tracked_pr) = tracked_url.as_deref().and_then(TrackedPr::parse) else {
            return Ok(false);
        };
        if matches!(task_status.as_str(), "done" | "cancelled")
            || !tracked_pr.same_identity(observed_pr)
            || merged_url.as_deref().and_then(TrackedPr::parse).is_some_and(|merged| merged.same_identity(tracked_pr))
            || pr_observation_order(&data, tracked_pr, observed_at.to_owned()) != PrObservationOrder::Newer
        {
            return Ok(false);
        }
        let transition = status == "pending"
            || status == "skipped" && detail.as_deref() == Some("No pull request was opened")
            || status == "failed" && detail.as_deref() == Some("Closed without merging");
        if status != "active" && !transition {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE task_milestones
             SET status = CASE WHEN $4 THEN 'active' ELSE status END,
                 detail = CASE WHEN $4 THEN 'Waiting for review' ELSE detail END,
                 data = (CASE WHEN $4 THEN data - 'runner_next_check_at' ELSE data END)
                   || jsonb_build_object(
                     'recheck_after', EXTRACT(EPOCH FROM $2::timestamptz),
                     'recheck_pr_url', $3::text,
                     'seen_at', COALESCE(data->'seen_at', to_jsonb(NOW()))
                   ),
                 done_at = CASE WHEN $4 THEN NULL ELSE done_at END,
                 next_check_at = CASE WHEN $4 THEN NOW() ELSE next_check_at END,
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(milestone_id)
        .bind(observed_at)
        .bind(tracked_pr.url())
        .bind(transition)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn claim_due(&self, limit: i64) -> Result<Vec<DueMerge>> {
        Ok(sqlx::query_as::<_, DueMerge>(
            "WITH due AS (
               SELECT m.id FROM task_milestones m
               WHERE m.kind = 'merge' AND m.status = 'active'
                 AND (m.next_check_at IS NULL OR m.next_check_at <= NOW())
               ORDER BY m.next_check_at NULLS FIRST
               FOR UPDATE SKIP LOCKED
               LIMIT $1
             ), bumped AS (
               UPDATE task_milestones m
               SET next_check_at = NOW() + make_interval(secs => $2), updated_at = NOW()
               FROM due WHERE m.id = due.id
               RETURNING m.id, m.task_id, m.data
             )
             SELECT b.id, b.task_id, b.data, t.pr_url, t.repo, t.title, t.assigned_agent_id AS agent_id
             FROM bumped b JOIN agent_tasks t ON t.id = b.task_id",
        )
        .bind(limit)
        .bind(check_every_secs() as f64)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn check_now(&self, task_id: Uuid) -> Result<bool> {
        self.schedule_check(task_id, false).await
    }

    pub async fn retry_check_now(&self, task_id: Uuid) -> Result<bool> {
        self.schedule_check(task_id, true).await
    }

    async fn schedule_check(&self, task_id: Uuid, retry_failed: bool) -> Result<bool> {
        Ok(sqlx::query(
            "WITH task AS MATERIALIZED (
               SELECT id, status, pr_url, merged_pr_url FROM agent_tasks WHERE id = $1 FOR UPDATE
             ), latest_close AS (
               SELECT payload->>'pr_url' AS pr_url FROM world_events
               WHERE task_id = $1 AND type = 'task.pr_closed' ORDER BY seq DESC LIMIT 1
             )
             UPDATE task_milestones m
             SET status = 'active',
                 detail = CASE WHEN m.status IN ('failed', 'skipped') THEN 'Waiting for review' ELSE COALESCE(m.detail, 'Waiting for review') END,
                 data = (m.data - 'runner_next_check_at')
                   || jsonb_build_object('seen_at', COALESCE(m.data->'seen_at', to_jsonb(NOW())))
                   || CASE WHEN $2 AND m.status = 'failed' AND m.detail = 'Closed without merging'
                       THEN jsonb_build_object(
                         'recheck_after', GREATEST(
                           CASE WHEN lower(m.data->>'recheck_pr_url') = lower(t.pr_url)
                             OR NOT (m.data ? 'recheck_pr_url') AND lower(m.data->>'closed_pr_url') = lower(t.pr_url)
                             THEN COALESCE((m.data->>'recheck_after')::double precision, '-Infinity'::double precision)
                             ELSE '-Infinity'::double precision END,
                           EXTRACT(EPOCH FROM NOW())
                         ),
                         'recheck_pr_url', t.pr_url
                       )
                     ELSE '{}'::jsonb END,
                 done_at = CASE WHEN m.status IN ('failed', 'skipped') THEN NULL ELSE m.done_at END,
                 next_check_at = NOW(), updated_at = NOW()
             FROM task t
             WHERE m.task_id = t.id AND m.kind = 'merge' AND t.pr_url IS NOT NULL AND t.status NOT IN ('done', 'cancelled')
               AND (
                 m.status IN ('pending', 'active')
                 OR m.status = 'skipped' AND m.detail = 'No pull request was opened'
                 OR $2 AND m.status = 'failed' AND m.detail = 'Closed without merging'
                   AND t.status <> 'done' AND t.merged_pr_url IS DISTINCT FROM t.pr_url
                   AND (
                     lower(m.data->>'closed_pr_url') = lower(t.pr_url)
                     OR NOT (m.data ? 'closed_pr_url')
                       AND (NOT EXISTS (SELECT 1 FROM latest_close) OR lower((SELECT pr_url FROM latest_close)) = lower(t.pr_url))
                   )
               )",
        )
        .bind(task_id)
        .bind(retry_failed)
        .execute(&self.pool)
        .await?
        .rows_affected()
            > 0)
    }

    pub async fn note(&self, id: Uuid, status: &str, detail: &str, data: Value) -> Result<()> {
        sqlx::query(
            "UPDATE task_milestones
             SET status = $2, detail = $3, data = data || $4,
                 done_at = CASE WHEN $2 = 'done' THEN NOW() ELSE done_at END,
                 next_check_at = CASE WHEN $2 = 'active' THEN next_check_at ELSE NULL END,
                 updated_at = NOW()
             WHERE id = $1 AND (kind <> 'merge' OR status = 'active')",
        )
        .bind(id)
        .bind(status)
        .bind(detail)
        .bind(data)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn merge_done(&self, task_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM task_milestones WHERE task_id = $1 AND kind = 'merge' AND status = 'done')",
        )
        .bind(task_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn open_count(&self, task_id: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM task_milestones WHERE task_id = $1 AND status IN ('pending', 'active')",
        )
        .bind(task_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn reopen_for_feedback(&self, task_id: Uuid, milestone_id: Uuid, prompt: &str) -> Result<Option<Uuid>> {
        self.reopen(task_id, milestone_id, prompt, None).await
    }

    pub async fn ci_seen(&self, task_id: Uuid, head_sha: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM quest_ci_feedback WHERE task_id = $1 AND head_sha = $2)",
        )
        .bind(task_id)
        .bind(head_sha)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn ci_rounds(&self, task_id: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM quest_ci_feedback WHERE task_id = $1 AND run_id IS NOT NULL")
            .bind(task_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn ci_give_up(&self, task_id: Uuid, head_sha: &str, jobs: &Value) -> Result<bool> {
        Ok(sqlx::query("INSERT INTO quest_ci_feedback (task_id, head_sha, jobs) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(task_id)
            .bind(head_sha)
            .bind(jobs)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn reopen_for_ci(&self, task_id: Uuid, milestone_id: Uuid, head_sha: &str, jobs: &Value, prompt: &str) -> Result<Option<Uuid>> {
        self.reopen(task_id, milestone_id, prompt, Some((head_sha, jobs))).await
    }

    async fn reopen(&self, task_id: Uuid, milestone_id: Uuid, prompt: &str, ci: Option<(&str, &Value)>) -> Result<Option<Uuid>> {
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_tasks
             WHERE id = $1 AND status NOT IN ('done', 'cancelled')
             FOR UPDATE SKIP LOCKED",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?;
        if task.is_none() {
            return Ok(None);
        }
        let session = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT s.id, s.agent_id
             FROM (
               SELECT id FROM agent_sessions
               WHERE task_id = $1 AND role = 'task' AND status <> 'archived'
               ORDER BY created_at DESC LIMIT 1
             ) latest
             JOIN agent_sessions s ON s.id = latest.id
             JOIN task_milestones m ON m.id = $2 AND m.task_id = $1 AND m.kind = 'merge' AND m.status = 'active'
             WHERE s.status <> 'archived'
             FOR UPDATE OF s, m SKIP LOCKED",
        )
        .bind(task_id)
        .bind(milestone_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((session_id, agent_id)) = session else {
            return Ok(None);
        };
        let busy = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM session_runs WHERE task_id = $1 AND status IN ('queued', 'running'))",
        )
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        if busy {
            return Ok(None);
        }
        if let Some((head_sha, jobs)) = ci {
            let fresh = sqlx::query(
                "INSERT INTO quest_ci_feedback (task_id, head_sha, jobs) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(task_id)
            .bind(head_sha)
            .bind(jobs)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if fresh == 0 {
                return Ok(None);
            }
        }
        let activity = if ci.is_some() { "Fixing failing CI" } else { "Addressing PR review feedback" };
        sqlx::query("UPDATE agent_sessions SET status = 'active', review_rounds = 0, updated_at = NOW() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        crate::world::reopen_quest(&mut tx, Some(task_id)).await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, activity)
             VALUES ($1, $2, $3, 'followup', 'queued', $4, 3600, $5)
             RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(prompt)
        .bind(activity)
        .fetch_one(&mut *tx)
        .await?;
        if let Some((head_sha, _)) = ci {
            sqlx::query("UPDATE quest_ci_feedback SET run_id = $3 WHERE task_id = $1 AND head_sha = $2")
                .bind(task_id)
                .bind(head_sha)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE task_milestones SET detail = $2, updated_at = NOW()
             WHERE id = $1",
        )
        .bind(milestone_id)
        .bind(if ci.is_some() { "Fixing failing CI" } else { "Addressing review feedback" })
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(run_id))
    }
}

pub fn finished_status(failed: bool, merged: bool, has_pr: bool, changes_requested: bool) -> &'static str {
    if failed {
        "failed"
    } else if merged {
        "done"
    } else if has_pr || changes_requested {
        "review"
    } else {
        "done"
    }
}

pub fn prompt_block(milestones: &[Milestone]) -> String {
    let work: Vec<&Milestone> = milestones.iter().filter(|m| m.kind == "work").collect();
    let merge = milestones.iter().any(|m| m.kind == "merge" && m.status != "skipped");
    if work.is_empty() && !merge {
        return String::new();
    }
    let mut lines = vec!["Milestones for this quest:".to_string()];
    for m in &work {
        let mark = if m.status == "done" { "x" } else { " " };
        lines.push(format!("{}. [{mark}] {}", m.position + 1, m.title));
    }
    if !work.is_empty() {
        lines.push("Work through them in order. In your report block add \"milestones_done\": [numbers] for every milestone you finished.".to_string());
    }
    if merge {
        lines.push("After you report done, Loka opens the PR and watches it until it merges. Review feedback on the PR comes back to this session as a follow-up.".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn a_merged_quest_never_goes_back_to_review() {
        assert_eq!(finished_status(false, false, true, false), "review");
        assert_eq!(finished_status(false, false, false, true), "review");
        assert_eq!(finished_status(false, false, false, false), "done");
        assert_eq!(finished_status(true, false, true, false), "failed");
        assert_eq!(finished_status(true, true, true, true), "failed");
        assert_eq!(finished_status(false, true, true, false), "done");
        assert_eq!(finished_status(false, true, false, true), "done");
    }

    #[tokio::test]
    async fn late_pr_rearms_only_the_no_pr_skip() -> Result<()> {
        let Ok(url) = std::env::var("MILESTONES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("late-pr-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, user, None).await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo) VALUES ($1, 'late PR', 'failed', 'o/r') RETURNING id",
        )
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let store = MilestoneStore::new(pool.clone());
        let milestone = store
            .add(task, &[NewMilestone { title: merge_title().into(), kind: "merge".into() }])
            .await?
            .pop()
            .expect("merge milestone");
        assert_eq!(store.get(milestone.id).await?.expect("pending milestone").status, "pending");
        assert!(!store.arm_merge(task).await?);
        let skipped = store.get(milestone.id).await?.expect("skipped milestone");
        assert_eq!((skipped.status.as_str(), skipped.detail.as_deref()), ("skipped", Some("No pull request was opened")));
        assert!(skipped.next_check_at.is_none());
        assert!(!store.retry_check_now(task).await?);

        sqlx::query("UPDATE task_milestones SET data = jsonb_build_object('runner_next_check_at', 9999999999) WHERE id = $1")
            .bind(milestone.id)
            .execute(&pool)
            .await?;
        let db = crate::db::Database::from_pool(pool.clone());
        let mut incomplete = std::collections::HashMap::from([
            ("title".to_string(), serde_json::json!("must roll back")),
            ("pr_url".to_string(), serde_json::json!("https://github.com/o/r/pull/9")),
        ]);
        let error = db.update_task(task, user, &incomplete).await.expect_err("incomplete PR pair");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::InvalidPullRequest)));
        incomplete.insert("pr_number".to_string(), serde_json::json!(9));
        incomplete.insert("pr_url".to_string(), serde_json::json!("https://github.com/o/other/pull/9"));
        let error = db.update_task(task, user, &incomplete).await.expect_err("wrong repository");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::InvalidPullRequest)));
        let untouched = db.get_task(task).await?.expect("untouched task");
        assert_eq!((untouched.title.as_str(), untouched.pr_url), ("late PR", None));

        let attach = std::collections::HashMap::from([
            ("title".to_string(), serde_json::json!("late PR attached")),
            ("pr_url".to_string(), serde_json::json!("https://github.com/o/r/pull/9")),
            ("pr_number".to_string(), serde_json::json!(9)),
        ]);
        let error = db
            .update_task_with_settings(task, user, &attach, Some(Some(0)), None)
            .await
            .expect_err("invalid limit");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::InvalidSettings)));
        let invalid_limit_task = db.get_task(task).await?.expect("task after invalid limit");
        let invalid_limit_milestone = store.get(milestone.id).await?.expect("milestone after invalid limit");
        assert_eq!((invalid_limit_task.title.as_str(), invalid_limit_task.pr_url, invalid_limit_task.time_limit_secs), ("late PR", None, None));
        assert_eq!((invalid_limit_milestone.status.as_str(), invalid_limit_milestone.detail.as_deref()), ("skipped", Some("No pull request was opened")));
        assert!(invalid_limit_milestone.next_check_at.is_none());
        assert_eq!(invalid_limit_milestone.data.get("runner_next_check_at").and_then(Value::as_i64), Some(9_999_999_999));

        let world = crate::world::WorldStore::new(pool.clone());
        world
            .set_quest_options(task, &crate::quests::ValidChoice { plan_first: Some(true), ..Default::default() })
            .await?;
        let attempts = crate::quests::ValidChoice { attempts: Some(2), ..Default::default() };
        let error = db
            .update_task_with_settings(task, user, &attach, Some(Some(15)), Some(&attempts))
            .await
            .expect_err("saved plan conflicts with attempts");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::InvalidSettings)));
        let saved_plan_task = db.get_task(task).await?.expect("task after saved plan conflict");
        let saved_plan_options = world.quest_options(task).await?;
        let saved_plan_milestone = store.get(milestone.id).await?.expect("milestone after saved plan conflict");
        assert_eq!((saved_plan_task.title.as_str(), saved_plan_task.pr_url, saved_plan_task.time_limit_secs), ("late PR", None, None));
        assert_eq!((saved_plan_options.plan_first, saved_plan_options.attempts), (true, 1));
        assert_eq!((saved_plan_milestone.status.as_str(), saved_plan_milestone.detail.as_deref()), ("skipped", Some("No pull request was opened")));
        assert!(saved_plan_milestone.next_check_at.is_none());
        assert_eq!(saved_plan_milestone.data.get("runner_next_check_at").and_then(Value::as_i64), Some(9_999_999_999));

        let options = crate::quests::ValidChoice { plan_first: Some(false), attempts: Some(2), ..Default::default() };
        assert_eq!(db.update_task_with_settings(task, user, &attach, Some(Some(15)), Some(&options)).await?, 1);
        let attached = store.get(milestone.id).await?.expect("rearmed milestone");
        assert_eq!((attached.status.as_str(), attached.detail.as_deref()), ("active", Some("Waiting for review")));
        assert!(attached.next_check_at.is_some());
        assert!(attached.data.get("runner_next_check_at").is_none());
        let attached_task = db.get_task(task).await?.expect("task with PR");
        let attached_options = world.quest_options(task).await?;
        assert_eq!(
            (
                attached_task.title.as_str(),
                attached_task.status.as_str(),
                attached_task.pr_url.as_deref(),
                attached_task.pr_number,
                attached_task.time_limit_secs,
            ),
            ("late PR attached", "failed", Some("https://github.com/o/r/pull/9"), Some(9), Some(900))
        );
        assert_eq!((attached_options.plan_first, attached_options.attempts), (false, 2));
        let duplicate = std::collections::HashMap::from([
            ("pr_url".to_string(), serde_json::json!("https://github.com/O/R/pull/9")),
            ("pr_number".to_string(), serde_json::json!(9)),
        ]);
        assert_eq!(db.update_task(task, user, &duplicate).await?, 1);
        let duplicate_task = db.get_task(task).await?.expect("duplicate task");
        let duplicate_milestone = store.get(milestone.id).await?.expect("duplicate milestone");
        assert_eq!(duplicate_task.status, "failed");
        assert_eq!(duplicate_task.pr_url.as_deref(), Some("https://github.com/o/r/pull/9"));
        assert_eq!(duplicate_milestone.next_check_at, attached.next_check_at);

        let replacement = std::collections::HashMap::from([
            ("title".to_string(), serde_json::json!("must still roll back")),
            ("pr_url".to_string(), serde_json::json!("https://github.com/o/r/pull/10")),
            ("pr_number".to_string(), serde_json::json!(10)),
        ]);
        let replacement_options = crate::quests::ValidChoice { attempts: Some(3), ..Default::default() };
        let error = db
            .update_task_with_settings(task, user, &replacement, Some(Some(20)), Some(&replacement_options))
            .await
            .expect_err("replacement PR");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::PullRequestConflict)));
        let unchanged_task = db.get_task(task).await?.expect("unchanged task");
        let unchanged_options = world.quest_options(task).await?;
        assert_eq!(
            (
                unchanged_task.title.as_str(),
                unchanged_task.pr_url.as_deref(),
                unchanged_task.pr_number,
                unchanged_task.time_limit_secs,
            ),
            ("late PR attached", Some("https://github.com/o/r/pull/9"), Some(9), Some(900))
        );
        assert_eq!((unchanged_options.plan_first, unchanged_options.attempts), (false, 2));

        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1")
        .bind(task)
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE task_milestones SET status = 'skipped', detail = 'No pull request was opened', next_check_at = NULL WHERE id = $1")
            .bind(milestone.id)
            .execute(&pool)
            .await?;
        assert!(store.retry_check_now(task).await?);
        let legacy = store.get(milestone.id).await?.expect("legacy skip rearmed");
        assert_eq!((legacy.status.as_str(), legacy.detail.as_deref()), ("active", Some("Waiting for review")));
        assert!(legacy.next_check_at.is_some());
        assert!(legacy.data.get("seen_at").is_some());

        sqlx::query("UPDATE task_milestones SET status = 'skipped', detail = 'Not needed', next_check_at = NULL WHERE id = $1")
            .bind(milestone.id)
            .execute(&pool)
            .await?;
        db.set_pr(task, "https://github.com/o/r/pull/9", 9).await?;
        assert_eq!(store.get(milestone.id).await?.expect("manual skip").detail.as_deref(), Some("Not needed"));

        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE task_milestones SET status = 'skipped', detail = 'No pull request was opened' WHERE id = $1")
            .bind(milestone.id)
            .execute(&pool)
            .await?;
        assert!(!store.retry_check_now(task).await?);

        sqlx::query("UPDATE agent_tasks SET status = 'cancelled', pr_url = NULL, pr_number = NULL WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        db.set_pr(task, "https://github.com/o/r/pull/9", 9).await?;
        let cancelled = db.get_task(task).await?.expect("cancelled task");
        assert_eq!((cancelled.status.as_str(), cancelled.pr_url), ("cancelled", None));
        let error = db.update_task(task, user, &duplicate).await.expect_err("cancelled PR attachment");
        assert!(matches!(error.downcast_ref(), Some(crate::db::UpdateTaskError::PullRequestConflict)));

        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&pool).await?;
        sqlx::query("DELETE FROM org_members WHERE user_id = $1").bind(user).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn merge_watch_lifecycle() -> Result<()> {
        let Ok(url) = std::env::var("MILESTONES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("watch-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, user, None).await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'watch', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("watch-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_url, assigned_agent_id)
             VALUES ($1, 'watch test', 'review', 'o/r', 'https://github.com/o/r/pull/9', $2) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, status) VALUES ($1, 'task', 'claude', 'sonnet', $2, 'completed') RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let store = MilestoneStore::new(pool.clone());
        assert_eq!(store.seed(task, Uuid::new_v4(), &["Not mine".into()]).await?, 0);
        assert_eq!(store.seed(task, agent, &["First".into(), " ".into(), "Second".into()]).await?, 2);
        assert_eq!(store.seed(task, agent, &["Third".into()]).await?, 0);
        store
            .add(task, &[NewMilestone { title: "Step".into(), kind: "work".into() }, NewMilestone { title: merge_title().into(), kind: "merge".into() }])
            .await?;
        store.complete_work(task, &[], &[], true).await?;
        assert!(store.arm_merge(task).await?);
        let active_id: Uuid = sqlx::query_scalar("SELECT id FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        let active_next = Utc::now() + chrono::Duration::minutes(10);
        sqlx::query(
            "UPDATE task_milestones
             SET detail = 'Still watching', next_check_at = $2,
                 data = data || jsonb_build_object('runner_next_check_at', 9999999999, 'runner_pr_claimed_at', 9999999998)
             WHERE id = $1",
        )
        .bind(active_id)
        .bind(active_next)
        .execute(&pool)
        .await?;
        let open_observed = Utc::now();
        assert!(store.rearm_open_pr(task, "https://github.com/O/R/pull/9", open_observed).await?);
        assert!(!store.rearm_open_pr(task, "https://github.com/o/r/pull/9", open_observed).await?);
        assert!(!store.rearm_open_pr(task, "https://github.com/o/r/pull/9", open_observed - chrono::Duration::seconds(1)).await?);
        let active: (String, Option<String>, Option<DateTime<Utc>>, Value) =
            sqlx::query_as("SELECT status, detail, next_check_at, data FROM task_milestones WHERE id = $1")
                .bind(active_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!((active.0.as_str(), active.1.as_deref(), active.2), ("active", Some("Still watching"), Some(active_next)));
        assert_eq!(active.3.get("runner_next_check_at").and_then(Value::as_i64), Some(9999999999));
        assert_eq!(active.3.get("runner_pr_claimed_at").and_then(Value::as_i64), Some(9999999998));
        assert_eq!(active.3.get("recheck_pr_url").and_then(Value::as_str), Some("https://github.com/o/r/pull/9"));
        assert!(store.claim_due(10).await?.iter().all(|d| d.task_id != task));
        assert!(store.check_now(task).await?);
        assert!(store.retry_check_now(task).await?, "explicit checks also keep an active watch due");
        let due = store.claim_due(10).await?;
        let mine = due.iter().find(|d| d.task_id == task).expect("due after check_now");
        assert_eq!(mine.pr_url.as_deref(), Some("https://github.com/o/r/pull/9"));
        assert!(mine.data.get("seen_at").is_some());
        assert!(store.claim_due(10).await?.iter().all(|d| d.task_id != task));
        let run = store.reopen_for_feedback(task, mine.id, "fix it").await?.expect("reopened");
        let (status, kind): (String, String) = sqlx::query_as(
            "SELECT s.status, r.kind FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id WHERE r.id = $1",
        )
        .bind(run)
        .fetch_one(&pool)
        .await?;
        assert_eq!((status.as_str(), kind.as_str()), ("active", "followup"));
        assert!(store.reopen_for_feedback(task, mine.id, "again").await?.is_none());
        let feedback_scheduled: bool = sqlx::query_scalar("SELECT next_check_at IS NOT NULL FROM task_milestones WHERE id = $1")
            .bind(mine.id)
            .fetch_one(&pool)
            .await?;
        assert!(feedback_scheduled, "review feedback keeps the merge watch scheduled");
        sqlx::query("UPDATE session_runs SET status = 'failed' WHERE id = $1").bind(run).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1").bind(task).execute(&pool).await?;
        sqlx::query("UPDATE task_milestones SET next_check_at = NOW() - INTERVAL '1 minute' WHERE id = $1")
            .bind(mine.id)
            .execute(&pool)
            .await?;
        let feedback_due = store.claim_due(10).await?;
        let mine = feedback_due.iter().find(|d| d.task_id == task).expect("due after failed feedback follow-up");
        let ci_run = store
            .reopen_for_ci(task, mine.id, "abc123", &serde_json::json!(["tests"]), "fix CI")
            .await?
            .expect("CI reopened");
        let ci_scheduled: bool = sqlx::query_scalar("SELECT next_check_at IS NOT NULL FROM task_milestones WHERE id = $1")
            .bind(mine.id)
            .fetch_one(&pool)
            .await?;
        assert!(ci_scheduled, "CI feedback keeps the merge watch scheduled");
        sqlx::query("UPDATE session_runs SET status = 'timed_out' WHERE id = $1").bind(ci_run).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'failed' WHERE id = $1").bind(task).execute(&pool).await?;
        sqlx::query("UPDATE task_milestones SET next_check_at = NULL WHERE id = $1")
            .bind(mine.id)
            .execute(&pool)
            .await?;
        assert!(store.claim_due(10).await?.iter().any(|d| d.task_id == task), "a legacy active watch without a timestamp remains due");
        assert!(store.arm_merge(task).await?);
        sqlx::query(
            "UPDATE task_milestones
             SET status = 'failed', detail = 'Closed without merging', data = data || jsonb_build_object('closed_pr_url', $2::text, 'runner_next_check_at', 9999999999),
                 done_at = NOW(), next_check_at = NULL
             WHERE id = $1",
        )
        .bind(mine.id)
        .bind("https://github.com/o/r/pull/9")
        .execute(&pool)
        .await?;
        assert!(!store.check_now(task).await?, "automatic nudges do not retry a closed pull request");
        assert!(store.retry_check_now(task).await?);
        let retried: (String, Option<String>, Value, bool, String, Option<String>) = sqlx::query_as(
            "SELECT m.status, m.detail, m.data, m.done_at IS NULL, t.status, t.pr_url
             FROM task_milestones m JOIN agent_tasks t ON t.id = m.task_id WHERE m.id = $1",
        )
        .bind(mine.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!((retried.0.as_str(), retried.1.as_deref(), retried.3, retried.4.as_str(), retried.5.as_deref()), ("active", Some("Waiting for review"), true, "failed", Some("https://github.com/o/r/pull/9")));
        assert_eq!(retried.2.get("closed_pr_url").and_then(Value::as_str), Some("https://github.com/o/r/pull/9"));
        assert!(retried.2.get("runner_next_check_at").is_none());
        assert_eq!(retried.2.get("recheck_pr_url").and_then(Value::as_str), Some("https://github.com/o/r/pull/9"));
        let recheck_after = retried.2.get("recheck_after").and_then(Value::as_f64).expect("retry time");
        assert!(store.check_now(task).await?);
        let preserved_recheck_after: f64 = sqlx::query_scalar("SELECT (data->>'recheck_after')::double precision FROM task_milestones WHERE id = $1")
            .bind(mine.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(preserved_recheck_after, recheck_after);
        assert!(store.claim_due(10).await?.iter().any(|due| due.task_id == task), "a local worker can claim an explicit retry");

        sqlx::query("UPDATE agent_tasks SET pr_url = 'https://github.com/o/r/pull/10', merged_pr_url = NULL, status = 'failed' WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query(
            "UPDATE task_milestones SET status = 'failed', detail = 'Closed without merging',
                 data = jsonb_build_object('closed_pr_url', 'https://github.com/o/r/pull/9'), done_at = NOW(), next_check_at = NULL
             WHERE id = $1",
        )
        .bind(mine.id)
        .execute(&pool)
        .await?;
        assert!(!store.retry_check_now(task).await?, "a close for the old pull request cannot retry the new one");

        sqlx::query("UPDATE task_milestones SET data = '{}' WHERE id = $1").bind(mine.id).execute(&pool).await?;
        assert!(store.retry_check_now(task).await?, "a legacy failed milestone without a close event can retry its recorded pull request");
        sqlx::query(
            "UPDATE task_milestones SET status = 'failed', detail = 'Closed without merging', data = '{}', done_at = NOW(), next_check_at = NULL
             WHERE id = $1",
        )
        .bind(mine.id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO world_events (type, task_id, payload) VALUES ('task.pr_closed', $1, jsonb_build_object('pr_url', 'https://github.com/o/r/pull/9'))",
        )
        .bind(task)
        .execute(&pool)
        .await?;
        assert!(!store.retry_check_now(task).await?, "a legacy close event for the old pull request cannot retry the new one");
        sqlx::query(
            "INSERT INTO world_events (type, task_id, payload) VALUES ('task.pr_closed', $1, jsonb_build_object('pr_url', 'https://github.com/o/r/pull/10'))",
        )
        .bind(task)
        .execute(&pool)
        .await?;
        assert!(store.retry_check_now(task).await?, "a legacy failed milestone follows its latest close event");

        for terminal in ["done", "cancelled"] {
            sqlx::query("UPDATE agent_tasks SET status = $2 WHERE id = $1").bind(task).bind(terminal).execute(&pool).await?;
            sqlx::query(
                "UPDATE task_milestones SET status = 'failed', detail = 'Closed without merging',
                     data = jsonb_build_object('closed_pr_url', 'https://github.com/o/r/pull/10'), done_at = NOW(), next_check_at = NULL
                 WHERE id = $1",
            )
            .bind(mine.id)
            .execute(&pool)
            .await?;
            assert!(!store.retry_check_now(task).await?, "{terminal} quests stay terminal");
        }
        sqlx::query("UPDATE agent_tasks SET status = 'failed', merged_pr_url = pr_url WHERE id = $1")
            .bind(task)
            .execute(&pool)
            .await?;
        assert!(!store.retry_check_now(task).await?, "a confirmed merge is never retried as a close");
        sqlx::query("UPDATE agent_tasks SET merged_pr_url = NULL WHERE id = $1").bind(task).execute(&pool).await?;
        assert!(store.retry_check_now(task).await?);
        assert_eq!(store.open_count(task).await?, 1);
        assert!(!store.merge_done(task).await?);
        store.note(mine.id, "done", "Merged", serde_json::json!({})).await?;
        assert_eq!(store.open_count(task).await?, 0);
        assert!(store.merge_done(task).await?);
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_sessions WHERE id = $1").bind(session).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_new_pr_keeps_the_old_one_in_history_and_watches_the_new_one() -> Result<()> {
        let Ok(url) = std::env::var("MILESTONES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = crate::db::Database::connect(&url).await?;
        let pool = db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("history-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'history', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("history-{tag}"))
            .bind(user)
            .fetch_one(&pool)
            .await?;
        let store = MilestoneStore::new(pool.clone());
        let first = "https://github.com/o/r/pull/9";
        let second = "https://github.com/o/r/pull/12";
        for (merge_status, state) in [("done", Some("merged")), ("failed", Some("closed")), ("active", None)] {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, repo, pr_url, pr_number, assigned_agent_id)
                 VALUES ($1, 'history test', 'done', 'o/r', $2, 9, $3) RETURNING id",
            )
            .bind(user)
            .bind(first)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
            store.add(task, &[NewMilestone { title: merge_title().into(), kind: "merge".into() }]).await?;
            sqlx::query("UPDATE task_milestones SET status = $2, done_at = NOW(), data = '{\"seen_at\": \"2026-01-01T00:00:00Z\"}' WHERE task_id = $1 AND kind = 'merge'")
                .bind(task)
                .bind(merge_status)
                .execute(&pool)
                .await?;
            db.set_pr(task, second, 12).await?;
            let unchanged: String = sqlx::query_scalar("SELECT pr_url FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!(unchanged, first, "{merge_status}: ordinary publication cannot replace the tracked PR");
            assert!(db.replace_current_pr(task, first, 9, second, 12).await?);
            assert!(!db.replace_current_pr(task, first, 9, second, 12).await?);
            let (pr_url, status, past): (String, String, serde_json::Value) =
                sqlx::query_as("SELECT pr_url, status, past_prs FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!((pr_url.as_str(), status.as_str()), (second, "review"), "{merge_status}");
            let past = past.as_array().cloned().unwrap_or_default();
            assert_eq!(past.len(), 1, "{merge_status}: the old PR is kept once");
            assert_eq!(past[0]["url"], first);
            assert_eq!(past[0]["number"], 9);
            assert_eq!(past[0]["state"].as_str(), state, "{merge_status}");
            assert!(!store.merge_done(task).await?, "{merge_status}: the new PR is not merged yet");
            assert!(store.arm_merge(task).await?, "{merge_status}: the merge watch follows the new PR");
            let watched: Option<serde_json::Value> = sqlx::query_scalar("SELECT match FROM agent_subscriptions WHERE task_id = $1").bind(task).fetch_optional(&pool).await?;
            assert_eq!(watched.and_then(|m| m["number"].as_i64()), Some(12), "{merge_status}");
            if merge_status == "done" {
                let seen: Option<serde_json::Value> = sqlx::query_scalar("SELECT data->'seen_at' FROM task_milestones WHERE task_id = $1 AND kind = 'merge'").bind(task).fetch_one(&pool).await?;
                assert_ne!(seen, Some(serde_json::json!("2026-01-01T00:00:00Z")), "feedback on the new PR is read from its own start");
            }
            sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&pool).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_late_old_pr_observation_cannot_settle_the_replacement() -> Result<()> {
        let Ok(url) = std::env::var("MILESTONES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = crate::db::Database::connect(&url).await?;
        let pool = db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("stale-pr-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let old = "https://github.com/o/r/pull/9";
        let new = "https://github.com/o/r/pull/12";
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_url, pr_number)
             VALUES ($1, 'stale PR test', 'review', 'o/r', $2, 9) RETURNING id",
        )
        .bind(user)
        .bind(old)
        .fetch_one(&pool)
        .await?;
        let store = MilestoneStore::new(pool.clone());
        store.add(task, &[NewMilestone { title: merge_title().into(), kind: "merge".into() }]).await?;
        assert!(store.arm_merge(task).await?);
        assert!(db.replace_current_pr(task, old, 9, new, 12).await?);
        assert!(store.arm_merge(task).await?);

        assert!(!db.settle_current_pr(task, old, true).await?);
        assert!(!db.settle_current_pr(task, old, false).await?);
        let (status, error, merge): (String, Option<String>, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert_eq!((status.as_str(), error, merge.as_str()), ("review", None, "active"));
        assert!(db.settle_current_pr(task, new, true).await?);
        let (status, merge): (String, String) = sqlx::query_as(
            "SELECT t.status, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await?;
        assert_eq!((status.as_str(), merge.as_str()), ("done", "done"));
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&pool).await?;
        Ok(())
    }
}
