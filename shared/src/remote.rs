use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::world::{AgentMessage, WorldAgent, WorldStore};

pub fn spend_refusal(agent: &WorldAgent, spent_today: f64) -> Option<String> {
    if let Some(reason) = agent.paused_reason.as_deref() {
        return Some(format!("{} is paused ({reason}). Its owner has to resume it in Loka first.", agent.name));
    }
    match agent.daily_budget_usd {
        Some(budget) if spent_today >= budget => Some(format!(
            "{} has used its daily budget of ${budget:.2}. It resumes tomorrow (UTC) or when its owner raises the budget.",
            agent.name
        )),
        _ => None,
    }
}

impl WorldStore {
    pub async fn find_message(&self, id: Uuid) -> Result<Option<AgentMessage>> {
        Ok(sqlx::query_as::<_, AgentMessage>(
            "SELECT m.*, s.role AS session_role FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id WHERE m.id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn open_asks(&self, agent_id: Uuid, limit: i64) -> Result<Vec<AgentMessage>> {
        Ok(sqlx::query_as::<_, AgentMessage>(
            "SELECT m.*, NULL::text AS session_role FROM agent_messages m
             WHERE m.agent_id = $1 AND m.author_kind = 'agent' AND m.meta->>'kind' = 'ask'
               AND NOT m.meta ? 'thread_user_id'
               AND m.created_at > NOW() - INTERVAL '14 days'
               AND NOT EXISTS (
                 SELECT 1 FROM agent_messages a
                 WHERE a.agent_id = m.agent_id AND a.meta->>'answers' = m.id::text
               )
             ORDER BY m.created_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn agent_replies_after(&self, agent_id: Uuid, viewer: Uuid, after: DateTime<Utc>, limit: i64) -> Result<Vec<AgentMessage>> {
        Ok(sqlx::query_as::<_, AgentMessage>(&format!(
            "SELECT m.*, s.role AS session_role FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id
             WHERE m.agent_id = $1 AND m.author_kind = 'agent' AND m.created_at > $2 AND {}
             ORDER BY m.created_at ASC LIMIT $3",
            crate::guests::thread_filter("$1", "$4")
        ))
        .bind(agent_id)
        .bind(after)
        .bind(limit)
        .bind(viewer)
        .fetch_all(self.pool())
        .await?)
    }
}
