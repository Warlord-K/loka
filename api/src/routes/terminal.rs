use axum::{
    extract::{ws::WebSocketUpgrade, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserId;
use crate::terminal::{self, TermGrant, TICKET_TTL_SECS};
use crate::AppState;

type ApiError = (StatusCode, Json<Value>);

fn problem(status: StatusCode, code: &str, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": code, "message": message.into() })))
}

fn internal(error: anyhow::Error) -> ApiError {
    tracing::error!(%error, "terminal route failed");
    problem(StatusCode::INTERNAL_SERVER_ERROR, "internal", "Terminal operation failed")
}

#[derive(Deserialize)]
pub struct NewTerminal {
    runner_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    task_id: Option<Uuid>,
    cols: Option<i32>,
    rows: Option<i32>,
}

pub const VIEW_NOT_YOURS: &str = "Only the owner of this agent can watch its browser";
pub const VIEW_NO_DEVICE: &str = "This agent has no device yet, so it has no browser to watch.";
pub const VIEW_NOT_ITS_QUEST: &str = "This quest is not this agent's, so its browser cannot be watched from here.";

fn view_refusal(device: &str, capabilities: &Value) -> Option<String> {
    if capabilities.get("browser").and_then(Value::as_bool) != Some(true) {
        return Some(format!("This agent works on {device}, which has no agent browser to watch."));
    }
    (!loka_shared::terminal::runner_shows_browsers(capabilities)).then(|| format!("Update Loka on {device} to watch agent browsers inside Loka."))
}

async fn view_target(state: &AppState, user: Uuid, agent_id: Uuid) -> Result<(Uuid, String), ApiError> {
    let agent = state
        .world
        .get_agent(agent_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "not_found", "Agent not found"))?;
    if agent.owner_user_id != Some(user) {
        return Err(problem(StatusCode::FORBIDDEN, "not_owner", VIEW_NOT_YOURS));
    }
    let runner_id = agent.runner_id.ok_or_else(|| problem(StatusCode::CONFLICT, "no_device", VIEW_NO_DEVICE))?;
    Ok((runner_id, agent.name))
}

pub async fn open_session(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    headers: HeaderMap,
    Json(request): Json<NewTerminal>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let view = match request.agent_id {
        Some(agent_id) => Some((agent_id, view_target(&state, user.0, agent_id).await?)),
        None => None,
    };
    let quest = match (&view, request.task_id) {
        (Some((agent_id, _)), Some(task_id)) => {
            if !state.world.agent_works_on(*agent_id, task_id).await.map_err(internal)? {
                return Err(problem(StatusCode::NOT_FOUND, "not_found", VIEW_NOT_ITS_QUEST));
            }
            Some(task_id)
        }
        _ => None,
    };
    let runner_id = match (&view, request.runner_id) {
        (Some((_, (runner_id, _))), _) => *runner_id,
        (None, Some(runner_id)) => runner_id,
        (None, None) => return Err(problem(StatusCode::BAD_REQUEST, "missing_device", "Pick a device")),
    };
    let runner = state
        .runners
        .get_live(runner_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "not_found", "Device not found"))?;
    if runner.owner_user_id != user.0 {
        return Err(problem(StatusCode::FORBIDDEN, "not_owner", "Only the owner of this Loka can open a terminal"));
    }
    if runner.status != "online" {
        return Err(problem(StatusCode::CONFLICT, "device_offline", format!("{} is offline. Start it, then try again.", runner.name)));
    }
    if let Some(refusal) = view.as_ref().and_then(|_| view_refusal(&runner.name, &runner.capabilities)) {
        return Err(problem(StatusCode::CONFLICT, "no_browser_view", refusal));
    }
    let cols = request.cols.unwrap_or(80).clamp(20, 400);
    let rows = request.rows.unwrap_or(24).clamp(5, 120);
    let cap = if view.is_some() { loka_shared::terminal::max_views_per_device() } else { loka_shared::terminal::max_per_device() };
    let session = match state
        .world
        .create_terminal(user.0, runner.id, view.as_ref().map(|(agent, _)| loka_shared::terminal::Watched { agent: *agent, quest }), cols, rows, cap)
        .await
        .map_err(internal)?
    {
        loka_shared::terminal::NewTerminal::Created(session) => session,
        loka_shared::terminal::NewTerminal::Busy { .. } => {
            let what = if view.is_some() { "browser views" } else { "terminals" };
            return Err(problem(
                StatusCode::CONFLICT,
                "device_busy",
                format!("{} runs at most {cap} {what} at once. Close one to open another.", runner.name),
            ));
        }
    };
    let exp = chrono::Utc::now().timestamp() + TICKET_TTL_SECS;
    let grant = TermGrant { user: user.0, session: session.id, runner: runner.id, exp };
    let ticket = terminal::sign(&grant, &terminal::ticket_secret());
    let origin = public_origin(&headers);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session_id": session.id,
            "device": runner.name,
            "agent": view.map(|(_, (_, name))| name),
            "socket": socket_url(&origin, &ticket),
            "expires_at": exp,
        })),
    ))
}

