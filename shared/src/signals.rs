use anyhow::{ensure, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::world::{NewEvent, WorldEvent, WorldStore};

pub const CHANNEL: &str = "world_signals";
pub const KINDS: [&str; 2] = [crate::typing::EVENT, crate::canvas::PRESENCE_EVENT];
pub const MAX_BYTES: usize = 7_000;

pub fn ephemeral(event_type: &str) -> bool {
    KINDS.contains(&event_type)
}

pub fn wire(event: NewEvent, org: Option<Uuid>) -> Result<WorldEvent> {
    ensure!(ephemeral(&event.event_type), "{} is not a live-only signal", event.event_type);
    Ok(WorldEvent {
        seq: 0,
        event_type: event.event_type,
        agent_id: event.agent_id,
        session_id: event.session_id,
        run_id: event.run_id,
        task_id: event.task_id,
        actor_user_id: event.actor_user_id,
        payload: event.payload,
        created_at: Utc::now(),
        org_id: org,
    })
}

pub fn encode(event: &WorldEvent) -> Result<String> {
    let text = serde_json::to_string(event)?;
    ensure!(text.len() <= MAX_BYTES, "{} signal is {} bytes, over {MAX_BYTES}", event.event_type, text.len());
    Ok(text)
}

pub fn decode(raw: &str) -> Option<WorldEvent> {
    let event = serde_json::from_str::<WorldEvent>(raw).ok()?;
    (ephemeral(&event.event_type) && event.seq == 0).then_some(event)
}

pub fn concerns(event: &WorldEvent, org: Uuid) -> bool {
    event.org_id.is_none_or(|own| own == org)
}

pub fn needs_scope(event: &WorldEvent) -> bool {
    event.agent_id.is_some() || event.payload.get("channel_id").is_some_and(|id| !id.is_null()) || event.payload.get("thread_user_id").is_some_and(|id| !id.is_null())
}

pub async fn send<'e>(executor: impl sqlx::PgExecutor<'e>, event: NewEvent, org: Option<Uuid>) -> Result<()> {
    let text = encode(&wire(event, org)?)?;
    sqlx::query("SELECT pg_notify($1, $2)").bind(CHANNEL).bind(text).execute(executor).await?;
    Ok(())
}

impl WorldStore {
    pub async fn signal(&self, event: NewEvent, org: Option<Uuid>) -> Result<()> {
        send(self.pool(), event, org).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signals_round_trip_and_stay_live_only() {
        let org = Uuid::new_v4();
        let event = wire(NewEvent::new(crate::canvas::PRESENCE_EVENT).payload(json!({ "report_id": "r", "x": 1.0 })), Some(org)).unwrap();
        let back = decode(&encode(&event).unwrap()).unwrap();
        assert_eq!((back.seq, back.event_type.as_str(), back.org_id), (0, crate::canvas::PRESENCE_EVENT, Some(org)));
        assert!(concerns(&back, org) && !concerns(&back, Uuid::new_v4()));
        assert!(!needs_scope(&back));
        assert!(wire(NewEvent::new("report.updated"), None).is_err());
        let mut stored = back.clone();
        stored.seq = 9;
        assert!(decode(&serde_json::to_string(&stored).unwrap()).is_none());
        let typing = wire(NewEvent::new(crate::typing::EVENT).agent(Uuid::new_v4()), None).unwrap();
        assert!(needs_scope(&typing) && concerns(&typing, org));
        let in_thread = wire(NewEvent::new(crate::typing::EVENT).payload(json!({ "thread_user_id": org })), None).unwrap();
        assert!(needs_scope(&in_thread));
        let huge = wire(NewEvent::new(crate::typing::EVENT).payload(json!({ "agent": "x".repeat(MAX_BYTES) })), None).unwrap();
        assert!(encode(&huge).is_err());
    }
}
