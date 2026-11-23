use anyhow::Result;
use serde_json::Value;
use uuid::Uuid;

use crate::world::{AgentMessage, InboxItem, WorldStore};

pub const REPLY_LIMIT: i64 = 500;

pub fn reply_root(items: &[InboxItem]) -> Option<Uuid> {
    let mut roots = items.iter().filter(|i| i.kind == "message").map(|i| {
        i.payload.get("reply_to").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok())
    });
    let first = roots.next()??;
    roots.all(|r| r == Some(first)).then_some(first)
}

pub fn channel_reply_ref(text: &str) -> Option<i64> {
    let text = text.trim().trim_start_matches("msg:").trim_start_matches("ch").trim_start_matches('#');
    text.parse::<i64>().ok().filter(|id| *id > 0)
}

impl WorldStore {
    pub async fn agent_thread_root(&self, agent_id: Uuid, message_id: Uuid, viewer: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT COALESCE(m.reply_to, m.id) FROM agent_messages m WHERE m.id = $1 AND m.agent_id = $2 AND {}",
            crate::guests::thread_filter("$2", "$3")
        ))
        .bind(message_id)
        .bind(agent_id)
        .bind(viewer)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn agent_own_thread_root(&self, agent_id: Uuid, message_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>("SELECT COALESCE(reply_to, id) FROM agent_messages WHERE id = $1 AND agent_id = $2")
            .bind(message_id)
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn message_replies(&self, agent_id: Uuid, root: Uuid, viewer: Uuid, limit: i64) -> Result<Vec<AgentMessage>> {
        self.message_replies_with_reply(agent_id, root, viewer, limit, None).await
    }

    pub async fn message_replies_with_reply(&self, agent_id: Uuid, root: Uuid, viewer: Uuid, limit: i64, reply: Option<Uuid>) -> Result<Vec<AgentMessage>> {
        let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        let filter = crate::guests::thread_filter_known(owner == Some(viewer), "$3");
        let first = sqlx::query_as::<_, AgentMessage>(&format!(
            "SELECT m.*, s.role AS session_role FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id
             WHERE m.id = $2 AND m.agent_id = $1 AND m.reply_to IS NULL AND {filter}"
        ))
        .bind(agent_id)
        .bind(root)
        .bind(viewer)
        .fetch_optional(self.pool())
        .await?;
        let Some(first) = first else {
            return Ok(Vec::new());
        };
        let mut rows = sqlx::query_as::<_, AgentMessage>(&format!(
            "SELECT * FROM (
               SELECT m.*, s.role AS session_role FROM agent_messages m
               LEFT JOIN agent_sessions s ON s.id = m.session_id
               WHERE m.agent_id = $1 AND m.reply_to = $2 AND {filter}
               ORDER BY (m.id = $5) DESC NULLS LAST, m.created_at DESC LIMIT $4
             ) r ORDER BY r.created_at"
        ))
        .bind(agent_id)
        .bind(root)
        .bind(viewer)
        .bind(limit)
        .bind(reply)
        .fetch_all(self.pool())
        .await?;
        rows.insert(0, first);
        Ok(rows)
    }

    pub async fn agent_thread_transcript(&self, agent_id: Uuid, root: Uuid, limit: i64) -> Result<String> {
        let rows = sqlx::query_as::<_, (String, Option<String>, String)>(
            "SELECT kind, name, body FROM (
               SELECT m.author_kind AS kind, COALESCE(a.name, u.name, split_part(u.email, '@', 1)) AS name, m.body, m.created_at
               FROM agent_messages m
               LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
               LEFT JOIN users u ON u.id = m.author_user_id
               WHERE m.agent_id = $1 AND (m.id = $2 OR m.reply_to = $2)
               ORDER BY m.created_at DESC LIMIT $3
             ) r ORDER BY r.created_at",
        )
        .bind(agent_id)
        .bind(root)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(kind, name, body)| format!("{}: {}", name.unwrap_or(kind), crate::links::excerpt(&body, 400)))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn item(kind: &str, payload: Value) -> InboxItem {
        InboxItem { id: 1, agent_id: Uuid::nil(), kind: kind.into(), event_seq: None, payload, status: "processing".into(), attempts: 0, created_at: Utc::now() }
    }

    #[test]
    fn replies_follow_the_thread_they_answer() {
        let root = Uuid::new_v4();
        let other = Uuid::new_v4();
        let threaded = item("message", json!({ "reply_to": root.to_string() }));
        let plain = item("message", json!({ "body": "hi" }));
        assert_eq!(reply_root(std::slice::from_ref(&threaded)), Some(root));
        assert_eq!(reply_root(&[threaded.clone(), item("task.finished", json!({}))]), Some(root));
        assert_eq!(reply_root(&[threaded.clone(), plain.clone()]), None);
        assert_eq!(reply_root(&[threaded, item("message", json!({ "reply_to": other.to_string() }))]), None);
        assert_eq!(reply_root(&[plain]), None);
        assert_eq!(reply_root(&[]), None);
    }

    #[tokio::test]
    async fn chat_replies_stay_in_their_thread() -> Result<()> {
        let Ok(url) = std::env::var("CHANNELS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let mut people = Vec::new();
        for name in ["owner", "bob"] {
            people.push(
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email, name, world_role) VALUES ($1, $2, 'write') RETURNING id")
                    .bind(format!("{name}-{tag}@test.dev"))
                    .bind(format!("{name}{tag}"))
                    .fetch_one(&pool)
                    .await?,
            );
        }
        let (owner, bob) = (people[0], people[1]);
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $1, 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("ag{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let said = |author: Uuid, body: &'static str| crate::world::NewMessage {
            agent_id: agent,
            author_kind: "user",
            author_user_id: Some(author),
            author_agent_id: None,
            body,
            session_id: None,
            run_id: None,
            meta: Value::Null,
        };
        let root = store.insert_message(said(owner, "plan the launch")).await?;
        let first = store.insert_reply(said(owner, "and the blog post"), Some(root.id)).await?;
        let answer = store
            .insert_reply(
                crate::world::NewMessage { author_kind: "agent", author_user_id: None, author_agent_id: Some(agent), ..said(owner, "on it") },
                Some(root.id),
            )
            .await?;
        sqlx::query("UPDATE agent_messages SET created_at = CASE id WHEN $1 THEN NOW() - INTERVAL '1 second' ELSE NOW() END WHERE id IN ($1, $2)")
            .bind(first.id)
            .bind(answer.id)
            .execute(&pool)
            .await?;
        assert_eq!(store.agent_thread_root(agent, first.id, owner).await?, Some(root.id));
        assert_eq!(store.agent_thread_root(agent, root.id, bob).await?, None, "bob cannot reply in the owner's thread");
        let timeline = store.list_messages(agent, owner, 50).await?;
        assert_eq!(timeline.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id]);
        assert_eq!(timeline[0].reply_count, 2);
        let authors = timeline[0].reply_authors.clone().unwrap_or_default();
        let kinds: Vec<&str> = authors.as_array().map(|a| a.iter().filter_map(|x| x["kind"].as_str()).collect()).unwrap_or_default();
        assert_eq!(kinds, vec!["agent", "user"], "latest replier first");
        let thread = store.message_replies(agent, root.id, owner, REPLY_LIMIT).await?;
        assert_eq!(thread.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, first.id, answer.id]);
        let recent = store.message_replies(agent, root.id, owner, 1).await?;
        assert_eq!(recent.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, answer.id]);
        let focused = store.message_replies_with_reply(agent, root.id, owner, 1, Some(first.id)).await?;
        assert_eq!(focused.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, first.id]);
        assert!(store.message_replies(agent, root.id, bob, REPLY_LIMIT).await?.is_empty());
        assert!(store.message_replies_with_reply(agent, root.id, bob, 1, Some(first.id)).await?.is_empty());
        let transcript = store.agent_thread_transcript(agent, root.id, 10).await?;
        assert!(transcript.contains("plan the launch") && transcript.ends_with(&format!("ag{tag}: on it")));
        Ok(())
    }

    #[test]
    fn reads_channel_message_refs() {
        assert_eq!(channel_reply_ref("msg:ch42"), Some(42));
        assert_eq!(channel_reply_ref("ch7"), Some(7));
        assert_eq!(channel_reply_ref("12"), Some(12));
        assert_eq!(channel_reply_ref("main"), None);
        assert_eq!(channel_reply_ref("0"), None);
    }
}
