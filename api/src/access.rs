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
        ["world", "items"] if get => Role::View,
        ["world", "regions", ..] | ["world", "loka", ..] if get => Role::View,
        ["world", "progress"] | ["world", "challenges", ..] | ["world", "attempts", ..] | ["world", "adventures"] if get => Role::View,
        ["world", "npcs", _, "talk"] if get => Role::View,
        ["world", "npcs", ..] | ["world", "challenges", ..] | ["world", "academy", ..] => Role::Write,
        ["world", "todos", ..] => Role::View,
        ["world", "reports", ..] if get => Role::View,
        ["world", "reports", ..] => Role::Write,
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
        ["world", "tasks", _, "room"] => Role::Write,
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

pub async fn role_middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let user = req.extensions().get::<UserId>().cloned().ok_or(StatusCode::UNAUTHORIZED)?;
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let viewer = viewer_for(&state, user.0).await?;
    if viewer.org.is_none() && !works_without_org(&method, &path) {
        return Err(StatusCode::FORBIDDEN);
    }
    if viewer.role < min_role(&method, &path) {
        return Err(StatusCode::FORBIDDEN);
    }
    req.extensions_mut().insert(viewer.role);
    req.extensions_mut().insert(viewer);
    Ok(next.run(req).await)
}

#[derive(PartialEq)]
enum Need {
    See,
    Use,
    Manage,
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
        ["world", "agents", id] => Some((id.parse::<uuid::Uuid>().ok(), if get { Need::See } else { Need::Manage })),
        ["world", "agents", id, "mcp-servers", ..] | ["world", "agents", id, "skills", ..] | ["world", "agents", id, "subscriptions"] | ["world", "agents", id, "watches", ..] => {
            Some((id.parse().ok(), if get { Need::Use } else { Need::Manage }))
        }
        ["world", "agents", id, "feeds", ..] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, "handoff"] | ["world", "agents", id, "tidy"] | ["world", "agents", id, "reset-context"] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, "omp-config", ..] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, "schedules", ..] | ["world", "agents", id, "voice", ..] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, "messages", _, "rewind"] | ["world", "agents", id, "asides"] | ["world", "agents", id, "quick"] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, "messages", ..] | ["world", "agents", id, "fork"] => Some((id.parse().ok(), Need::Use)),
        ["world", "agents", id, "sessions" | "runs" | "usage" | "kit", ..] => Some((id.parse().ok(), Need::Manage)),
        ["world", "agents", id, ..] => Some((id.parse().ok(), if get { Need::Use } else { Need::Manage })),
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
        let ok = match need {
            Need::See => true,
            Need::Use => loka_shared::access::can_use(&viewer, &agent),
            Need::Manage => loka_shared::access::can_manage(&viewer, &agent),
        };
        if !ok {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(next.run(req).await)
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
        assert_eq!(min_role(&Method::GET, "/world/command-rules"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/command-rules/x"), Role::Write);
        assert_eq!(min_role(&Method::PUT, "/world/environment/TOKEN"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/environment/TOKEN"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/omp-config"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/world/agents/abc/omp-config/equip"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/tasks"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/api/world/tasks/abc/room"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/sessions"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/agents/abc/feeds"), Role::Write);
        assert_eq!(min_role(&Method::DELETE, "/world/agents/abc/feeds/def"), Role::Write);
        assert_eq!(min_role(&Method::GET, "/world/regions/abc"), Role::View);
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
    }
}
