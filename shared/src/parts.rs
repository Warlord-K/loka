use anyhow::{bail, Result};
use serde::Serialize;
use uuid::Uuid;

use crate::world::WorldStore;

pub const MAX_PARTS: i64 = 12;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PartBrief {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub assigned_agent_id: Option<Uuid>,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PartParent {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub assigned_agent_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub parent_task_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
}

impl WorldStore {
    pub async fn part_parent(&self, owner: Uuid, org: Option<Uuid>, key: &str) -> Result<PartParent> {
        let key = key.trim().trim_start_matches("quest:");
        let found = sqlx::query_as::<_, PartParent>(
            "SELECT id, title, status, assigned_agent_id, team_id, parent_task_id, org_id FROM agent_tasks
             WHERE (id::text = $1 OR (length($1) >= 6 AND id::text LIKE $1 || '%')) AND user_id = $2
               AND ($3::uuid IS NULL OR org_id IS NOT DISTINCT FROM $3)
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(key)
        .bind(owner)
        .bind(org)
        .fetch_optional(self.pool())
        .await?;
        let Some(parent) = found else {
            bail!("no quest {key} to make this a part of");
        };
        if parent.team_id.is_some() {
            bail!("quest {key} belongs to a team; pass team too");
        }
        if parent.parent_task_id.is_some() {
            bail!("quest {key} is itself a part; make this a part of its parent quest instead");
        }
        if matches!(parent.status.as_str(), "done" | "cancelled" | "failed") {
            bail!("quest {key} is already {}", parent.status);
        }
        let (working, parts): (bool, i64) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
                             WHERE s.task_id = $1 AND r.status IN ('queued', 'running')),
                    (SELECT COUNT(*) FROM agent_tasks WHERE parent_task_id = $1)",
        )
        .bind(parent.id)
        .fetch_one(self.pool())
        .await?;
        if working {
            bail!("quest {key} is being worked on by its own session right now; split a quest into parts instead of starting it, not while it runs");
        }
        if parts >= MAX_PARTS {
            bail!("quest {key} already has {MAX_PARTS} parts");
        }
        Ok(parent)
    }

    pub async fn assign_part(&self, task_id: Uuid, agent_id: Uuid, parent: &PartParent) -> Result<()> {
        if parent.id == task_id {
            bail!("a quest cannot be a part of itself");
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress', updated_at = NOW() WHERE id = $1 AND status = 'todo'")
            .bind(parent.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE agent_tasks SET parent_task_id = $2, assigned_agent_id = $3, assignee_user_id = NULL,
                    org_id = COALESCE($4, org_id), updated_at = NOW()
             WHERE id = $1",
        )
        .bind(task_id)
        .bind(parent.id)
        .bind(agent_id)
        .bind(parent.org_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn quest_parts(&self, task_id: Uuid) -> Result<Vec<PartBrief>> {
        Ok(sqlx::query_as::<_, PartBrief>(
            "SELECT id, title, status, assigned_agent_id, pr_url FROM agent_tasks WHERE parent_task_id = $1 ORDER BY created_at, id",
        )
        .bind(task_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn quest_parent(&self, task_id: Uuid) -> Result<Option<PartBrief>> {
        Ok(sqlx::query_as::<_, PartBrief>(
            "SELECT p.id, p.title, p.status, p.assigned_agent_id, p.pr_url
             FROM agent_tasks k JOIN agent_tasks p ON p.id = k.parent_task_id WHERE k.id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::NewEvent;
    use serde_json::{json, Value};
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn quests_split_into_parts_close_together() -> Result<()> {
        let Ok(url) = std::env::var("PARTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{tag}@parts.test"))
            .fetch_one(&pool)
            .await?;
        let agent = |name: &'static str| {
            let pool = pool.clone();
            let slug = format!("{}-{}", name.to_lowercase(), &tag[..8]);
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'opus', $3) RETURNING id",
                )
                .bind(slug)
                .bind(name)
                .bind(owner)
                .fetch_one(&pool)
                .await
            }
        };
        let lead = agent("Ada").await?;
        let helper = agent("Rook").await?;
        let quest = |title: &'static str, assigned: Option<Uuid>| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, $2, 'todo', $3) RETURNING id")
                    .bind(owner)
                    .bind(title)
                    .bind(assigned)
                    .fetch_one(&pool)
                    .await
            }
        };
        let status = |id: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1").bind(id).fetch_one(&pool).await }
        };
        let set = |id: Uuid, to: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query("UPDATE agent_tasks SET status = $2 WHERE id = $1").bind(id).bind(to).execute(&pool).await.map(|_| ()) }
        };
        let inbox = |who: Uuid, kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = $2 ORDER BY id")
                    .bind(who)
                    .bind(kind)
                    .fetch_all(&pool)
                    .await
            }
        };

        let parent_id = quest("Ship the importer", Some(lead)).await?;
        let parent = store.part_parent(owner, None, &parent_id.to_string()[..8]).await?;
        assert_eq!(parent.id, parent_id);
        let one = quest("Parser", None).await?;
        let two = quest("Docs", None).await?;
        store.assign_part(one, helper, &parent).await?;
        store.assign_part(two, lead, &parent).await?;
        assert_eq!(status(parent_id).await?, "in_progress");
        assert!(store.quest_has_parts(parent_id).await?);
        assert_eq!(store.quest_parts(parent_id).await?.len(), 2);
        assert_eq!(store.quest_parent(one).await?.map(|p| p.id), Some(parent_id));
        let assigned = inbox(helper, "task.assigned").await?;
        assert_eq!(assigned.last().unwrap()["part_of"], json!(parent_id));
        assert_eq!(assigned.last().unwrap()["part_from"], json!("Ada"));
        assert!(store.part_parent(owner, None, &one.to_string()).await.unwrap_err().to_string().contains("itself a part"));
        assert!(store.assign_part(parent_id, lead, &parent).await.is_err());

        store
            .emit(NewEvent::new("task.finished").agent(helper).task(Some(one)).payload(json!({ "task_id": one, "status": "done", "report": { "summary": "Parser works" } })), None)
            .await?;
        let heard = inbox(lead, "quest.parts").await?;
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0]["member"], json!("Rook"));
        assert_eq!(heard[0]["part_of"], json!(parent_id));
        store
            .emit(NewEvent::new("task.finished").agent(lead).task(Some(two)).payload(json!({ "task_id": two, "status": "done" })), None)
            .await?;
        assert_eq!(inbox(lead, "quest.parts").await?.len(), 1);

        set(one, "done").await?;
        assert_eq!(status(parent_id).await?, "in_progress");
        set(two, "done").await?;
        assert_eq!(status(parent_id).await?, "done");
        let closed = inbox(lead, "quest.parts").await?;
        assert_eq!(closed.iter().filter(|p| p["closed"] == json!(true) && p["task_id"] == json!(parent_id) && p["parts"] == json!(2)).count(), 1);
        let finished: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.finished' AND payload->>'auto_closed' = 'true'")
            .bind(parent_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(finished, 1);
        assert!(store.part_parent(owner, None, &parent_id.to_string()).await.unwrap_err().to_string().contains("already done"));

        let stuck_id = quest("Stuck", Some(lead)).await?;
        let stuck = store.part_parent(owner, None, &stuck_id.to_string()).await?;
        let bad = quest("Fails", None).await?;
        store.assign_part(bad, helper, &stuck).await?;
        set(bad, "failed").await?;
        assert_eq!(status(stuck_id).await?, "in_progress");
        store
            .emit(NewEvent::new("task.finished").agent(helper).task(Some(bad)).payload(json!({ "task_id": bad, "status": "failed", "error": "tests fail" })), None)
            .await?;
        let failed = inbox(lead, "quest.parts").await?;
        assert_eq!(failed.last().unwrap()["error"], json!("tests fail"));

        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{tag}-other@parts.test"))
            .fetch_one(&pool)
            .await?;
        assert!(store.part_parent(stranger, None, &stuck_id.to_string()).await.is_err());
        Ok(())
    }
}