fn public_origin(headers: &HeaderMap) -> String {
    if let Ok(url) = std::env::var("WORLD_PUBLIC_API_URL") {
        if !url.trim().is_empty() {
            return url.trim().trim_end_matches('/').to_string();
        }
    }
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("localhost:8081");
    let local = host.starts_with("localhost") || host.starts_with("127.0.0.1") || host.starts_with("[::1]");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        .filter(|v| v == "http" || v == "https")
        .unwrap_or_else(|| if local { "http".into() } else { "https".into() });
    format!("{proto}://{host}")
}

fn socket_url(origin: &str, ticket: &str) -> String {
    let ws = if let Some(rest) = origin.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = origin.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        origin.to_string()
    };
    format!("{ws}/api/terminal/socket?ticket={}", urlencoding::encode(ticket))
}

#[derive(Deserialize)]
pub struct TicketQuery {
    ticket: String,
}

pub async fn browser_socket(State(state): State<AppState>, Query(query): Query<TicketQuery>, upgrade: WebSocketUpgrade) -> Response {
    let Some(grant) = terminal::verify(&query.ticket, &terminal::ticket_secret(), chrono::Utc::now().timestamp()) else {
        return problem(StatusCode::UNAUTHORIZED, "invalid_ticket", "This terminal link expired. Open the terminal again.").into_response();
    };
    let session = match state.world.terminal(grant.session).await {
        Ok(Some(session)) => session,
        Ok(None) => return problem(StatusCode::NOT_FOUND, "not_found", "Session not found").into_response(),
        Err(error) => return internal(error).into_response(),
    };
    if session.owner_user_id != grant.user {
        return problem(StatusCode::FORBIDDEN, "not_owner", "Only the owner of this Loka can open a terminal").into_response();
    }
    if session.closed_at.is_some() || session.runner_id != grant.runner {
        return problem(StatusCode::CONFLICT, "closed", "That terminal session has ended. Open a new one.").into_response();
    }
    let device = state
        .runners
        .get_live(session.runner_id)
        .await
        .map(|r| r.map(|r| r.name).unwrap_or_else(|| "device".to_string()))
        .unwrap_or_else(|_| "device".to_string());
    let (hub, world) = (state.term.clone(), state.world.clone());
    upgrade
        .max_message_size(loka_shared::terminal::BROWSER_FRAME_MAX)
        .max_frame_size(loka_shared::terminal::BROWSER_FRAME_MAX)
        .on_upgrade(move |socket| terminal::serve_browser(socket, hub, world, grant, device))
}

pub async fn claim(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let runner = super::runners::authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if runner.status == "paused" {
        return Err(problem(StatusCode::CONFLICT, "runner_paused", "Runner is paused"));
    }
    let Some(job) = state.runners.claim_terminal(runner_id, loka_shared::terminal::TerminalRoom::SHELLS).await.map_err(internal)? else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    Ok(Json(job).into_response())
}

pub async fn runner_socket(
    State(state): State<AppState>,
    Path((runner_id, session_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Runner "))
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return problem(StatusCode::UNAUTHORIZED, "missing_authorization", "Authorization is required").into_response();
    };
    let runner = match state.runners.authenticate(runner_id, token).await {
        Ok(Some(runner)) if runner.id == runner_id => runner,
        Ok(_) => return problem(StatusCode::UNAUTHORIZED, "invalid_runner", "Runner credential is invalid").into_response(),
        Err(error) => return internal(error).into_response(),
    };
    let session = match state.world.terminal(session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => return problem(StatusCode::NOT_FOUND, "not_found", "Session not found").into_response(),
        Err(error) => return internal(error).into_response(),
    };
    if session.runner_id != runner_id || session.status != "open" || session.closed_at.is_some() {
        return problem(StatusCode::CONFLICT, "closed", "That terminal session is not open anymore.").into_response();
    }
    tracing::info!(target: "terminal_audit", session = %session_id, runner = %runner.id, owner = %runner.owner_user_id, name = %runner.name, "terminal: device connected");
    let (hub, world) = (state.term.clone(), state.world.clone());
    upgrade
        .max_message_size(loka_shared::terminal::RUNNER_FRAME_MAX)
        .max_frame_size(loka_shared::terminal::RUNNER_FRAME_MAX)
        .on_upgrade(move |socket| terminal::serve_runner(socket, hub, world, runner_id, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_says_why_a_device_cannot_show_the_browser() {
        assert_eq!(view_refusal("Studio", &json!({"browser": true, "browser_view": true})), None);
        assert_eq!(view_refusal("Studio", &json!({"browser": true})).as_deref(), Some("Update Loka on Studio to watch agent browsers inside Loka."));
        assert_eq!(view_refusal("Cloud", &json!({})).as_deref(), Some("This agent works on Cloud, which has no agent browser to watch."));
        assert_eq!(view_refusal("Cloud", &json!({"browser": false, "browser_view": true})).as_deref(), Some("This agent works on Cloud, which has no agent browser to watch."));
    }
}
