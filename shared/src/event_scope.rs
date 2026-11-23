use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::world::WorldEvent;

pub(crate) const ORG_VISIBLE: &[&str] = &[
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

pub fn audience_scoped_card_invalidation(event: &WorldEvent) -> bool {
    matches!(event.event_type.as_str(), "task.changed" | "task.deleted")
        && (event.quest_scope.is_some() || event.agent_id.is_none())
}

pub fn creator_can_refresh(event: &WorldEvent, viewer: Uuid) -> bool {
    event.event_type == "task.deleted"
        && event
            .quest_scope
            .as_ref()
            .and_then(|scope| scope.get("creator_user_id"))
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<Uuid>().ok())
            == Some(viewer)
}

pub fn quest_scope_allows(event: &WorldEvent, viewer: Uuid) -> bool {
    let Some(delivery) = event.quest_scope.as_ref().and_then(|scope| scope.get("delivery")) else {
        return false;
    };
    scope_allows(delivery, viewer)
}

pub fn quest_detail_allows(event: &WorldEvent, viewer: Uuid) -> bool {
    let Some(scope) = event.quest_scope.as_ref() else {
        return false;
    };
    let Some(detail) = scope.get("detail").or_else(|| scope.get("delivery")) else {
        return false;
    };
    scope_allows(detail, viewer)
}

fn scope_allows(scope: &Value, viewer: Uuid) -> bool {
    let organization = scope.get("audience_org").and_then(Value::as_bool) == Some(true);
    let addressed = scope
        .get("audience")
        .and_then(Value::as_array)
        .is_some_and(|people| people.iter().any(|person| person.as_str().and_then(|id| id.parse::<Uuid>().ok()) == Some(viewer)));
    let excluded = scope
        .get("audience_exclude")
        .and_then(Value::as_array)
        .is_some_and(|people| people.iter().any(|person| person.as_str().and_then(|id| id.parse::<Uuid>().ok()) == Some(viewer)));
    (organization || addressed) && !excluded
}

pub fn card_invalidation_for_viewer(mut event: WorldEvent, viewer: Uuid, hidden: bool, admin: bool) -> WorldEvent {
    if event.event_type == "task.changed" && event.quest_scope.is_some() {
        event.agent_id = None;
        event.actor_user_id = None;
        return event;
    }
    if event.event_type != "task.deleted" || event.quest_scope.is_none() || (!hidden && (admin || quest_detail_allows(&event, viewer))) {
        return event;
    }
    let task_id = event.payload.get("task_id").cloned().unwrap_or(Value::Null);
    event.agent_id = None;
    event.session_id = None;
    event.run_id = None;
    event.task_id = None;
    event.actor_user_id = None;
    event.payload = serde_json::json!({
        "task_id": task_id,
        "quiet": true
    });
    event
}

pub fn private_from(event: &WorldEvent, viewer: Uuid, role: crate::access::Role, owned: &HashSet<Uuid>) -> bool {
    if let Some(open) = crate::rooms::room_open_to(event, viewer, role, owned) {
        return !open;
    }
    if let Some(outside) = crate::guests::outside_thread(event, viewer, owned) {
        return outside;
    }
    if audience_scoped_card_invalidation(event) {
        return if event.quest_scope.is_some() {
            !quest_scope_allows(event, viewer)
        } else {
            event.actor_user_id != Some(viewer) && !addressed_to(event, viewer)
        };
    }
    if org_visible(&event.event_type) {
        return false;
    }
    let owner = event.payload.get("owner_user_id").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
    let addressed = addressed_to(event, viewer);
    let mine = event.actor_user_id == Some(viewer) || owner == Some(viewer) || addressed || event.agent_id.is_some_and(|agent| owned.contains(&agent));
    !mine
}

