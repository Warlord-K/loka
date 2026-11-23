use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::Response,
};
use loka_shared::access::{Role, Viewer};

use crate::auth::UserId;
use crate::AppState;

const CACHE_TTL: Duration = Duration::from_secs(20);

fn cache() -> &'static Mutex<HashMap<uuid::Uuid, (Viewer, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<uuid::Uuid, (Viewer, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn min_role(method: &Method, path: &str) -> Role {
    let path = path.strip_prefix("/api").unwrap_or(path).trim_end_matches('/');
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let get = method == Method::GET;
    match segs.as_slice() {
        ["world", "me"] => Role::View,
        ["world", "tokens", ..] => Role::View,
        ["world", "github", ..] => Role::Write,
        ["world", "agents"] if get => Role::View,
        ["world", "stream"] | ["world", "events"] => Role::View,
        ["world", "items", ..] if get => Role::View,
        ["world", "regions", ..] | ["world", "loka", ..] if get => Role::View,
        ["world", "progress"] | ["world", "challenges", ..] | ["world", "attempts", ..] | ["world", "adventures"] if get => Role::View,
        ["world", "npcs", _, "talk"] if get => Role::View,
        ["world", "npcs", ..] | ["world", "challenges", ..] | ["world", "academy", ..] => Role::Write,
        ["world", "todos", ..] => Role::View,
        ["world", "reports", ..] if get => Role::View,
        ["world", "reports", _, "presence"] => Role::View,
        ["world", "reports", ..] => Role::Write,
        ["world", "practice", ..] if get => Role::View,
        ["world", "practice", ..] => Role::Write,
        ["world", "search"] => Role::View,
        ["world", "readiness"] => Role::View,
        ["world", "notifications"] | ["world", "notifications", "seen"] => Role::View,
        ["world", "runners", ..] => Role::Write,
        ["world", "storage", ..] => Role::Write,
        ["world", "environment", ..] => Role::Write,
        ["world", "command-rules", ..] => Role::Write,
        ["world", "shared-keys", ..] => Role::Admin,
        ["world", "quests"] if get => Role::Read,
        ["world", "rules", ..] if !get => Role::Write,
        ["world", "mcp-servers", ..] if !get => Role::Admin,
        ["world", "skills", ..] if !get => Role::Admin,
        ["world", "agents", _] if get => Role::Read,
        ["world", "agents", _, "messages"] | ["world", "agents", _, "asides"] | ["world", "agents", _, "quick"] => Role::Write,
        ["world", "quick", ..] => Role::Write,
        ["world", "agents", _, "messages", _, "replies"] => Role::Write,
        ["world", "agents", _, "sessions"] | ["world", "agents", _, "runs"] | ["world", "agents", _, "kit"] => Role::Write,
        ["world", "agents", _, "feeds", ..] => Role::Write,
        ["world", "agents", _, "omp-config", ..] => Role::Write,
        ["world", "sessions", ..] | ["world", "runs", ..] => Role::Write,
        ["world", "tasks", _, "room"] if get => Role::View,
        ["world", "tasks", _, "room", "messages"] if *method == Method::POST => Role::View,
        ["world", "channels"] | ["world", "channels", _] if get => Role::Read,
        ["world", "channels", _, "messages"] if get => Role::Read,
        ["world", "channels", _, "read"] => Role::Read,
        ["world", "assistant"] => Role::Write,
        ["tasks"] | ["config"] | ["users"] if get => Role::Read,
        _ if get => Role::Read,
        _ => Role::Write,
    }
}

pub fn works_without_org(method: &Method, path: &str) -> bool {
    let path = path.strip_prefix("/api").unwrap_or(path).trim_end_matches('/');
    matches!((method, path), (&Method::GET, "/world/me") | (&Method::GET, "/config"))
}

