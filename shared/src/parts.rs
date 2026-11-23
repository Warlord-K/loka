use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    rooms::room_reach,
    world::{emit_in, NewEvent, ReviewerChoice, WorldStore},
};

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
        if let Some(room) = self.room_asking(parent.id).await? {
            let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_tasks WHERE parent_task_id = $1 AND status NOT IN ('done', 'cancelled', 'failed')")
                .bind(parent.id)
                .fetch_one(self.pool())
                .await?;
            if let (crate::quest_questions::Asking::Crew, Some(size)) = (room.mode(), room.crew_size) {
                if open >= size as i64 {
                    bail!("quest {key} runs as a crew of {size}, and {open} parts are still open; finish one first or raise crew_size with update_task");
                }
            }
        }
        Ok(parent)
    }

    pub async fn assign_part(&self, task_id: Uuid, agent_id: Uuid, parent: &PartParent) -> Result<()> {
        if parent.id == task_id {
            bail!("a quest cannot be a part of itself");
        }
        let agent = self.get_agent(agent_id).await?.ok_or_else(|| anyhow::anyhow!("agent {agent_id} not found"))?;
        let base = crate::variety::Pick::new(&agent.cli, &agent.model);
        let rule = self.rule_model_pin(agent.owner_user_id, agent.org_id).await?;
        let available = self.signed_in_clis(agent.owner_user_id, agent.runner_id, &base).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM agent_tasks WHERE id = $1 FOR UPDATE")
            .bind(parent.id)
            .fetch_one(&mut *tx)
            .await?;
        let (crew, pick) = self.pick_crew_helper(&mut tx, task_id, parent.id, rule, &base, &available).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress', updated_at = NOW() WHERE id = $1 AND status = 'todo'")
            .bind(parent.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE agent_tasks SET parent_task_id = $2, assigned_agent_id = $3, assignee_user_id = NULL,
                    org_id = COALESCE($4, org_id), attempts = CASE WHEN $5::jsonb IS NULL THEN attempts ELSE 1 END,
                    attempt_variants = COALESCE($5, attempt_variants), updated_at = NOW()
             WHERE id = $1",
        )
        .bind(task_id)
        .bind(parent.id)
        .bind(agent_id)
        .bind(parent.org_id)
        .bind(pick.as_ref().map(|choice| json!([choice.to_value()])))
        .execute(&mut *tx)
        .await?;
        if crew {
            let reach = room_reach(&mut tx, parent.id).await?;
            emit_in(
                &mut tx,
                &reach.scope(
                    NewEvent::new("room.part_added")
                        .agent(agent_id)
                        .task(Some(parent.id))
                        .payload(json!({ "task_id": parent.id, "part_task_id": task_id, "agent_id": agent_id, "quiet": true })),
                ),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn pick_crew_helper(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        task_id: Uuid,
        parent_id: Uuid,
        rule: Option<ReviewerChoice>,
        base: &crate::variety::Pick,
        available: &[crate::starter_team::CliModels],
    ) -> Result<(bool, Option<ReviewerChoice>)> {
        let row = sqlx::query_as::<_, (bool, i16, i64, i64, Option<serde_json::Value>, serde_json::Value, serde_json::Value)>(
            "SELECT p.team_id IS NULL AND COALESCE(p.asking, pa.asking, 'off') = 'crew',
                    COALESCE(p.crew_size, $3),
                    (SELECT COUNT(*) FROM agent_tasks k WHERE k.parent_task_id = $2 AND k.status NOT IN ('done', 'cancelled', 'failed') AND k.id <> $1),
                    (SELECT COUNT(*) FROM agent_tasks k WHERE k.parent_task_id = $2 AND k.created_at < t.created_at),
                    p.crew_variants, COALESCE(pa.crew_variants, '[]'::jsonb), t.attempt_variants
             FROM agent_tasks t
             JOIN agent_tasks p ON p.id = $2
             LEFT JOIN agent_profiles pa ON pa.id = p.assigned_agent_id
             WHERE t.id = $1",
        )
        .bind(task_id)
        .bind(parent_id)
        .bind(crate::quest_questions::DEFAULT_CREW)
        .fetch_optional(&mut **tx)
        .await?;
        let Some((crew, crew_size, open, slot, quest, agent, current)) = row else {
            return Ok((false, None));
        };
        if !crew {
            return Ok((false, None));
        }
        if open >= i64::from(crew_size) {
            bail!("quest {parent_id} runs as a crew of {crew_size}, and {open} parts are still open; finish one first or raise crew_size with update_task");
        }
        if current.as_array().is_some_and(|entries| !entries.is_empty()) {
            return Ok((true, None));
        }
        let slot = slot.max(0) as usize;
        let pins = crate::variety::Pins {
            quest: crate::variety::stated(quest.as_ref(), slot),
            agent: crate::variety::stated(Some(&agent), slot),
            rule,
        };
        let picked = crate::variety::pick(crate::variety::Role::Helper, slot, &pins, base, available);
        Ok((true, (picked != *base).then(|| picked.choice())))
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
    async fn crew_assignment_persists_its_variant_before_waking_on_one_connection() -> Result<()> {
        let Ok(url) = std::env::var("PARTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{tag}@atomic-parts.test"))
            .fetch_one(&pool)
            .await?;
        let token: Vec<u8> = Uuid::new_v4().as_bytes().iter().chain(Uuid::new_v4().as_bytes()).copied().collect();
        let runner: Uuid = sqlx::query_scalar(
            "INSERT INTO world_runners (owner_user_id, name, platform, token_hash, status, capabilities, last_seen_at)
             VALUES ($1, 'Mac', 'macos', $2, 'online', $3, NOW()) RETURNING id",
        )
        .bind(owner)
        .bind(token)
        .bind(json!({ "backends": { "claude": true, "codex": true }, "models": { "claude": ["opus"], "codex": ["gpt-5.5"] } }))
        .fetch_one(&pool)
        .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, runner_id)
             VALUES ($1, 'Rook', 'claude', 'opus', $2, $3) RETURNING id",
        )
        .bind(format!("rook-{}", &tag[..8]))
        .bind(owner)
        .bind(runner)
        .fetch_one(&pool)
        .await?;
        let parent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, asking, crew_size, crew_variants)
             VALUES ($1, 'Ship it', 'todo', $2, 'crew', 2, $3) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .bind(json!([{ "cli": "codex", "model": "gpt-5.5" }]))
        .fetch_one(&pool)
        .await?;
        let part: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title) VALUES ($1, 'Build it') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let parent = store.part_parent(owner, None, &parent_id.to_string()).await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), store.assign_part(part, agent, &parent)).await??;
        let state: (Option<Uuid>, Value, i64) = sqlx::query_as(
            "SELECT t.assigned_agent_id, t.attempt_variants,
                    (SELECT COUNT(*) FROM agent_inbox i WHERE i.agent_id = $2 AND i.kind = 'task.assigned' AND i.payload->>'task_id' = $1::text)
             FROM agent_tasks t WHERE t.id = $1",
        )
        .bind(part)
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, (Some(agent), json!([{ "cli": "codex", "model": "gpt-5.5" }]), 1));
        Ok(())
    }

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
        sqlx::query("UPDATE agent_tasks SET asking = 'crew' WHERE id = $1").bind(parent_id).execute(&pool).await?;
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
        let added: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
            "SELECT task_id, agent_id, payload FROM world_events WHERE task_id = $1 AND type = 'room.part_added' ORDER BY seq",
        )
        .bind(parent_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].0, parent_id);
        assert_eq!(added[0].1, helper);
        assert_eq!(added[0].2["part_task_id"], json!(one));
        assert_eq!(added[0].2["quiet"], json!(true));
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