pub fn private_from_admin(event: &WorldEvent, viewer: Uuid, owned: &HashSet<Uuid>, admin: bool) -> bool {
    if admin && audience_scoped_card_invalidation(event) {
        return event.quest_scope.as_ref().and_then(|scope| scope.get("former_only")).and_then(Value::as_bool) == Some(true);
    }
    private_from(
        event,
        viewer,
        if admin { crate::access::Role::Admin } else { crate::access::Role::Write },
        owned,
    )
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
            quest_scope: None,
        }
    }

    #[test]
    fn new_event_types_stay_with_their_owner() {
        let (me, other, mine, theirs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let owned = HashSet::from([mine]);
        assert!(!private_from(&event("task.finished", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("message.created", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned), "another owner's agent's replies stay in their thread");
        assert!(!private_from(&event("message.created", Some(mine), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(!private_from(&event("message.created", Some(theirs), Some(me), json!({})), me, crate::access::Role::Write, &owned), "my own message to a public agent");
        assert!(private_from(&event("message.created", Some(mine), Some(other), json!({})), me, crate::access::Role::Write, &owned), "a guest's message stays in the guest's thread");
        let mine_thread = json!({ "thread_user_id": me.to_string() });
        assert!(!private_from(&event("message.created", Some(theirs), None, mine_thread.clone()), me, crate::access::Role::Write, &owned), "a reply in my thread");
        assert!(private_from(&event("aside.answered", Some(theirs), None, mine_thread.clone()), other, crate::access::Role::Write, &HashSet::from([theirs])), "even the owner stays out of a guest thread");
        assert!(!private_from(&event("task.finished", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("brand.new", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned), "unknown types are private");
        assert!(!private_from(&event("brand.new", Some(mine), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(!private_from(&event("brand.new", None, Some(me), json!({})), me, crate::access::Role::Write, &owned));
        assert!(!private_from(&event("brand.new", None, None, json!({ "owner_user_id": me })), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("brand.new", None, None, json!({ "owner_user_id": other })), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("run.failed", Some(theirs), None, json!({ "diagnosis": "tail" })), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("session.replied", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(private_from(&event("tick", Some(theirs), None, json!({})), me, crate::access::Role::Write, &owned));
        assert!(!org_visible("messages.edited"), "prefixes need their dot");
        let budget = json!({ "audience": [me.to_string()] });
        assert!(!private_from(&event("org.budget", None, None, budget.clone()), me, crate::access::Role::Write, &owned), "the audience sees it");
        assert!(private_from(&event("org.budget", None, None, budget), other, crate::access::Role::Write, &owned), "everyone else does not");
        let mut card = event("task.changed", None, None, json!({ "task_id": Uuid::new_v4(), "quiet": true }));
        card.quest_scope = Some(json!({ "delivery": { "audience": [me.to_string()] } }));
        assert!(!private_from(&card, me, crate::access::Role::Write, &owned));
        assert!(private_from(&card, other, crate::access::Role::Write, &owned));
        card.quest_scope = Some(json!({ "delivery": { "audience_org": true, "audience_exclude": [other.to_string()] } }));
        assert!(!private_from(&card, me, crate::access::Role::Write, &owned));
        assert!(!private_from_admin(&card, other, &owned, true));
        card.quest_scope.as_mut().and_then(Value::as_object_mut).unwrap().insert("former_only".into(), json!(true));
        assert!(private_from_admin(&card, other, &owned, true));
        card.agent_id = Some(theirs);
        card.actor_user_id = Some(me);
        let changed = card_invalidation_for_viewer(card.clone(), me, false, false);
        assert_eq!((changed.agent_id, changed.actor_user_id), (None, None));
        card.event_type = "task.deleted".into();
        card.quest_scope.as_mut().and_then(Value::as_object_mut).unwrap().insert("creator_user_id".into(), json!(me));
        assert!(creator_can_refresh(&card, me));
        assert!(!creator_can_refresh(&card, other));
        card.quest_scope.as_mut().and_then(Value::as_object_mut).unwrap().insert("detail".into(), json!({ "audience": [me.to_string()] }));
        card.session_id = Some(Uuid::new_v4());
        card.run_id = Some(Uuid::new_v4());
        card.task_id = Some(Uuid::new_v4());
        card.actor_user_id = Some(me);
        let routed = card_invalidation_for_viewer(card.clone(), other, false, false);
        assert_eq!((routed.agent_id, routed.session_id, routed.run_id, routed.task_id, routed.actor_user_id), (None, None, None, None, None));
        assert_eq!(routed.payload, json!({ "task_id": card.payload["task_id"], "quiet": true }));
        assert_eq!(card_invalidation_for_viewer(card.clone(), me, false, false).agent_id, Some(theirs));
        let historical = event("task.deleted", None, Some(me), json!({ "task_id": Uuid::new_v4(), "audience": [me] }));
        assert!(!private_from(&historical, me, crate::access::Role::Write, &owned));
        assert!(private_from(&historical, other, crate::access::Role::Write, &owned));
        assert!(!private_from_admin(&historical, other, &owned, true));
        let actor_only = event("task.deleted", None, Some(me), json!({ "task_id": Uuid::new_v4() }));
        assert!(!private_from(&actor_only, me, crate::access::Role::Write, &owned));
        assert!(private_from(&actor_only, other, crate::access::Role::Write, &owned));
        let typing_for_guest = json!({ "typing": true, "thread_user_id": other.to_string() });
        assert!(private_from(&event("agent.typing", Some(mine), None, typing_for_guest.clone()), me, crate::access::Role::Write, &owned), "typing in a guest thread stays with the guest");
        assert!(!private_from(&event("agent.typing", Some(mine), None, typing_for_guest), other, crate::access::Role::Write, &HashSet::new()));
        let typing_in_channel = json!({ "typing": true, "channel_id": Uuid::new_v4().to_string() });
        assert!(!private_from(&event("agent.typing", Some(theirs), None, typing_in_channel), me, crate::access::Role::Write, &owned), "channel typing follows the channel's own visibility");
    }
}
