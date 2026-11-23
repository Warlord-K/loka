use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::guests::{asked_by_user, in_thread};
use crate::world::{InboxItem, NewEvent, WorldAgent, WorldStore};

pub const EVENT: &str = "agent.typing";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Chat(Uuid),
    Channel(Uuid, Option<i64>),
}

fn thread_root(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn from_person(item: &InboxItem) -> bool {
    item.payload.get("author_kind").and_then(Value::as_str) != Some("agent") && item.payload.get("subscribed").and_then(Value::as_bool) != Some(true)
}

pub fn places(owner: Option<Uuid>, thread: Option<Uuid>, items: &[InboxItem]) -> Vec<Place> {
    let mut places = Vec::new();
    let mut push = |place: Place| {
        if !places.contains(&place) {
            places.push(place);
        }
    };
    if items.iter().any(|i| i.kind == "message" && from_person(i)) {
        if let Some(user) = thread.or(owner) {
            push(Place::Chat(user));
        }
    }
    for item in items.iter().filter(|i| i.kind == "channel.mention") {
        if let Some(id) = item.payload.get("channel_id").and_then(Value::as_str).and_then(|id| id.parse().ok()) {
            push(Place::Channel(id, thread_root(item.payload.get("reply_to"))));
        }
    }
    places
}

pub fn aside_place(asked_by: &Value) -> Option<Place> {
    let origin = asked_by.get("origin")?;
    match origin.get("kind").and_then(Value::as_str)? {
        "thread" => asked_by_user(asked_by).map(Place::Chat),
        "channel" => origin
            .get("channel_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse().ok())
            .map(|id| Place::Channel(id, thread_root(origin.get("reply_to")))),
        _ => None,
    }
}

pub fn payload(agent_name: &str, place: Place, typing: bool) -> Value {
    let base = json!({ "typing": typing, "agent": agent_name, "quiet": true });
    match place {
        Place::Chat(user) => in_thread(base, Some(user)),
        Place::Channel(id, root) => {
            let mut base = base;
            base["channel_id"] = json!(id);
            base["reply_to"] = json!(root);
            base
        }
    }
}

impl WorldStore {
    pub async fn turn_typing_places(&self, agent: &WorldAgent, items: &[InboxItem]) -> Vec<Place> {
        let thread = self.reply_thread(agent, items).await.unwrap_or(None);
        places(agent.owner_user_id, thread, items)
    }

    pub async fn set_typing(&self, agent: &WorldAgent, places: &[Place], typing: bool, run_id: Option<Uuid>, session_id: Option<Uuid>) -> Result<()> {
        for place in places {
            let mut event = NewEvent::new(EVENT).agent(agent.id).payload(payload(&agent.name, *place, typing));
            if let Some(run) = run_id {
                event = event.run(run);
            }
            if let Some(session) = session_id {
                event = event.session(session);
            }
            self.signal(event, agent.org_id).await?;
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
    fn typing_follows_where_the_reply_lands() {
        let (owner, guest, channel) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let message = item("message", json!({ "author_kind": "user", "body": "hi" }));
        let from_agent = item("message", json!({ "author_kind": "agent", "body": "fyi" }));
        let mention = item("channel.mention", json!({ "channel_id": channel.to_string(), "author_kind": "user" }));
        let task = item("task.assigned", json!({ "task_id": Uuid::new_v4().to_string() }));
        assert_eq!(places(Some(owner), None, std::slice::from_ref(&message)), vec![Place::Chat(owner)]);
        assert_eq!(places(Some(owner), Some(guest), std::slice::from_ref(&message)), vec![Place::Chat(guest)]);
        assert_eq!(places(Some(owner), None, &[from_agent, task.clone()]), Vec::<Place>::new());
        assert_eq!(places(None, None, std::slice::from_ref(&message)), Vec::<Place>::new());
        assert_eq!(places(Some(owner), None, &[message, mention.clone(), mention]), vec![Place::Chat(owner), Place::Channel(channel, None)]);
        let in_thread_reply = item("channel.mention", json!({ "channel_id": channel.to_string(), "author_kind": "user", "reply_to": 42 }));
        assert_eq!(places(Some(owner), None, std::slice::from_ref(&in_thread_reply)), vec![Place::Channel(channel, Some(42))]);
        assert_eq!(payload("Priya", Place::Channel(channel, Some(42)), true)["reply_to"], json!(42));
        let thread = payload("Priya", Place::Chat(guest), true);
        assert_eq!(thread["thread_user_id"], json!(guest));
        assert!(thread.get("channel_id").is_none());
        assert_eq!(payload("Priya", Place::Channel(channel, None), false)["channel_id"], json!(channel));
        let asked = json!({ "user_id": guest.to_string(), "origin": { "kind": "thread" } });
        assert_eq!(aside_place(&asked), Some(Place::Chat(guest)));
        let asked = json!({ "origin": { "kind": "channel", "channel_id": channel.to_string() } });
        assert_eq!(aside_place(&asked), Some(Place::Channel(channel, None)));
        let asked = json!({ "origin": { "kind": "channel", "channel_id": channel.to_string(), "reply_to": 7 } });
        assert_eq!(aside_place(&asked), Some(Place::Channel(channel, Some(7))));
        assert_eq!(aside_place(&json!({ "origin": { "kind": "agent" } })), None);
    }
}
