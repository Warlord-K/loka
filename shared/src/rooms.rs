use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::world::{AgentMessage, SessionBrief, SessionRunView, WorldEvent, WorldStore};

pub const ROOM_EVENTS: i64 = 300;
pub const ROOM_MESSAGES: i64 = 200;
pub const ROOM_RUNS_PER_SESSION: i64 = 60;

#[derive(Debug, Clone, Serialize)]
pub struct QuestRoom {
    pub sessions: Vec<SessionBrief>,
    pub runs: Vec<SessionRunView>,
    pub events: Vec<WorldEvent>,
    pub messages: Vec<AgentMessage>,
    pub cursor: i64,
}

pub fn quest_pattern(task_id: Uuid) -> String {
    format!("%{task_id}%")
}

impl WorldStore {
    pub async fn quest_sessions(&self, task_id: Uuid) -> Result<Vec<SessionBrief>> {
        Ok(sqlx::query_as::<_, SessionBrief>(
            "SELECT s.id, s.agent_id, s.role, s.backend, s.model, s.status, s.task_id, t.title AS task_title,
                    COALESCE(s.title, t.title) AS title, s.summary,
                    COALESCE((SELECT SUM(cost_usd) FROM session_runs r WHERE r.session_id = s.id), 0)::float8 AS cost_usd,
                    COALESCE((SELECT SUM(input_tokens + cached_input_tokens + output_tokens) FROM session_runs r WHERE r.session_id = s.id), 0)::int8 AS tokens,
                    (SELECT COUNT(*) FROM session_runs r WHERE r.session_id = s.id) AS runs,
                    EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')) AS running,
                    s.created_at, s.updated_at, s.working_dir, t.target_branch AS base_branch,
                    s.plan_state, s.attempt, s.attempt_state
             FROM agent_sessions s JOIN agent_tasks t ON t.id = s.task_id
             WHERE s.task_id = $1
             ORDER BY s.created_at",
        )
        .bind(task_id)
        .fetch_all(self.pool())
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn quest_room(
        &self,
        task_id: Uuid,
        since: DateTime<Utc>,
        agents: &[Uuid],
        viewer: Uuid,
        after_seq: Option<i64>,
        after_message: Option<DateTime<Utc>>,
    ) -> Result<QuestRoom> {
        let sessions: Vec<SessionBrief> = self
            .quest_sessions(task_id)
            .await?
            .into_iter()
            .filter(|s| agents.contains(&s.agent_id))
            .collect();
        let session_ids: Vec<Uuid> = sessions.iter().map(|s| s.id).collect();
        let mut runs = Vec::new();
        for session in &sessions {
            runs.extend(self.list_runs_page(session.id, None, ROOM_RUNS_PER_SESSION, true).await?);
        }
        runs.sort_by_key(|r| r.run.created_at);
        let cursor = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(seq) FROM world_events")
            .fetch_one(self.pool())
            .await?
            .unwrap_or(0);
        let mut events = sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM (
               SELECT * FROM world_events WHERE task_id = $1 AND seq > $3 AND seq <= $5
               UNION
               SELECT * FROM world_events WHERE session_id = ANY($2) AND seq > $3 AND seq <= $5
             ) e
             WHERE e.type NOT IN ('agent.progress', 'agent.status', 'message.created', 'run.activity', 'task.changed')
             ORDER BY seq DESC LIMIT $4",
        )
        .bind(task_id)
        .bind(&session_ids)
        .bind(after_seq.unwrap_or(0))
        .bind(ROOM_EVENTS)
        .bind(cursor)
        .fetch_all(self.pool())
        .await?;
        events.reverse();
        let mut messages = sqlx::query_as::<_, AgentMessage>(&format!(
            "SELECT m.*, s.role AS session_role FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id
             WHERE m.agent_id = ANY($2)
               AND m.created_at >= $3
               AND ($4::timestamptz IS NULL OR m.created_at > $4 OR m.edited_at > $4)
               AND (m.meta->>'task_id' = $1::text
                 OR m.run_id IN (
                   SELECT r.id FROM session_runs r
                   WHERE r.agent_id = ANY($2) AND r.created_at >= $3
                     AND (r.task_id = $1 OR (r.kind = 'turn' AND r.prompt LIKE $5))
                 ))
               AND (m.meta->>'kind' = 'quest' OR m.meta->>'task_id' = $1::text OR {})
             ORDER BY m.created_at DESC LIMIT $6",
            crate::guests::thread_filter("m.agent_id", "$7")
        ))
        .bind(task_id)
        .bind(agents)
        .bind(since)
        .bind(after_message)
        .bind(quest_pattern(task_id))
        .bind(ROOM_MESSAGES)
        .bind(viewer)
        .fetch_all(self.pool())
        .await?;
        messages.reverse();
        Ok(QuestRoom { sessions, runs, events, messages, cursor })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quest_room_gathers_sessions_events_and_messages() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("ROOMS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ($1, $2) RETURNING id")
            .bind(format!("room {tag}"))
            .bind(format!("{tag}@room.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility)
             VALUES ($1, $1, '🦊', 'claude', 'sonnet', $2, 'private') RETURNING id",
        )
        .bind(format!("room-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, assigned_agent_id) VALUES ($1, 'Fix the login', $2) RETURNING id")
            .bind(owner)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title) VALUES ($1, 'Something else') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'sonnet') RETURNING id")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let work: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'claude', 'sonnet', $2) RETURNING id")
            .bind(agent)
            .bind(task)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'task', 'running', 'Task: Fix the login')")
            .bind(work)
            .bind(agent)
            .bind(task)
            .execute(&pool)
            .await?;
        let turn: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'completed', $3) RETURNING id")
            .bind(main)
            .bind(agent)
            .bind(format!("[event] message from you:\nhow is it going\n\nquest:{task}"))
            .fetch_one(&pool)
            .await?;
        let unrelated: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'completed', 'hello') RETURNING id")
            .bind(main)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta) VALUES ($1, 'user', $2, 'how is it going', $3)")
            .bind(agent)
            .bind(owner)
            .bind(serde_json::json!({ "task_id": task }))
            .execute(&pool)
            .await?;
        for (run, body) in [(turn, "going well"), (unrelated, "hi there")] {
            sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, run_id) VALUES ($1, 'agent', $1, $2, $3)")
                .bind(agent)
                .bind(body)
                .bind(run)
                .execute(&pool)
                .await?;
        }
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.started', $1, $2, '{}')")
            .bind(agent)
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, session_id, payload) VALUES ('review.finished', $1, $2, '{}')")
            .bind(agent)
            .bind(work)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.started', $1, $2, '{}')")
            .bind(agent)
            .bind(other)
            .execute(&pool)
            .await?;
        let since = chrono::Utc::now() - chrono::Duration::hours(1);

        let room = store.quest_room(task, since, &[agent], owner, None, None).await?;
        assert_eq!(room.sessions.iter().map(|s| s.id).collect::<Vec<_>>(), vec![work]);
        assert_eq!(room.runs.len(), 1);
        assert!(room.runs[0].run.prompt.is_empty());
        let types: Vec<&str> = room.events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"task.started") && types.contains(&"review.finished"));
        assert!(room.events.iter().all(|e| e.task_id != Some(other)));
        let bodies: Vec<&str> = room.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, vec!["how is it going", "going well"]);

        let hidden = store.quest_room(task, since, &[], owner, None, None).await?;
        assert!(hidden.sessions.is_empty() && hidden.messages.is_empty());

        let later = store.quest_room(task, since, &[agent], owner, Some(room.cursor), room.messages.last().map(|m| m.created_at)).await?;
        assert!(later.events.is_empty() && later.messages.is_empty());

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = ANY($1)").bind(vec![task, other]).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn quest_pattern_wraps_the_full_id() {
        let id = Uuid::nil();
        assert_eq!(quest_pattern(id), "%00000000-0000-0000-0000-000000000000%");
    }
}