pub async fn viewer_for(state: &AppState, user: uuid::Uuid) -> Result<Viewer, StatusCode> {
    let cached = cache()
        .lock()
        .ok()
        .and_then(|c| c.get(&user).cloned())
        .filter(|(_, at)| at.elapsed() < CACHE_TTL)
        .map(|(v, _)| v);
    if let Some(v) = cached {
        return Ok(v);
    }
    let v = state.world.resolve_viewer(user).await.map_err(|e| {
        tracing::error!(error = %e, "role lookup failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if let Ok(mut c) = cache().lock() {
        c.insert(user, (v.clone(), Instant::now()));
    }
    Ok(v)
}

pub fn posts_a_message(method: &Method, path: &str) -> bool {
    if method != Method::POST {
        return false;
    }
    let path = path.strip_prefix("/api").unwrap_or(path).trim_end_matches('/');
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    matches!(segs.as_slice(), ["world", "agents" | "channels" | "sessions" | "quick", _, "messages"] | ["world", "agents", _, "quick"] | ["world", "tasks", _, "room", "messages"])
}

pub fn refusal_reason(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "empty or invalid message",
        StatusCode::UNAUTHORIZED => "not signed in",
        StatusCode::FORBIDDEN => "not allowed",
        StatusCode::NOT_FOUND => "agent, channel, thread or quest not found",
        StatusCode::CONFLICT => "conflict",
        StatusCode::PAYLOAD_TOO_LARGE => "too large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE | StatusCode::UNPROCESSABLE_ENTITY => "unreadable request body",
        StatusCode::TOO_MANY_REQUESTS => "message rate limit",
        StatusCode::INSUFFICIENT_STORAGE => "disk full",
        s if s.is_server_error() => "server error",
        _ => "refused",
    }
}

fn log_refused_message(status: StatusCode, reason: &str, path: &str, user: Option<&UserId>) {
    if status.is_client_error() {
        tracing::warn!(status = status.as_u16(), reason, path, user = ?user.map(|u| u.0), "message not sent");
    }
}

async fn admit(state: &AppState, user: Option<UserId>, method: &Method, path: &str) -> Result<(UserId, Viewer), (StatusCode, &'static str)> {
    let user = user.ok_or((StatusCode::UNAUTHORIZED, "not signed in"))?;
    let viewer = viewer_for(state, user.0).await.map_err(|status| (status, refusal_reason(status)))?;
    if viewer.org.is_none() && !works_without_org(method, path) {
        return Err((StatusCode::FORBIDDEN, "no organization"));
    }
    if viewer.role < min_role(method, path) {
        return Err((StatusCode::FORBIDDEN, "role too low"));
    }
    Ok((user, viewer))
}

pub async fn role_middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let message = posts_a_message(&method, &path);
    let user = req.extensions().get::<UserId>().cloned();
    let (user, viewer) = match admit(&state, user.clone(), &method, &path).await {
        Ok(admitted) => admitted,
        Err((status, reason)) => {
            if message {
                log_refused_message(status, reason, &path, user.as_ref());
            }
            return Err(status);
        }
    };
    req.extensions_mut().insert(viewer.role);
    req.extensions_mut().insert(viewer);
    let response = next.run(req).await;
    if message {
        log_refused_message(response.status(), refusal_reason(response.status()), &path, Some(&user));
    }
    Ok(response)
}

#[derive(Debug, PartialEq)]
enum Need {
    See,
    Use,
    Manage,
}

fn world_agent_need(tail: &[&str], get: bool) -> Need {
    let read_or_manage = if get { Need::Use } else { Need::Manage };
    match tail {
        [] => if get { Need::See } else { Need::Manage },
        ["mcp-servers", ..] | ["skills", ..] | ["subscriptions"] | ["watches", ..] => read_or_manage,
        ["feeds", ..] | ["handoff"] | ["transfer"] | ["tidy"] | ["reset-context"] | ["omp-config", ..] => Need::Manage,
        ["schedules", ..] | ["voice", ..] => Need::Manage,
        ["messages", _, "rewind"] | ["asides"] | ["quick"] => Need::Manage,
        ["messages", ..] | ["fork"] => Need::Use,
        ["sessions" | "runs" | "usage" | "kit", ..] => Need::Manage,
        _ => read_or_manage,
    }
}

