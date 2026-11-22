use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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
        let titles: Vec<String> = titles.iter().map(|title| title.trim().to_lowercase()).filter(|title| !title.is_empty()).collect();
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
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn arm_merge(&self, task_id: Uuid, has_pr: bool) -> Result<bool> {
        let rows = if has_pr {
            sqlx::query(
                "UPDATE task_milestones
                 SET status = 'active', detail = 'Waiting for review',
                     next_check_at = NOW() + make_interval(secs => $2),
                     data = data || jsonb_build_object('seen_at', COALESCE(data->'seen_at', to_jsonb(NOW()))),
                     updated_at = NOW()
                 WHERE task_id = $1 AND kind = 'merge' AND status IN ('pending', 'active')",
            )
            .bind(task_id)
            .bind(check_every_secs() as f64)
            .execute(&self.pool)
            .await?
            .rows_affected()
        } else {
            sqlx::query(
                "UPDATE task_milestones
                 SET status = 'skipped', detail = 'No pull request was opened', next_check_at = NULL, updated_at = NOW()
                 WHERE task_id = $1 AND kind = 'merge' AND status IN ('pending', 'active')",
            )
            .bind(task_id)
            .execute(&self.pool)
            .await?;
            0
        };
        Ok(rows > 0)
    }

    pub async fn settle_merge(&self, task_id: Uuid, merged: bool) -> Result<()> {
        let (status, detail) = if merged {
            ("done", "Merged")
        } else {
            ("failed", "Closed without merging")
        };
        sqlx::query(
            "UPDATE task_milestones
             SET status = $2, detail = $3, done_at = NOW(), next_check_at = NULL, updated_at = NOW()
             WHERE task_id = $1 AND kind = 'merge' AND status IN ('pending', 'active')",
        )
        .bind(task_id)
        .bind(status)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_due(&self, limit: i64) -> Result<Vec<DueMerge>> {
        Ok(sqlx::query_as::<_, DueMerge>(
            "WITH due AS (
               SELECT m.id FROM task_milestones m
               WHERE m.kind = 'merge' AND m.status = 'active' AND m.next_check_at <= NOW()
               ORDER BY m.next_check_at
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
        Ok(sqlx::query(
            "UPDATE task_milestones SET next_check_at = NOW(), updated_at = NOW()
             WHERE task_id = $1 AND kind = 'merge' AND status = 'active'",
        )
        .bind(task_id)
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
             WHERE id = $1",
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
        let session = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT id, agent_id FROM agent_sessions
             WHERE task_id = $1 AND role = 'task' AND status <> 'archived'
             ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
        )
        .bind(task_id)
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
            "UPDATE task_milestones SET detail = $2, next_check_at = NULL, updated_at = NOW()
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
        assert!(store.arm_merge(task, true).await?);
        assert!(store.claim_due(10).await?.iter().all(|d| d.task_id != task));
        store.check_now(task).await?;
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
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(run).execute(&pool).await?;
        assert!(store.arm_merge(task, true).await?);
        assert_eq!(store.open_count(task).await?, 1);
        assert!(!store.merge_done(task).await?);
        store.note(mine.id, "done", "Merged", serde_json::json!({})).await?;
        assert_eq!(store.open_count(task).await?, 0);
        assert!(store.merge_done(task).await?);
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_sessions WHERE id = $1").bind(session).execute(&pool).await?;
        Ok(())
    }
}
