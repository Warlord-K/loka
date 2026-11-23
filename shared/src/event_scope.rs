use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::world::WorldEvent;

const ORG_VISIBLE: &[&str] = &[
    "agent.created",
    "agent.updated",
    "agent.status",
    "agent.progress",
    "agent.typing",
    "agent.seen",
    "agent.paused",
    "agent.resumed",
    "agent.removed",
    "agent.transferred",
    "agent.main_changed",
    "message.",
    "messages.rewound",
    "channel.",
    "run.started",
    "run.steered",
    "run.activity",
    "run.completed",
    "task.",
    "review.finished",
    "milestones.updated",
    "skill.updated",
    "item.",
    "timer.done",
    "region.",
    "report.",
    "presence.",
    "shell.",
    "slack.",
];

pub fn org_visible(event_type: &str) -> bool {
    ORG_VISIBLE
        .iter()
        .any(|allowed| if allowed.ends_with('.') { event_type.starts_with(allowed) } else { event_type == *allowed })
}

pub fn addressed_to(event: &WorldEvent, viewer: Uuid) -> bool {
    event
        .payload
        .get("audience")
        .and_then(Value::as_array)
        .is_some_and(|people| people.iter().any(|p| p.as_str().and_then(|id| id.parse::<Uuid>().ok()) == Some(viewer)))
}

pub fn private_from(event: &WorldEvent, viewer: Uuid, owned: &HashSet<Uuid>) -> bool {
    if let Some(outside) = crate::guests::outside_thread(event, viewer, owned) {
        return outside;
    }
    if org_visible(&event.event_type) {
        return false;
    }
    let owner = event.payload.get("owner_user_id").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
    let addressed = addressed_to(event, viewer);
    let mine = event.actor_user_id == Some(viewer) || owner == Some(viewer) || addressed || event.agent_id.is_some_and(|agent| owned.contains(&agent));
    !mine
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, agent: Option<Uuid>, actor: Option<Uuid>, payload: Value) -> WorldEvent {
        WorldEvent {
            seq: 1,
            event_type: kind.into(),
            agent_id: agent,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: actor,
            payload,
            created_at: chrono::Utc::now(),
            org_id: None,
        }
    }

    #[test]
    fn new_event_types_stay_with_their_owner() {
        let (me, other, mine, theirs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let owned = HashSet::from([mine]);
        assert!(!private_from(&event("task.finished", Some(theirs), None, json!({})), me, &owned));
        assert!(private_from(&event("message.created", Some(theirs), None, json!({})), me, &owned), "another owner's agent's replies stay in their thread");
        assert!(!private_from(&event("message.created", Some(mine), None, json!({})), me, &owned));
        assert!(!private_from(&event("message.created", Some(theirs), Some(me), json!({})), me, &owned), "my own message to a public agent");
        assert!(private_from(&event("message.created", Some(mine), Some(other), json!({})), me, &owned), "a guest's message stays in the guest's thread");
        let mine_thread = json!({ "thread_user_id": me.to_string() });
        assert!(!private_from(&event("message.created", Some(theirs), None, mine_thread.clone()), me, &owned), "a reply in my thread");
        assert!(private_from(&event("aside.answered", Some(theirs), None, mine_thread.clone()), other, &HashSet::from([theirs])), "even the owner stays out of a guest thread");
        assert!(!private_from(&event("task.finished", Some(theirs), None, json!({})), me, &owned));
        assert!(private_from(&event("brand.new", Some(theirs), None, json!({})), me, &owned), "unknown types are private");
        assert!(!private_from(&event("brand.new", Some(mine), None, json!({})), me, &owned));
        assert!(!private_from(&event("brand.new", None, Some(me), json!({})), me, &owned));
        assert!(!private_from(&event("brand.new", None, None, json!({ "owner_user_id": me })), me, &owned));
        assert!(private_from(&event("brand.new", None, None, json!({ "owner_user_id": other })), me, &owned));
        assert!(private_from(&event("run.failed", Some(theirs), None, json!({ "diagnosis": "tail" })), me, &owned));
        assert!(private_from(&event("session.replied", Some(theirs), None, json!({})), me, &owned));
        assert!(private_from(&event("tick", Some(theirs), None, json!({})), me, &owned));
        assert!(!org_visible("messages.edited"), "prefixes need their dot");
        let budget = json!({ "audience": [me.to_string()] });
        assert!(!private_from(&event("org.budget", None, None, budget.clone()), me, &owned), "the audience sees it");
        assert!(private_from(&event("org.budget", None, None, budget), other, &owned), "everyone else does not");
        let typing_for_guest = json!({ "typing": true, "thread_user_id": other.to_string() });
        assert!(private_from(&event("agent.typing", Some(mine), None, typing_for_guest.clone()), me, &owned), "typing in a guest thread stays with the guest");
        assert!(!private_from(&event("agent.typing", Some(mine), None, typing_for_guest), other, &HashSet::new()));
        let typing_in_channel = json!({ "typing": true, "channel_id": Uuid::new_v4().to_string() });
        assert!(!private_from(&event("agent.typing", Some(theirs), None, typing_in_channel), me, &owned), "channel typing follows the channel's own visibility");
    }
}
