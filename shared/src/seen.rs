use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::guests::in_thread;
use crate::world::{InboxItem, NewEvent, WorldAgent, WorldStore};

pub const EVENT: &str = "agent.seen";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub chat: Vec<Uuid>,
    pub channels: Vec<(Uuid, i64)>,
}

pub fn seen_in(items: &[(&str, &Value)]) -> Seen {
    let mut seen = Seen::default();
    for (kind, p) in items {
        if p.get("subscribed").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match *kind {
            "message" => {
                if let Some(id) = p.get("message_id").and_then(Value::as_str).and_then(|id| id.parse().ok()) {
                    if !seen.chat.contains(&id) {
                        seen.chat.push(id);
                    }
                }
            }
            "channel.mention" => {
                let channel = p.get("channel_id").and_then(Value::as_str).and_then(|id| id.parse().ok());
                if let (Some(channel), Some(message)) = (channel, p.get("message_id").and_then(Value::as_i64)) {
                    if !seen.channels.contains(&(channel, message)) {
                        seen.channels.push((channel, message));
                    }
                }
            }
            _ => {}
        }
    }
    seen
}

impl WorldStore {
    pub async fn mark_seen(&self, agent: &WorldAgent, items: &[InboxItem]) -> Result<()> {
        let pairs: Vec<(&str, &Value)> = items.iter().map(|i| (i.kind.as_str(), &i.payload)).collect();
        self.mark_seen_of(agent, &pairs).await
    }

    pub async fn mark_seen_of(&self, agent: &WorldAgent, items: &[(&str, &Value)]) -> Result<()> {
        let seen = seen_in(items);
        if !seen.chat.is_empty() {
            let rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(&format!(
                "UPDATE agent_messages m SET seen_at = NOW()
                 WHERE m.id = ANY($1) AND m.agent_id = $2 AND m.seen_at IS NULL
                 RETURNING m.id, {}",
                crate::guests::THREAD_KEY
            ))
            .bind(&seen.chat)
            .bind(agent.id)
            .fetch_all(self.pool())
            .await?;
            let mut threads: Vec<(Option<Uuid>, Vec<Uuid>)> = Vec::new();
            for (id, thread) in rows {
                let thread = thread.or(agent.owner_user_id);
                match threads.iter_mut().find(|(t, _)| *t == thread) {
                    Some((_, ids)) => ids.push(id),
                    None => threads.push((thread, vec![id])),
                }
            }
            for (thread, ids) in threads {
                let Some(thread) = thread else { continue };
                let payload = in_thread(json!({ "seen": ids, "agent": agent.name, "quiet": true }), Some(thread));
                self.emit(NewEvent::new(EVENT).agent(agent.id).payload(payload), None).await?;
            }
        }
        let mut channels: Vec<Uuid> = seen.channels.iter().map(|(c, _)| *c).collect();
        channels.sort();
        channels.dedup();
        for channel in channels {
            let ids: Vec<i64> = seen.channels.iter().filter(|(c, _)| *c == channel).map(|(_, m)| *m).collect();
            let marked: Vec<i64> = sqlx::query_scalar(
                "INSERT INTO channel_message_seen (message_id, agent_id)
                 SELECT m.id, $2 FROM channel_messages m WHERE m.id = ANY($1) AND m.channel_id = $3
                 ON CONFLICT DO NOTHING RETURNING message_id",
            )
            .bind(&ids)
            .bind(agent.id)
            .bind(channel)
            .fetch_all(self.pool())
            .await?;
            if marked.is_empty() {
                continue;
            }
            let payload = json!({ "seen": marked, "channel_id": channel, "agent": agent.name, "quiet": true });
            self.emit(NewEvent::new(EVENT).agent(agent.id).payload(payload), None).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, payload: Value) -> InboxItem {
        InboxItem { id: 1, agent_id: Uuid::nil(), kind: kind.into(), event_seq: None, payload, status: "pending".into(), attempts: 0, created_at: chrono::Utc::now() }
    }

    #[test]
    fn picks_the_messages_an_agent_picked_up() {
        let (message, channel) = (Uuid::new_v4(), Uuid::new_v4());
        let items = [
            item("message", json!({ "message_id": message.to_string() })),
            item("message", json!({ "message_id": message.to_string() })),
            item("channel.mention", json!({ "channel_id": channel.to_string(), "message_id": 7 })),
            item("channel.message", json!({ "channel_id": channel.to_string(), "message_id": 8, "subscribed": true })),
            item("task.assigned", json!({ "task_id": Uuid::new_v4().to_string() })),
        ];
        let pairs: Vec<(&str, &Value)> = items.iter().map(|i| (i.kind.as_str(), &i.payload)).collect();
        assert_eq!(seen_in(&pairs), Seen { chat: vec![message], channels: vec![(channel, 7)] });
    }

    #[tokio::test]
    async fn seen_marks_once_and_stays_in_the_thread() -> Result<()> {
        let Ok(url) = std::env::var("SEEN_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let user = |name: &str| {
            let pool = pool.clone();
            let (email, name) = (format!("{name}-{tag}@test.dev"), format!("{name}{tag}"));
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email, name, world_role) VALUES ($1, $2, 'write') RETURNING id")
                    .bind(email)
                    .bind(name)
                    .fetch_one(&pool)
                    .await
            }
        };
        let (owner, guest) = (user("owner").await?, user("guest").await?);
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        crate::orgs::join_default_for_tests(&pool, guest, None).await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility) VALUES ($1, $1, 'claude', 'sonnet', $2, 'public') RETURNING id",
        )
        .bind(format!("seen{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let message: Uuid = sqlx::query_scalar("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'hi') RETURNING id")
            .bind(agent_id)
            .bind(guest)
            .fetch_one(&pool)
            .await?;
        let channel = store.create_channel(None, &format!("seen{tag}"), None, owner).await?;
        let posted = store.post_channel_message(&channel, crate::channels::Author::User(owner), "hello", Value::Null).await?;
        let items = [
            item("message", json!({ "message_id": message.to_string() })),
            item("channel.mention", json!({ "channel_id": channel.id.to_string(), "message_id": posted.id })),
        ];
        store.mark_seen(&agent, &items).await?;
        store.mark_seen(&agent, &items).await?;
        let seen_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT seen_at FROM agent_messages WHERE id = $1").bind(message).fetch_one(&pool).await?;
        assert!(seen_at.is_some());
        let events: Vec<Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE agent_id = $1 AND type = 'agent.seen' ORDER BY seq")
            .bind(agent_id)
            .fetch_all(&pool)
            .await?;
        assert_eq!(events.len(), 2, "each place is announced once, repeats are quiet");
        assert_eq!(events[0]["thread_user_id"], json!(guest), "chat receipts stay in the guest's thread");
        assert_eq!(events[1]["channel_id"], json!(channel.id));
        let shown = store.channel_messages(channel.id, None, 10).await?;
        assert_eq!(shown.iter().find(|m| m.id == posted.id).map(|m| m.seen_by.clone()), Some(json!([agent_id])));
        Ok(())
    }
}