fn agent_allows(need: &Need, viewer: &Viewer, agent: &loka_shared::world::WorldAgent) -> bool {
    match need {
        Need::See => true,
        Need::Use => loka_shared::access::can_use(viewer, agent),
        Need::Manage => loka_shared::access::can_manage(viewer, agent),
    }
}

pub async fn agent_guard(State(state): State<AppState>, req: Request, next: Next) -> Result<Response, StatusCode> {
    let Some(viewer) = req.extensions().get::<Viewer>().cloned() else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let path = req.uri().path().strip_prefix("/api").unwrap_or(req.uri().path()).to_string();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let get = req.method() == Method::GET;
    let fail = |e: anyhow::Error| {
        tracing::error!(error = %e, "access lookup failed");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    let target = match segs.as_slice() {
        ["agents", id] => Some((id.parse::<uuid::Uuid>().ok(), if get { Need::See } else { Need::Manage })),
        ["agents", id, ..] => Some((id.parse::<uuid::Uuid>().ok(), Need::Manage)),
        ["world", "agents", id, tail @ ..] => Some((id.parse::<uuid::Uuid>().ok(), world_agent_need(tail, get))),
        ["world", "sessions", id, ..] => {
            let agent = match id.parse::<uuid::Uuid>() {
                Ok(sid) => state.world.get_session(sid).await.map_err(fail)?.map(|s| s.agent_id),
                Err(_) => None,
            };
            Some((agent, Need::Manage))
        }
        ["world", "runs", id, ..] => {
            let agent = match id.parse::<uuid::Uuid>() {
                Ok(rid) => state.world.run_agent_id(rid).await.map_err(fail)?,
                Err(_) => None,
            };
            Some((agent, Need::Manage))
        }
        _ => None,
    };
    if let Some((agent_id, need)) = target {
        let agent_id = agent_id.ok_or(StatusCode::NOT_FOUND)?;
        let agent = state.world.get_agent(agent_id).await.map_err(fail)?.ok_or(StatusCode::NOT_FOUND)?;
        if !loka_shared::access::can_see(&viewer, &agent) {
            return Err(StatusCode::NOT_FOUND);
        }
        if !agent_allows(&need, &viewer, &agent) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod agent_need_tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn the_crew_preview_needs_the_same_access_as_composing_a_quest() {
        assert_eq!(world_agent_need(&["crew-preview"], true), Need::Use);
        assert_eq!(world_agent_need(&["messages"], false), Need::Use, "composing a quest is posting to the agent");
        assert_eq!(world_agent_need(&[], true), Need::See);
        assert_eq!(world_agent_need(&["skills"], true), Need::Use);
        assert_eq!(world_agent_need(&["skills"], false), Need::Manage);
        assert_eq!(world_agent_need(&["messages", "m", "rewind"], false), Need::Manage);
        let owner = Uuid::new_v4();
        let org = Uuid::new_v4();
        let agent: loka_shared::world::WorldAgent = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "slug": "pip", "name": "Pip", "avatar_emoji": "a", "cli": "claude", "model": "opus",
            "role": "worker", "look": null, "gear": null, "status": "idle", "visibility": "private", "tags": [],
            "created_at": chrono::Utc::now(), "revoked_powers": [], "is_main": false, "jobs": [], "owner_user_id": owner, "org_id": org
        }))
        .unwrap();
        let viewer = |user: Uuid, role: Role| Viewer { user, org: Some(org), role, org_role: None, instance_admin: false };
        assert!(agent_allows(&Need::Use, &viewer(owner, Role::Write), &agent));
        assert!(!agent_allows(&Need::Use, &viewer(Uuid::new_v4(), Role::Write), &agent), "someone who cannot use the agent is refused");
        assert!(!agent_allows(&Need::Use, &viewer(owner, Role::Read), &agent), "read-only people cannot compose quests");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_table() {
        assert_eq!(min_role(&Method::GET, "/world/agents"), Role::View);
        assert_eq!(min_role(&Method::GET, "/api/world/agents/abc"), Role::Read);
        assert_eq!(min_role(&Method::POST, "/world/agents/abc/messages"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/messages"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/channels/abc/messages"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/channels/abc/messages"), Role::Read);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/messages/def/replies"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/channels/abc/messages/1/replies"), Role::Read);
        assert_eq!(min_role(&Method::POST, "/world/skills"), Role::Admin);
        assert_eq!(min_role(&Method::GET, "/world/skills"), Role::Read);
        assert_eq!(min_role(&Method::POST, "/world/runs/abc/retry"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/environment"), Role::Write);
        assert_eq!(min_role(&Method::PUT, "/world/environment/choices"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/environment/import"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/command-rules"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/command-rules/x"), Role::Write);
        assert_eq!(min_role(&Method::PUT, "/world/environment/TOKEN"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/environment/TOKEN"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/omp-config"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/agents/abc/omp-config/equip"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/tasks"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/api/world/tasks/abc/room"), Role::View);
        assert_eq!(min_role(&Method::POST, "/api/world/tasks/abc/room/messages"), Role::View);
        assert_eq!(min_role(&Method::POST, "/api/world/tasks/abc/room/members"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/api/world/tasks/abc/resume"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/sessions"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/feeds"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/agents/abc/feeds/def"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/regions/abc"), Role::View);
        assert_eq!(min_role(&Method::GET, "/world/items/abc"), Role::View);
        assert_eq!(min_role(&Method::GET, "/world/challenges/abc"), Role::View);
        assert_eq!(min_role(&Method::POST, "/world/challenges/abc/attempts"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/npcs/abc/talk"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/academy/offer"), Role::Write);
        assert_eq!(min_role(&Method::PUT, "/world/academy"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/progress"), Role::View);
        assert_eq!(min_role(&Method::POST, "/world/tokens"), Role::View);
        assert_eq!(min_role(&Method::DELETE, "/world/tokens/abc"), Role::View);
        assert_eq!(min_role(&Method::GET, "/world/reports/abc/revisions"), Role::View);
        assert_eq!(min_role(&Method::PATCH, "/world/reports/abc"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/reports/abc/revisions/2/restore"), Role::Write);
        assert!(works_without_org(&Method::GET, "/api/world/me"));
        assert!(!works_without_org(&Method::GET, "/api/world/agents"));
        assert!(!works_without_org(&Method::GET, "/api/world/stream"));
        assert!(posts_a_message(&Method::POST, "/api/world/agents/abc/messages"));
        assert!(posts_a_message(&Method::POST, "/api/world/channels/abc/messages"));
        assert!(!posts_a_message(&Method::GET, "/api/world/channels/abc/messages"));
        assert!(!posts_a_message(&Method::POST, "/api/world/agents/abc/messages/def/rewind"));
        assert_eq!(refusal_reason(StatusCode::INSUFFICIENT_STORAGE), "disk full");
        assert_eq!(refusal_reason(StatusCode::UNPROCESSABLE_ENTITY), "unreadable request body");
    }

    #[test]
    fn room_message_posts_use_message_rate_limit() {
        assert!(posts_a_message(&Method::POST, "/world/tasks/abc/room/messages"));
        assert!(posts_a_message(&Method::POST, "/api/world/tasks/abc/room/messages"));
        assert!(!posts_a_message(&Method::GET, "/api/world/tasks/abc/room/messages"));
        assert!(!posts_a_message(&Method::POST, "/api/world/tasks/abc/room/members"));
        assert!(!posts_a_message(&Method::POST, "/api/world/tasks/abc/room/messages/def"));
    }
}
