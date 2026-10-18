use anyhow::{bail, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{NewEvent, NewMessage, WorldAgent, WorldStore};

pub const ALLOW: &str = "Allow for this quest";
pub const DENY: &str = "Not now";

pub fn is_allow(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.starts_with("allow") || t == "yes" || t == "ok" || t == "okay"
}

pub enum AppsAnswer {
    NotAppsCard,
    Stale,
    Answered { allowed: bool },
}

impl WorldStore {
    pub async fn apps_allowed_for_run(&self, run_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM session_runs r JOIN agent_tasks t ON t.id = r.task_id
                             WHERE r.id = $1 AND t.apps_allowed_at IS NOT NULL)",
        )
        .bind(run_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn request_app_control(&self, agent: &WorldAgent, task_id: Uuid, session_id: Uuid, reason: &str) -> Result<String> {
        let Some(owner) = agent.owner_user_id else {
            bail!("only agents that work for someone can ask to use apps");
        };
        let row = sqlx::query_as::<_, (String, bool)>(
            "SELECT title, apps_allowed_at IS NOT NULL FROM agent_tasks WHERE id = $1 AND assigned_agent_id = $2",
        )
        .bind(task_id)
        .bind(agent.id)
        .fetch_optional(self.pool())
        .await?;
        let Some((title, allowed)) = row else {
            bail!("this quest is not yours");
        };
        if allowed {
            return Ok("Your owner already allowed app control for this quest. Go ahead.".into());
        }
        let pending = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_messages m
               WHERE m.agent_id = $1 AND m.meta->'apps'->>'task_id' = $2
                 AND NOT EXISTS (SELECT 1 FROM agent_messages a
                                  WHERE a.agent_id = m.agent_id AND a.author_user_id = $3 AND a.meta->>'answers' = m.id::text))",
        )
        .bind(agent.id)
        .bind(task_id.to_string())
        .bind(owner)
        .fetch_one(self.pool())
        .await?;
        if pending {
            return Ok("You already asked and your owner has not answered yet. Keep working on anything that does not need apps; their answer reaches you as a message.".into());
        }
        let reason: String = reason.trim().chars().take(400).collect();
        let text = if reason.is_empty() {
            format!("I want to use apps on your Mac for \"{title}\". Allow it for this quest?")
        } else {
            format!("I want to use apps on your Mac for \"{title}\": {reason}\n\nAllow it for this quest?")
        };
        let choices = [ALLOW, DENY];
        let main = self.main_session(agent.id).await?;
        let message = self
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: &text,
                session_id: main.as_ref().map(|m| m.id),
                run_id: None,
                meta: json!({
                    "kind": "ask",
                    "choices": choices,
                    "task_id": task_id,
                    "apps": { "task_id": task_id, "session_id": session_id },
                }),
            })
            .await?;
        self.emit(
            NewEvent::new("message.created")
                .agent(agent.id)
                .task(Some(task_id))
                .payload(json!({ "message_id": message.id, "author": agent.name, "author_kind": "agent", "body": text, "ask": true })),
            None,
        )
        .await?;
        self.emit(
            NewEvent::new("attention.requested")
                .agent(agent.id)
                .session(session_id)
                .task(Some(task_id))
                .payload(json!({ "text": text, "agent": agent.name, "message_id": message.id, "choices": choices })),
            None,
        )
        .await?;
        Ok("Asked your owner. Keep working on anything that does not need apps; their answer reaches you as a message in this session.".into())
    }

    pub async fn is_apps_card(&self, agent_id: Uuid, card: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_messages WHERE id = $1 AND agent_id = $2 AND meta ? 'apps')")
            .bind(card)
            .bind(agent_id)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn answer_app_control(&self, agent_id: Uuid, card: Uuid, text: &str, by: &str) -> Result<AppsAnswer> {
        let apps = sqlx::query_scalar::<_, Option<Value>>("SELECT meta->'apps' FROM agent_messages WHERE id = $1 AND agent_id = $2")
            .bind(card)
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        let Some(apps) = apps else {
            return Ok(AppsAnswer::NotAppsCard);
        };
        let id = |k: &str| apps.get(k).and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok());
        let (Some(task_id), Some(session_id)) = (id("task_id"), id("session_id")) else {
            return Ok(AppsAnswer::Stale);
        };
        let allowed = is_allow(text);
        if allowed {
            sqlx::query("UPDATE agent_tasks SET apps_allowed_at = COALESCE(apps_allowed_at, NOW()) WHERE id = $1 AND assigned_agent_id = $2")
                .bind(task_id)
                .bind(agent_id)
                .execute(self.pool())
                .await?;
        }
        let Some(session) = self.get_session(session_id).await?.filter(|s| s.agent_id == agent_id && s.status != "archived") else {
            return Ok(AppsAnswer::Stale);
        };
        let note = if allowed {
            format!("[{by}, your owner, verified] allowed app control for this quest. Go ahead with the apps launcher.")
        } else {
            format!("[{by}, your owner, verified] said not now to app control. Do not use apps in this quest: finish another way, or say in your report what you would need.")
        };
        if self.start_followup(&session, &note, by, false).await.is_err() {
            return Ok(AppsAnswer::Stale);
        }
        Ok(AppsAnswer::Answered { allowed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_answers() {
        assert!(is_allow(ALLOW));
        assert!(is_allow("yes"));
        assert!(!is_allow(DENY));
        assert!(!is_allow("no thanks"));
    }
}
