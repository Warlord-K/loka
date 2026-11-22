use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{NewMessage, WorldStore};

pub const SPOKEN_NOTE: &str = "[voice] They said this out loud and will hear your answer read aloud. Answer for the ear: one to three short spoken sentences, plain words, no markdown, tables, code blocks, links or long lists. Say numbers, names and times the way you would say them out loud. If there is a lot to say, give the gist and offer to put the details in the chat or a thread.";

pub fn spoken(payload: &Value) -> bool {
    ["spoken", "voice"].iter().any(|k| payload.get(*k).and_then(Value::as_bool) == Some(true))
}

pub fn with_note(prompt: String, payload: &Value) -> String {
    if spoken(payload) && !prompt.contains(SPOKEN_NOTE) {
        format!("{prompt}\n\n{SPOKEN_NOTE}")
    } else {
        prompt
    }
}

pub fn turn_key(raw: &str) -> Option<String> {
    let key: String = raw.trim().chars().take(64).collect();
    let fine = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    fine.then_some(key)
}

pub struct CallTurn<'a> {
    pub agent_id: Uuid,
    pub call_id: &'a str,
    pub turn: &'a str,
    pub from_user: Option<Uuid>,
    pub body: &'a str,
    pub partial: bool,
    pub thread: Option<Uuid>,
}

impl WorldStore {
    pub async fn save_call_turn(&self, t: CallTurn<'_>) -> Result<bool> {
        let author_kind = if t.from_user.is_some() { "user" } else { "agent" };
        let found: Option<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT id, body, COALESCE((meta->>'partial')::boolean, false) FROM agent_messages
             WHERE agent_id = $1 AND author_kind = $2 AND meta->>'kind' = 'voice' AND meta->>'call_id' = $3 AND meta->>'turn' = $4
               AND author_user_id IS NOT DISTINCT FROM $5
               AND meta->>'thread_user_id' IS NOT DISTINCT FROM $6
               AND created_at > NOW() - INTERVAL '1 day'
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(t.agent_id)
        .bind(author_kind)
        .bind(t.call_id)
        .bind(t.turn)
        .bind(t.from_user)
        .bind(t.thread.map(|u| u.to_string()))
        .fetch_optional(self.pool())
        .await?;
        if let Some((id, body, partial)) = found {
            if body == t.body && partial == t.partial {
                return Ok(false);
            }
            let flag = if t.partial { json!({ "partial": true }) } else { json!({}) };
            sqlx::query("UPDATE agent_messages SET body = $2, meta = (meta - 'partial') || $3, edited_at = NOW() WHERE id = $1")
                .bind(id)
                .bind(t.body)
                .bind(flag)
                .execute(self.pool())
                .await?;
            return Ok(true);
        }
        let mut meta = json!({ "kind": "voice", "call_id": t.call_id, "turn": t.turn });
        if t.partial {
            meta["partial"] = json!(true);
        }
        self.insert_message(NewMessage {
            agent_id: t.agent_id,
            author_kind,
            author_user_id: t.from_user,
            author_agent_id: None,
            body: t.body,
            session_id: None,
            run_id: None,
            meta: crate::guests::in_thread(meta, t.thread),
        })
        .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_keys_are_short_and_plain() {
        assert_eq!(turn_key(" a-resp_1 ").as_deref(), Some("a-resp_1"));
        assert_eq!(turn_key("u-item:2.0").as_deref(), Some("u-item:2.0"));
        assert_eq!(turn_key(""), None);
        assert_eq!(turn_key("a b"), None);
        assert_eq!(turn_key("a'--"), None);
        assert_eq!(turn_key(&"x".repeat(100)).map(|k| k.len()), Some(64));
    }

    #[tokio::test]
    async fn call_turns_land_in_the_chat_as_they_arrive() -> Result<()> {
        let Ok(url) = std::env::var("VOICE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("voice-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Callie', 'claude', 'haiku', $2) RETURNING id")
            .bind(format!("callie-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let turn = |turn: &'static str, from_user: Option<Uuid>, body: &'static str, partial: bool| CallTurn { agent_id: agent, call_id: "call-1", turn, from_user, body, partial, thread: None };

        assert!(store.save_call_turn(turn("u-1", Some(owner), "Can you check the build?", false)).await?);
        assert!(store.save_call_turn(turn("a-1", None, "Sure, I am", true)).await?);
        assert!(store.save_call_turn(turn("a-1", None, "Sure, I am looking at it", true)).await?);
        assert!(!store.save_call_turn(turn("a-1", None, "Sure, I am looking at it", true)).await?, "an unchanged turn is not written again");

        let rows: Vec<(String, String, Value)> = sqlx::query_as("SELECT author_kind, body, meta FROM agent_messages WHERE agent_id = $1 ORDER BY created_at")
            .bind(agent)
            .fetch_all(&pool)
            .await?;
        assert_eq!(rows.len(), 2, "each turn is one chat message, updated in place");
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("user", "Can you check the build?"));
        assert_eq!((rows[1].0.as_str(), rows[1].1.as_str()), ("agent", "Sure, I am looking at it"));
        assert_eq!(rows[1].2["partial"], json!(true), "a turn cut off mid sentence stays in the chat, marked partial");

        assert!(store.save_call_turn(turn("a-1", None, "Sure, I am looking at it now.", false)).await?);
        let (body, meta): (String, Value) = sqlx::query_as("SELECT body, meta FROM agent_messages WHERE agent_id = $1 AND author_kind = 'agent'")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(body, "Sure, I am looking at it now.");
        assert!(meta.get("partial").is_none());
        assert_eq!(meta["call_id"], json!("call-1"));

        let other: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("voice-other-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let theirs = CallTurn { thread: Some(other), ..turn("a-1", None, "Not yours to change", false) };
        assert!(store.save_call_turn(theirs).await?, "another caller's turn with the same keys is a new message");
        let mine: String = sqlx::query_scalar("SELECT body FROM agent_messages WHERE agent_id = $1 AND author_kind = 'agent' AND meta->>'thread_user_id' IS NULL")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(mine, "Sure, I am looking at it now.", "a call in another thread never rewrites the owner's message");

        let page = store.list_messages_page(agent, owner, 50, None, None).await?;
        assert_eq!(page.iter().filter(|m| m.meta["kind"] == json!("voice")).count(), 2, "the owner's chat shows both sides of the call");

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, other]).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn voice_items_get_one_note() {
        let once = with_note("hi".into(), &json!({ "spoken": true }));
        assert!(once.ends_with(SPOKEN_NOTE));
        assert_eq!(with_note(once.clone(), &json!({ "voice": true })), once);
        assert_eq!(with_note("hi".into(), &json!({ "spoken": false })), "hi");
        assert!(!SPOKEN_NOTE.contains('\u{2014}'));
    }
}
