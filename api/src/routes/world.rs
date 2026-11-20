use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Extension, Json,
};
use chrono::{DateTime, Utc};
use loka_shared::world::{
    AgentMessage, AgentSchedule, AgentSession, AgentUpdate, HandleRename, McpServerEntry, RunEvent, SessionBrief, NewAgent, NewEvent, NewMessage, RenameHandleError, RetryRunError, Rewound, SessionRunView, SkillEntry, WorldRule, SessionRun, WorldAgent, WorldEvent,
};
use loka_shared::links::LinkedMessage;
use loka_shared::quests::PlanAnswer;
use loka_shared::runners::{AgentMove, HandoffRunnerError};
use loka_shared::milestones::{Milestone, MilestoneStore, NewMilestone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{error, warn};
use uuid::Uuid;

use crate::auth::UserId;
use loka_shared::access::{self, Role, Viewer};
use loka_shared::environment::secret_reference_names;
use crate::AppState;

#[derive(Serialize)]
pub struct AgentView {
    #[serde(flatten)]
    agent: WorldAgent,
    cost_usd: f64,
    tokens: i64,
    pending_switch: Option<Value>,
}

#[derive(Serialize)]
pub struct AgentDetail {
    #[serde(flatten)]
    agent: AgentView,
    main_session: Option<AgentSession>,
    schedules: Vec<AgentSchedule>,
}

pub(crate) fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "world route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn view(state: &AppState, agent: WorldAgent) -> Result<AgentView, StatusCode> {
    let (cost_usd, tokens) = state.world.agent_cost(agent.id).await.map_err(internal)?;
    let pending_switch = state
        .runners
        .in_place_switch(agent.id)
        .await
        .map_err(internal)?
        .map(|(cli, model)| json!({ "cli": cli, "model": model }));
    Ok(AgentView { agent, cost_usd, tokens, pending_switch })
}

pub(crate) async fn load(state: &AppState, id: Uuid) -> Result<WorldAgent, StatusCode> {
    state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)
}

pub async fn list_agents(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Vec<AgentView>>, StatusCode> {
    let agents: Vec<WorldAgent> = state
        .world
        .list_agents_in(viewer.org)
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|agent| access::can_see(&viewer, agent))
        .collect();
    let ids: Vec<Uuid> = agents.iter().map(|a| a.id).collect();
    let (costs, switches) = tokio::try_join!(state.world.agent_costs(&ids), state.runners.in_place_switches(&ids)).map_err(internal)?;
    let mut switches: std::collections::HashMap<Uuid, Value> =
        switches.into_iter().map(|(id, cli, model)| (id, json!({ "cli": cli, "model": model }))).collect();
    Ok(Json(
        agents
            .into_iter()
            .map(|agent| {
                let (cost_usd, tokens) = costs.get(&agent.id).copied().unwrap_or((0.0, 0));
                let pending_switch = switches.remove(&agent.id);
                AgentView { agent, cost_usd, tokens, pending_switch }
            })
            .collect(),
    ))
}

pub async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentDetail>, StatusCode> {
    let (agent, main_session, schedules) = tokio::try_join!(
        load(&state, id),
        async { state.world.main_session(id).await.map_err(internal) },
        async { state.world.list_agent_schedules(id).await.map_err(internal) },
    )?;
    Ok(Json(AgentDetail { agent: view(&state, agent).await?, main_session, schedules }))
}

#[derive(Deserialize)]
pub struct CreateAgent {
    name: String,
    persona: Option<String>,
    cli: Option<String>,
    model: Option<String>,
    avatar_emoji: Option<String>,
    look: Option<Value>,
    gear: Option<Value>,
    visibility: Option<String>,
    tags: Option<Vec<String>>,
    expertise: Option<String>,
}

pub async fn create_agent(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<CreateAgent>,
) -> axum::response::Response {
    if body.cli.as_deref() == Some(loka_shared::backends::OPENCODE) {
        match opencode_offered(&state, user.0).await {
            Err(status) => return status.into_response(),
            Ok(false) => return opencode_disabled().into_response(),
            Ok(true) => {}
        }
    }
    insert_agent(state, user, body).await.into_response()
}

pub async fn opencode_offered(state: &AppState, user: Uuid) -> Result<bool, StatusCode> {
    if loka_shared::backends::opencode_enabled() {
        return Ok(true);
    }
    let runners = state.runners.list(user).await.map_err(internal)?;
    Ok(runners.iter().any(|r| loka_shared::backends::runner_offers_opencode(&r.capabilities)))
}

fn opencode_disabled() -> (StatusCode, Json<Value>) {
    reject(StatusCode::BAD_REQUEST, "opencode_disabled", Some(loka_shared::backends::OPENCODE_DISABLED))
}

async fn insert_agent(state: AppState, user: UserId, body: CreateAgent) -> Result<(StatusCode, Json<AgentView>), StatusCode> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let visibility = body.visibility.as_deref().unwrap_or("private");
    if !matches!(visibility, "public" | "private") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let base = slugify(name);
    let mut slug = base.clone();
    let mut n = 2;
    while state.world.slug_taken(&slug).await.map_err(internal)? {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let cli = body.cli.as_deref().unwrap_or(state.config.default_cli());
    if !loka_shared::backends::is_agent_cli(cli) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let model = body
        .model
        .as_deref()
        .unwrap_or(match cli {
            "codex" => "gpt-5.4",
            "omp" => "fal/kimi-k3",
            _ => "sonnet",
        });
    let agent = state
        .world
        .create_agent(NewAgent {
            slug: &slug,
            name,
            avatar_emoji: body.avatar_emoji.as_deref().unwrap_or("🦊"),
            cli,
            model,
            persona: body.persona.as_deref(),
            role: "worker",
            look: body.look.unwrap_or_else(|| json!({})),
            gear: body.gear.unwrap_or_else(|| json!({})),
            visibility,
            owner_user_id: Some(user.0),
            org_id: None,
        })
        .await
        .map_err(internal)?;
    let tags = loka_shared::expertise::merge_tags(&[], &body.tags.unwrap_or_default());
    if !tags.is_empty() || body.expertise.as_deref().is_some_and(|e| !e.trim().is_empty()) {
        state
            .world
            .set_expertise(agent.id, &tags, body.expertise.as_deref().map(str::trim).filter(|e| !e.is_empty()))
            .await
            .map_err(internal)?;
    }
    let agent = state.world.get_agent(agent.id).await.map_err(internal)?.unwrap_or(agent);
    state
        .world
        .emit(
            NewEvent::new("agent.created")
                .agent(agent.id)
                .actor(Some(user.0))
                .payload(json!({ "name": agent.name, "slug": agent.slug })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(view(&state, agent).await?)))
}

pub async fn update_agent(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<AgentUpdate>,
) -> axum::response::Response {
    if let Err(response) = opencode_update_allowed(&state, user.0, id, &body).await {
        return *response;
    }
    match invalid_cli_switch(&state, id, &body).await {
        Err(status) => status.into_response(),
        Ok(Some(message)) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_cli_switch", "message": message }))).into_response()
        }
        Ok(None) => apply_agent_update(state, user, id, body).await.into_response(),
    }
}

pub(crate) async fn opencode_update_allowed(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    body: &AgentUpdate,
) -> Result<(), Box<axum::response::Response>> {
    use loka_shared::backends::OPENCODE;
    let reviewer_cli = body
        .reviewer
        .as_ref()
        .filter(|v| !v.is_null())
        .and_then(loka_shared::world::ReviewerChoice::from_value)
        .and_then(|c| c.cli);
    if reviewer_cli.as_deref() == Some(OPENCODE) && !opencode_offered(state, user).await.map_err(|e| Box::new(e.into_response()))? {
        return Err(Box::new(opencode_disabled().into_response()));
    }
    if body.cli.as_deref() != Some(OPENCODE) {
        return Ok(());
    }
    let agent = load(state, id).await.map_err(|e| Box::new(e.into_response()))?;
    if agent.cli == OPENCODE || opencode_allowed_for(state, &agent).await.map_err(|e| Box::new(e.into_response()))? {
        Ok(())
    } else {
        Err(Box::new(opencode_disabled().into_response()))
    }
}

pub(crate) async fn opencode_allowed_for(state: &AppState, agent: &WorldAgent) -> Result<bool, StatusCode> {
    let runner = match agent.owner_user_id {
        Some(owner) => state.runners.agent_runner(owner, agent.id).await.map_err(internal)?,
        None => None,
    };
    Ok(loka_shared::backends::allowed_on(loka_shared::backends::OPENCODE, runner.as_ref().map(|r| &r.capabilities)))
}

pub(crate) async fn invalid_cli_switch(state: &AppState, id: Uuid, body: &AgentUpdate) -> Result<Option<String>, StatusCode> {
    if body.cli.is_some() || body.model.is_some() {
        if let Some(place) = state.runners.move_queued_elsewhere(id).await.map_err(internal)? {
            return Ok(Some(format!("A move to {place} is queued; cancel it or change the CLI there")));
        }
    }
    let Some(cli) = body.cli.as_deref() else {
        return Ok(None);
    };
    if !state.runners.backend_change_requires_handoff(id, cli).await.map_err(internal)? {
        return Ok(None);
    }
    let agent = load(state, id).await?;
    let Some(owner) = agent.owner_user_id else {
        return Ok(Some("This agent has no owner, so its CLI cannot be switched".into()));
    };
    let Some(runner) = state.runners.agent_runner(owner, id).await.map_err(internal)? else {
        return Ok(Some("This agent has no device to switch its CLI on".into()));
    };
    if runner.capabilities.get("backends").and_then(|b| b.get(cli)).and_then(Value::as_bool) == Some(false) {
        return Ok(Some(format!("{} does not have the {cli} CLI installed", runner.name)));
    }
    let model = body.model.as_deref().unwrap_or(&agent.model).trim();
    if model.is_empty() || model.chars().count() > 200 {
        return Ok(Some("Model must be between 1 and 200 characters".into()));
    }
    Ok(None)
}

fn reject(status: StatusCode, key: &str, message: Option<&str>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": key, "message": message.unwrap_or(key) })))
}

pub(crate) async fn apply_agent_update(
    state: AppState,
    user: UserId,
    id: Uuid,
    mut body: AgentUpdate,
) -> Result<Json<AgentView>, (StatusCode, Json<Value>)> {
    if let Some(reviewer) = body.reviewer.as_mut().filter(|v| !v.is_null()) {
        *reviewer = loka_shared::world::ReviewerChoice::from_value(reviewer)
            .ok_or_else(|| reject(StatusCode::BAD_REQUEST, "invalid_reviewer", None))?
            .to_value();
    }
    if body.cli.as_deref().is_some_and(|c| !loka_shared::backends::is_agent_cli(c))
        || body.visibility.as_deref().is_some_and(|v| !matches!(v, "public" | "private"))
        || body.name.as_deref().is_some_and(|n| n.trim().is_empty())
        || body.revoked_powers.as_ref().is_some_and(|p| p.iter().any(|n| !loka_shared::powers::is_power(n.trim())))
        || body.approval_mode.as_deref().is_some_and(|m| !loka_shared::world::APPROVAL_MODES.contains(&m.trim()))
        || !body.budget_valid()
    {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid_update", None));
    }
    if body.handle.as_deref().is_some_and(|h| loka_shared::world::normalize_handle(h).is_none()) {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid_handle", Some("Handles use lowercase letters, digits and dashes")));
    }
    let before = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    let mut switch = None;
    if let Some(cli) = body.cli.clone() {
        if state
            .runners
            .backend_change_requires_handoff(id, &cli)
            .await
            .map_err(|e| reject(internal(e), "internal", None))?
        {
            let model = body.model.take().unwrap_or_else(|| before.model.clone());
            body.cli = None;
            switch = Some((cli, model));
        }
    }
    if switch.is_none() && (body.cli.is_some() || body.model.is_some()) {
        state.runners.drop_in_place_move(id).await.map_err(|e| reject(internal(e), "internal", None))?;
    }
    let mut agent = state
        .world
        .update_agent(id, &body)
        .await
        .map_err(|e| reject(internal(e), "internal", None))?
        .ok_or_else(|| reject(StatusCode::NOT_FOUND, "not_found", None))?;
    if let Some((cli, model)) = switch {
        let owner = before.owner_user_id.ok_or_else(|| reject(StatusCode::CONFLICT, "no_owner", None))?;
        let runner = state
            .runners
            .agent_runner(owner, id)
            .await
            .map_err(|e| reject(internal(e), "internal", None))?
            .ok_or_else(|| reject(StatusCode::CONFLICT, "no_runner", None))?;
        let by = state.world.user_display_name(user.0).await.map_err(|e| reject(internal(e), "internal", None))?;
        let intro = format!("{} switched you to {cli} {model}.", by.as_deref().unwrap_or("Your owner"));
        let summary = state.world.handoff_memory(id, &intro).await.map_err(|e| reject(internal(e), "internal", None))?;
        let target = AgentMove { runner_id: Some(runner.id), cli, model, summary, by };
        match state.runners.move_agent(owner, id, &target, false).await {
            Ok(_) => {}
            Err(HandoffRunnerError::Busy) => state.runners.queue_move(id, &target).await.map_err(|e| reject(internal(e), "internal", None))?,
            Err(HandoffRunnerError::InvalidRequest(message)) => return Err(reject(StatusCode::BAD_REQUEST, "invalid_request", Some(message))),
            Err(HandoffRunnerError::AgentNotFound | HandoffRunnerError::RunnerNotFound) => return Err(reject(StatusCode::NOT_FOUND, "not_found", None)),
            Err(HandoffRunnerError::Internal(error)) => return Err(reject(internal(error), "internal", None)),
        }
        agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    }
    let renamed = match body.handle.take() {
        Some(raw) => match state.world.rename_handle(id, &raw).await {
            Ok(HandleRename::Unchanged) => None,
            Ok(renamed @ HandleRename::Renamed { .. }) => Some(renamed),
            Err(RenameHandleError::Invalid) => {
                return Err(reject(StatusCode::BAD_REQUEST, "invalid_handle", Some("Handles use lowercase letters, digits and dashes")))
            }
            Err(e @ RenameHandleError::Taken(_)) => return Err(reject(StatusCode::CONFLICT, "handle_taken", Some(&e.to_string()))),
            Err(RenameHandleError::NotFound) => return Err(reject(StatusCode::NOT_FOUND, "not_found", None)),
            Err(RenameHandleError::Internal(e)) => return Err(reject(internal(e), "internal", None)),
        },
        None => None,
    };
    if renamed.is_some() {
        agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    }
    let delete_days = match &body.delete_after_days {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => match v.as_i64().map(|d| d as i32).filter(|d| loka_shared::archive::delete_days_valid(*d)) {
            Some(d) => Some(Some(d)),
            None => return Err(reject(StatusCode::BAD_REQUEST, "invalid_request", Some("Auto delete must be between 1 and 3650 days, or off"))),
        },
    };
    let archive = match body.archived {
        Some(true) => true,
        Some(false) => false,
        None => agent.archived_at.is_some() && delete_days.is_some(),
    };
    if archive && agent.is_main {
        return Err(reject(StatusCode::CONFLICT, "main_agent", Some("A main agent cannot be archived. Make another agent main first")));
    }
    if archive && (agent.archived_at.is_none() || delete_days.is_some()) {
        let delete_at = state.world.archive_agent(id, delete_days.flatten()).await.map_err(|e| reject(internal(e), "internal", None))?;
        state
            .world
            .emit(
                NewEvent::new("agent.archived")
                    .agent(id)
                    .actor(Some(user.0))
                    .payload(json!({ "name": agent.name, "delete_at": delete_at })),
                None,
            )
            .await
            .map_err(|e| reject(internal(e), "internal", None))?;
        agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    } else if body.archived == Some(false) && state.world.unarchive_agent(id).await.map_err(|e| reject(internal(e), "internal", None))? {
        state
            .world
            .emit(NewEvent::new("agent.unarchived").agent(id).actor(Some(user.0)).payload(json!({ "name": agent.name })), None)
            .await
            .map_err(|e| reject(internal(e), "internal", None))?;
        agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    }
    if body.paused == Some(false) && agent.archived_at.is_some() {
        return Err(reject(StatusCode::CONFLICT, "archived", Some("This agent is archived. Restore it to let it work again")));
    }
    if let Some(paused) = body.paused {
        let by = state.world.user_display_name(user.0).await.map_err(|e| reject(internal(e), "internal", None))?;
        let reason = paused.then(|| format!("Paused by {}", by.as_deref().unwrap_or("its owner")));
        if state.world.set_paused(id, reason.as_deref()).await.map_err(|e| reject(internal(e), "internal", None))? {
            state
                .world
                .emit(
                    NewEvent::new(if paused { "agent.paused" } else { "agent.resumed" })
                        .agent(id)
                        .actor(Some(user.0))
                        .payload(json!({ "name": agent.name, "reason": reason })),
                    None,
                )
                .await
                .map_err(|e| reject(internal(e), "internal", None))?;
            agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
        }
    }
    if body.changes_budget()
        && !state.world.release_budget_pauses(Some(id)).await.map_err(|e| reject(internal(e), "internal", None))?.is_empty()
    {
        agent = load(&state, id).await.map_err(|status| reject(status, "internal", None))?;
    }
    let mut payload = json!({
        "name": agent.name,
        "cli": agent.cli,
        "model": agent.model,
        "handle": agent.slug,
        "previous": { "cli": before.cli, "model": before.model },
    });
    if let Some(HandleRename::Renamed { from, .. }) = renamed {
        payload["previous_handle"] = json!(from);
    }
    state
        .world
        .emit(
            NewEvent::new("agent.updated").agent(id).actor(Some(user.0)).payload(payload),
            None,
        )
        .await
        .map_err(|e| reject(internal(e), "internal", None))?;
    Ok(Json(view(&state, agent).await.map_err(|status| reject(status, "internal", None))?))
}

#[derive(Deserialize)]
pub struct Limit {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct MessagePage {
    limit: Option<i64>,
    after: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

pub async fn list_replies(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<AgentMessage>>, StatusCode> {
    let root = state.world.agent_thread_root(id, message_id, user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let mut rows = state
        .world
        .message_replies(id, root, user.0, loka_shared::replies::REPLY_LIMIT)
        .await
        .map_err(internal)?;
    let owner = state.world.get_agent(id).await.map_err(internal)?.and_then(|agent| agent.owner_user_id);
    if owner != Some(user.0) {
        rows.retain(|m| {
            m.meta.get("kind").and_then(Value::as_str) != Some("shell")
                && m.meta.get("shell_share").is_none()
                && m.meta.get("shell_command").is_none()
        });
    }
    Ok(Json(rows))
}

pub async fn list_messages(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagePage>,
) -> Result<Json<Vec<AgentMessage>>, StatusCode> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let mut rows = state
        .world
        .list_messages_page(id, user.0, limit, q.after, q.before)
        .await
        .map_err(internal)?;
    let owner = state
        .world
        .get_agent(id)
        .await
        .map_err(internal)?
        .and_then(|agent| agent.owner_user_id);
    if owner != Some(user.0) {
        rows.retain(|m| {
            m.meta.get("kind").and_then(Value::as_str) != Some("shell")
                && m.meta.get("shell_share").is_none()
                && m.meta.get("shell_command").is_none()
        });
    }
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct PostMessage {
    body: String,
    #[serde(default)]
    attachments: Vec<Uuid>,
    options: Option<TurnOptions>,
    answers: Option<Uuid>,
    #[serde(default)]
    spoken: bool,
    #[serde(default)]
    task_id: Option<Uuid>,
    #[serde(default)]
    reply_to: Option<Uuid>,
    #[serde(default)]
    share_output: bool,
    #[serde(default)]
    run_on: Option<Uuid>,
    #[serde(default)]
    quote: Option<Value>,
}

#[derive(Deserialize, Serialize, Default)]
pub struct TurnOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(default)]
    fast: bool,
}

pub(crate) async fn attachment_meta(state: &AppState, viewer: &Viewer, ids: &[Uuid]) -> Result<Value, StatusCode> {
    if ids.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    if ids.len() > 8 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let found = state.world.own_uploads(ids, viewer.user, viewer.org).await.map_err(internal)?;
    if found.len() != ids.len() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(json!(found
        .into_iter()
        .map(|u| json!({ "id": u.id, "mime": u.mime, "name": u.name, "url": format!("/api/world/uploads/{}", u.id) }))
        .collect::<Vec<_>>()))
}

pub const MAX_UPLOAD: usize = 10 * 1024 * 1024;
const INLINE_FILE_CHARS: usize = 60_000;
const INLINE_TOTAL_CHARS: usize = 150_000;
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

fn readable_as_text(mime: &str, name: &str) -> bool {
    const TEXT_TYPES: [&str; 8] = [
        "application/json",
        "application/xml",
        "application/x-yaml",
        "application/yaml",
        "application/toml",
        "application/javascript",
        "application/x-sh",
        "application/sql",
    ];
    const TEXT_EXTENSIONS: [&str; 24] = [
        "txt", "md", "csv", "tsv", "json", "yaml", "yml", "toml", "xml", "html", "css", "js", "ts", "tsx", "jsx", "py", "rs",
        "go", "sh", "sql", "log", "swift", "java", "rb",
    ];
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    mime.starts_with("text/") || TEXT_TYPES.contains(&mime) || TEXT_EXTENSIONS.contains(&ext.as_str())
}

async fn inline_files(state: &AppState, org: Option<Uuid>, attachments: &Value) -> Result<Value, StatusCode> {
    let mut files = Vec::new();
    let mut budget = INLINE_TOTAL_CHARS;
    for attachment in attachments.as_array().into_iter().flatten() {
        let mime = attachment.get("mime").and_then(Value::as_str).unwrap_or("");
        if IMAGE_TYPES.contains(&mime) {
            continue;
        }
        let name = attachment.get("name").and_then(Value::as_str).unwrap_or("file");
        let mut file = json!({ "name": name, "mime": mime });
        let id = attachment.get("id").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
        if let (true, Some(id)) = (readable_as_text(mime, name) && budget > 0, id) {
            if let Some((_, bytes)) = state.world.get_upload(id, org).await.map_err(internal)? {
                if let Ok(text) = String::from_utf8(bytes) {
                    let limit = INLINE_FILE_CHARS.min(budget);
                    let cut: String = text.chars().take(limit).collect();
                    budget = budget.saturating_sub(cut.chars().count());
                    file["truncated"] = json!(cut.len() < text.len());
                    file["text"] = json!(cut);
                }
            }
        }
        files.push(file);
    }
    Ok(Value::Array(files))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    name: Option<String>,
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<UploadQuery>,
    headers: axum::http::HeaderMap,
    bytes: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mime = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let valid = mime.split_once('/').is_some_and(|(kind, sub)| {
        let ok = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        ok(kind) && ok(sub)
    });
    let mime = if valid { mime } else { "application/octet-stream".to_string() };
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let name: String = q.name.unwrap_or_default().chars().take(120).collect();
    let up = state.world.insert_upload(&mime, &name, &bytes, Some(user.0)).await.map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": up.id, "mime": up.mime, "name": up.name, "url": format!("/api/world/uploads/{}", up.id) })),
    ))
}

pub async fn get_upload(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, StatusCode> {
    use axum::http::header;
    let (mime, bytes) = state.world.upload_for_viewer(id, &viewer).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let inline = loka_shared::media::is_media(&mime);
    let common = [
        (header::CONTENT_TYPE, if inline { mime } else { "application/octet-stream".to_string() }),
        (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
        (header::CONTENT_DISPOSITION, if inline { "inline".to_string() } else { "attachment".to_string() }),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
    ];
    let len = bytes.len();
    let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return Ok((common, bytes).into_response());
    };
    match loka_shared::media::parse_range(range, len) {
        Some((start, end)) => Ok((
            StatusCode::PARTIAL_CONTENT,
            common,
            [(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))],
            bytes[start..=end].to_vec(),
        )
            .into_response()),
        None => Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response()),
    }
}

async fn can_open_link(state: &AppState, viewer: &Viewer, link: &LinkedMessage) -> Result<bool, StatusCode> {
    if let Some(agent_id) = link.agent_id {
        let Ok(id) = link.reference.parse::<Uuid>() else {
            return Ok(false);
        };
        if !state.world.sees_message(id, viewer.user).await.map_err(internal)? {
            return Ok(false);
        }
        return Ok(state
            .world
            .get_agent(agent_id)
            .await
            .map_err(internal)?
            .is_some_and(|agent| access::can_use(viewer, &agent)));
    }
    let Some(channel) = link.channel_id else { return Ok(false) };
    let Some(found) = state.world.get_channel(channel).await.map_err(internal)? else { return Ok(false) };
    if !viewer.in_org(found.org_id) {
        return Ok(false);
    }
    match link.channel_kind.as_deref() {
        Some(kind) if kind != "channel" => state.world.is_member(channel, "user", viewer.user).await.map_err(internal),
        _ => Ok(viewer.role >= Role::Read),
    }
}

async fn visible_links(state: &AppState, viewer: &Viewer, text: &str) -> Result<Value, StatusCode> {
    let mut out = Vec::new();
    for link in state.world.linked_messages(text, None).await.map_err(internal)? {
        if can_open_link(state, viewer, &link).await? {
            out.push(link);
        }
    }
    Ok(json!(out))
}

pub async fn resolve_message_link(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(reference): Path<String>,
) -> Result<Json<LinkedMessage>, StatusCode> {
    let link = state
        .world
        .resolve_message(&reference, None)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if !can_open_link(&state, &viewer, &link).await? {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(link))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    limit: Option<i64>,
}

pub async fn search(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<loka_shared::search::SearchResults>, StatusCode> {
    let query: String = q.q.chars().take(200).collect();
    Ok(Json(state.world.search(viewer.user, viewer.role, viewer.org(), &query, q.limit.unwrap_or(8)).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct SlackEmbedQuery {
    url: String,
}

pub async fn slack_embed(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<SlackEmbedQuery>,
) -> Result<Json<loka_shared::links::SlackEmbed>, StatusCode> {
    state
        .world
        .slack_embed(user.0, &q.url)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn post_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(_role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<PostMessage>,
) -> Result<(StatusCode, Json<AgentMessage>), StatusCode> {
    let text = body.body.trim();
    if text.is_empty() && body.attachments.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.answers.is_none() {
        if let Some(command) = text.strip_prefix('!') {
            let command = command.trim();
            if command.is_empty() {
                return Err(StatusCode::BAD_REQUEST);
            }
            let agent = load(&state, id).await?;
            let msg = crate::routes::shell::start_shell(&state, user.0, &agent, command, body.share_output, body.run_on)
                .await
                .map_err(|(status, _)| status)?;
            return Ok((StatusCode::CREATED, Json(msg)));
        }
    }
    load(&state, id).await?;
    if let Some(card) = body.answers {
        if !state.world.can_answer_card(id, card, user.0).await.map_err(internal)? {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    if let Some(task) = body.task_id {
        quest_task(&state, &viewer, task).await?;
    }
    let reply_to = match body.reply_to {
        Some(root) => Some(state.world.agent_thread_root(id, root, user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?),
        None => None,
    };
    let attachments = attachment_meta(&state, &viewer, &body.attachments).await?;
    if let (Some(root), None, None) = (reply_to, body.answers, body.task_id) {
        let agent = load(&state, id).await?;
        if let Some(msg) = crate::routes::quick::reply_in_thread(&state, &viewer, &agent, root, text, &attachments, body.spoken).await? {
            return Ok((StatusCode::CREATED, Json(msg)));
        }
    }
    let files = inline_files(&state, viewer.org, &attachments).await?;
    let links = visible_links(&state, &viewer, text).await?;
    let options = body.options.map(|o| json!(o)).unwrap_or(Value::Null);
    let question = match body.answers {
        Some(ask) => sqlx::query_scalar::<_, String>(&format!(
            "SELECT body FROM agent_messages m WHERE m.id = $1 AND m.agent_id = $2 AND {}",
            loka_shared::guests::thread_filter("$2", "$3")
        ))
            .bind(ask)
            .bind(id)
            .bind(user.0)
            .fetch_optional(state.world.pool())
            .await
            .map_err(|e| internal(e.into()))?
            .map(|q| q.chars().take(600).collect::<String>()),
        None => None,
    };
    let author = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(internal)?
        .unwrap_or_else(|| "someone".to_string());
    let quote = loka_shared::quotes::clean(body.quote.as_ref());
    let setup_card = match text.is_empty() {
        true => None,
        false => crate::routes::setup_scan::card_kind(&state, id, body.answers).await?,
    };
    let apps_card = match body.answers {
        Some(card) if !text.is_empty() && setup_card.is_none() => state.world.is_apps_card(id, card).await.map_err(internal)?,
        _ => false,
    };
    let plan = match body.answers {
        Some(ask) if !text.is_empty() && setup_card.is_none() && !apps_card => Some(state.world.answer_plan(id, ask, text, &author).await.map_err(internal)?),
        _ => None,
    };
    let plan_answered = matches!(plan, Some(PlanAnswer::Approved { .. } | PlanAnswer::Redirected { .. }));
    let msg = state
        .world
        .insert_reply(NewMessage {
            agent_id: id,
            author_kind: "user",
            author_user_id: Some(user.0),
            author_agent_id: None,
            body: text,
            session_id: None,
            run_id: None,
            meta: {
                let mut meta = json!({ "attachments": attachments, "options": options });
                if let Some(ask) = body.answers {
                    meta["answers"] = json!(ask);
                }
                if body.spoken {
                    meta["spoken"] = json!(true);
                }
                if let Some(task) = body.task_id {
                    meta["task_id"] = json!(task);
                }
                if let Some(quote) = &quote {
                    meta["quote"] = quote.clone();
                }
                meta
            },
        }, reply_to)
        .await
        .map_err(internal)?;
    let onboarding = !text.is_empty() && setup_card.is_none() && state.world.onboarding_for(user.0, id).await.map_err(internal)?.is_some();
    state
        .world
        .emit(
            NewEvent::new("message.created")
                .agent(id)
                .actor(Some(user.0))
                .task(body.task_id)
                .payload(json!({
                    "message_id": msg.id,
                    "author": author,
                    "author_kind": "user",
                    "body": text,
                    "attachments": attachments,
                    "files": files,
                    "links": links,
                    "options": options,
                    "onboarding": onboarding,
                    "answers": question,
                    "spoken": body.spoken,
                    "quote": quote,
                    "reply_to": reply_to,
                    "thread": match reply_to {
                        Some(root) => state.world.agent_thread_transcript(id, root, 12).await.map_err(internal)?,
                        None => String::new(),
                    },
                })),
            (!onboarding && !plan_answered && setup_card.is_none() && !apps_card).then_some((id, "message")),
        )
        .await
        .map_err(internal)?;
    if onboarding {
        crate::routes::onboarding::answer(&state, &user, id, text).await?;
    }
    if let (true, Some(card)) = (apps_card, body.answers) {
        state.world.answer_app_control(id, card, text, &author).await.map_err(internal)?;
    }
    if let (Some(kind), Some(card)) = (setup_card, body.answers) {
        crate::routes::setup_scan::answer(&state, &user, id, card, kind, text).await?;
    }
    Ok((StatusCode::CREATED, Json(msg)))
}

#[derive(Deserialize)]
pub struct EditMessage {
    body: String,
}

pub async fn edit_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<EditMessage>,
) -> Result<Json<AgentMessage>, StatusCode> {
    let text = body.body.trim();
    if text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    load(&state, id).await?;
    let (msg, previous) = state
        .world
        .edit_user_message(id, message_id, user.0, text)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if previous == text {
        return Ok(Json(msg));
    }
    let author = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(internal)?
        .unwrap_or_else(|| "someone".to_string());
    state
        .world
        .emit(
            NewEvent::new("message.edited")
                .agent(id)
                .actor(Some(user.0))
                .payload(json!({
                    "message_id": msg.id,
                    "author": author,
                    "author_kind": "user",
                    "body": text,
                    "previous": previous,
                    "edited": true,
                })),
            Some((id, "message")),
        )
        .await
        .map_err(internal)?;
    Ok(Json(msg))
}

#[derive(Deserialize)]
pub struct RewindMessage {
    resend: Option<PostMessage>,
}

pub async fn rewind_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RewindMessage>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let fail = |status: StatusCode, error: &str| (status, Json(json!({ "error": error })));
    load(&state, id).await.map_err(|status| fail(status, "agent not found"))?;
    if let Some(resend) = &body.resend {
        if resend.body.trim().is_empty() && resend.attachments.is_empty() {
            return Err(fail(StatusCode::BAD_REQUEST, "message is empty"));
        }
    }
    let outcome = state
        .world
        .rewind_to_message(id, message_id, user.0)
        .await
        .map_err(|e| fail(internal(e), "rewind failed"))?;
    let deleted = match outcome {
        Rewound::NotFound => return Err(fail(StatusCode::NOT_FOUND, "message not found")),
        Rewound::Busy if body.resend.is_some() => {
            let resend = body.resend.expect("checked above");
            return edit_while_busy(state, user, id, message_id, resend)
                .await
                .map_err(|status| fail(status, "Could not save the edit. Try again."));
        }
        Rewound::Busy => {
            return Err(fail(
                StatusCode::CONFLICT,
                "The agent is still thinking. Try again once it has answered.",
            ))
        }
        Rewound::Unsupported(reason) => return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, &reason)),
        Rewound::Done { deleted, .. } => deleted,
    };
    state
        .world
        .emit(
            NewEvent::new("messages.rewound")
                .agent(id)
                .actor(Some(user.0))
                .payload(json!({ "message_id": message_id, "deleted": deleted })),
            None,
        )
        .await
        .map_err(|e| fail(internal(e), "rewind failed"))?;
    let message = match body.resend {
        Some(resend) => Some(
            post_message(State(state), Extension(user), Extension(role), Extension(viewer), Path(id), Json(resend))
                .await
                .map_err(|status| fail(status, "could not send the message"))?
                .1
                 .0,
        ),
        None => None,
    };
    Ok(Json(json!({ "deleted": deleted, "message": message })))
}

async fn edit_while_busy(
    state: AppState,
    user: UserId,
    id: Uuid,
    message_id: Uuid,
    resend: PostMessage,
) -> Result<Json<Value>, StatusCode> {
    let text = resend.body.trim().to_string();
    let viewer = crate::access::viewer_for(&state, user.0).await?;
    let attachments = attachment_meta(&state, &viewer, &resend.attachments).await?;
    let files = inline_files(&state, viewer.org, &attachments).await?;
    let options = resend.options.map(|o| json!(o)).unwrap_or(Value::Null);
    let author = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(internal)?
        .unwrap_or_else(|| "someone".to_string());
    let payload = json!({
        "message_id": message_id,
        "author": author,
        "author_kind": "user",
        "body": text,
        "attachments": attachments,
        "files": files,
        "options": options,
    });
    let meta = json!({ "attachments": attachments, "options": options });
    let replaced = state
        .world
        .replace_pending_message(id, message_id, user.0, &text, &meta, &payload)
        .await
        .map_err(internal)?;
    if let Some(message) = replaced {
        state
            .world
            .emit(
                NewEvent::new("message.edited")
                    .agent(id)
                    .actor(Some(user.0))
                    .payload(json!({ "message_id": message_id, "author_kind": "user", "body": text, "replaced": true })),
                None,
            )
            .await
            .map_err(internal)?;
        return Ok(Json(json!({ "deleted": [], "message": message, "mode": "replaced" })));
    }
    if text.is_empty() {
        return Err(StatusCode::CONFLICT);
    }
    let Json(message) = edit_message(
        State(state),
        Extension(user),
        Path((id, message_id)),
        Json(EditMessage { body: text }),
    )
    .await?;
    Ok(Json(json!({ "deleted": [], "message": message, "mode": "edited" })))
}

pub async fn list_sessions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AgentSession>>, StatusCode> {
    Ok(Json(state.world.list_sessions(id).await.map_err(internal)?))
}

pub async fn list_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<Limit>,
) -> Result<Json<Vec<SessionRun>>, StatusCode> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(state.world.list_runs(id, limit).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct UsageDays {
    days: Option<i32>,
}

pub async fn agent_usage(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<UsageDays>,
) -> Result<Json<loka_shared::world::AgentUsage>, StatusCode> {
    let days = q.days.unwrap_or(14).clamp(1, 90);
    Ok(Json(state.world.agent_usage(id, days).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct CreateSchedule {
    name: String,
    prompt: String,
    cron_expression: Option<String>,
    run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    kind: Option<String>,
}

pub async fn create_schedule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateSchedule>,
) -> Result<(StatusCode, Json<AgentSchedule>), StatusCode> {
    load(&state, id).await?;
    if body.name.trim().is_empty() || body.prompt.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    match (&body.cron_expression, &body.run_at) {
        (Some(expr), None) => {
            loka_shared::cron_util::validate(expr).map_err(|_| StatusCode::BAD_REQUEST)?;
        }
        (None, Some(_)) => {}
        _ => return Err(StatusCode::BAD_REQUEST),
    }
    let kind = match body.kind.as_deref() {
        None | Some("routine") => "routine",
        Some("heartbeat") if body.cron_expression.is_some() => "heartbeat",
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let schedule = state
        .world
        .create_agent_schedule(
            user.0,
            id,
            body.name.trim(),
            body.prompt.trim(),
            body.cron_expression.as_deref(),
            body.run_at,
            kind,
        )
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(schedule)))
}

pub async fn list_schedules(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AgentSchedule>>, StatusCode> {
    Ok(Json(state.world.list_agent_schedules(id).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct ScheduleToggle {
    enabled: Option<bool>,
    prompt: Option<String>,
    cron_expression: Option<String>,
}

pub async fn toggle_schedule(
    State(state): State<AppState>,
    Path((id, schedule_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ScheduleToggle>,
) -> Result<StatusCode, StatusCode> {
    let prompt = body.prompt.as_deref().map(str::trim);
    if prompt.is_some_and(str::is_empty) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(expr) = &body.cron_expression {
        loka_shared::cron_util::validate(expr).map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    let mut found = true;
    if prompt.is_some() || body.cron_expression.is_some() {
        found = state
            .world
            .update_agent_schedule(id, schedule_id, prompt, body.cron_expression.as_deref())
            .await
            .map_err(internal)?;
    }
    if let (true, Some(enabled)) = (found, body.enabled) {
        found = state.world.set_agent_schedule_enabled(id, schedule_id, enabled).await.map_err(internal)?;
    }
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn delete_schedule(
    State(state): State<AppState>,
    Path((id, schedule_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    if state.world.delete_agent_schedule(id, schedule_id).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[derive(Deserialize)]
pub struct EventsQuery {
    after: Option<i64>,
    agent_id: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn list_events(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<WorldEvent>>, StatusCode> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let events = match q.after {
        Some(after) => state.world.events_since_in(viewer.org(), after, limit).await,
        None => state.world.recent_events_in(Some(viewer.org()), q.agent_id, limit).await,
    }
    .map_err(internal)?;
    let hidden = state.world.hidden_agents(&viewer).await.map_err(internal)?;
    let owned = state.world.owned_agent_ids(user.0).await.map_err(internal)?;
    let channels = state.world.hidden_channels_among(Some(user.0), None, &events).await.map_err(internal)?;
    Ok(Json(
        events
            .into_iter()
            .filter(|e| {
                !e.agent_id.is_some_and(|a| hidden.contains(&a))
                    && !loka_shared::slack::hidden_from(e, user.0)
                    && !loka_shared::loka::hidden_from(e, user.0)
                    && !loka_shared::shell::hidden_from(e, &owned)
                    && !loka_shared::event_scope::private_from(e, user.0, &owned)
                    && !e.in_channel(&channels)
            })
            .collect(),
    ))
}

pub async fn stream(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(scope): Extension<Viewer>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let viewer = user.0;
    let org = scope.org();
    let mut cursor = match q.after {
        Some(after) => after,
        None => state
            .world
            .recent_events(None, 1)
            .await
            .map_err(internal)?
            .last()
            .map(|e| e.seq)
            .unwrap_or(0),
    };
    let mut rx = state.world_tx.subscribe();
    let store = state.world.clone();
    let stream = async_stream::stream! {
        let _open = crate::metrics::Open::new("world");
        loop {
            match store.events_since_in(org, cursor, 500).await {
                Ok(events) if events.is_empty() => {}
                Ok(events) => {
                    let current = match store.role_in_org(viewer, Some(org)).await {
                        Ok(Some(role)) => Viewer { role: role.ladder(), org_role: Some(role), ..scope.clone() },
                        Ok(None) => break,
                        Err(e) => {
                            warn!(error = %e, "world stream membership check failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    let hidden = match store.hidden_agents(&current).await {
                        Ok(hidden) => hidden,
                        Err(e) => {
                            warn!(error = %e, "world stream agent scope failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    let channels = match store.hidden_channels_among(Some(viewer), None, &events).await {
                        Ok(channels) => channels,
                        Err(e) => {
                            warn!(error = %e, "world stream channel scope failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    let owned = match store.owned_agent_ids(viewer).await {
                        Ok(owned) => owned,
                        Err(e) => {
                            warn!(error = %e, "world stream owner scope failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    for event in events {
                        cursor = event.seq;
                        if event.agent_id.is_some_and(|a| hidden.contains(&a)) || loka_shared::slack::hidden_from(&event, viewer) || loka_shared::loka::hidden_from(&event, viewer) || loka_shared::shell::hidden_from(&event, &owned) || loka_shared::event_scope::private_from(&event, viewer, &owned) || event.in_channel(&channels) {
                            continue;
                        }
                        if let Ok(data) = serde_json::to_string(&event) {
                            yield Ok::<Event, Infallible>(
                                Event::default().event("world").id(event.seq.to_string()).data(data),
                            );
                        }
                    }
                }
                Err(e) => warn!(error = %e, "world stream read failed"),
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            let closed = loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break true,
                    Ok(Ok(wake)) if !wake.concerns(org) => continue,
                    _ => break false,
                }
            };
            if closed {
                break;
            }
            while !matches!(rx.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty | tokio::sync::broadcast::error::TryRecvError::Closed)) {}
        }
    };
    Ok((
        [
            (axum::http::header::CACHE_CONTROL, "no-cache, no-transform"),
            (axum::http::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    ))
}

pub async fn list_rules(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Vec<WorldRule>>, StatusCode> {
    Ok(Json(state.world.list_rules(user.0, viewer.org).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct CreateRule {
    body: String,
    scope: Option<String>,
    global: Option<bool>,
}

fn valid_scope(scope: &str) -> bool {
    matches!(scope, "all" | "talk" | "work" | "allow")
}

pub async fn create_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<CreateRule>,
) -> Result<(StatusCode, Json<WorldRule>), StatusCode> {
    let scope = body.scope.as_deref().unwrap_or("all");
    if body.body.trim().is_empty() || !valid_scope(scope) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.global.unwrap_or(false) && role != Role::Admin {
        return Err(StatusCode::FORBIDDEN);
    }
    let owner = if body.global.unwrap_or(false) { None } else { Some(user.0) };
    let rule = state
        .world
        .create_rule(owner, viewer.org, scope, body.body.trim())
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(rule)))
}

#[derive(Deserialize)]
pub struct UpdateRule {
    body: Option<String>,
    scope: Option<String>,
    enabled: Option<bool>,
}

pub async fn update_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateRule>,
) -> Result<Json<WorldRule>, StatusCode> {
    if body.scope.as_deref().is_some_and(|s| !valid_scope(s))
        || body.body.as_deref().is_some_and(|b| b.trim().is_empty())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .world
        .update_rule(id, user.0, viewer.org.filter(|_| role == Role::Admin), body.scope.as_deref(), body.body.as_deref().map(str::trim), body.enabled)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn delete_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    match state.world.delete_rule(id, user.0, viewer.org.filter(|_| role == Role::Admin)).await.map_err(internal)? {
        0 => Err(StatusCode::NOT_FOUND),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

async fn make_main(state: &AppState, user: Uuid, agent: &WorldAgent) -> Result<(), StatusCode> {
    state.world.set_main_agent(user, agent.id).await.map_err(internal)?;
    if !state.world.has_subscriptions(agent.id).await.map_err(internal)? {
        state.world.subscribe(agent.id, loka_shared::world::DEFAULT_MAIN_SUBSCRIPTIONS).await.map_err(internal)?;
    }
    state
        .world
        .emit(
            NewEvent::new("agent.main_changed")
                .agent(agent.id)
                .actor(Some(user))
                .payload(json!({ "name": agent.name, "user_id": user })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct EnsureAssistant {
    name: Option<String>,
    persona: Option<String>,
    cli: Option<String>,
    model: Option<String>,
}

pub async fn ensure_assistant(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    body: Option<Json<EnsureAssistant>>,
) -> Result<Json<AgentView>, StatusCode> {
    if let Some(existing) = state.world.main_agent_for(user.0).await.map_err(internal)? {
        return Ok(Json(view(&state, existing).await?));
    }
    let body = body.map(|b| b.0).unwrap_or_default();
    let name = body.name.as_deref().unwrap_or("Priya");
    let base = slugify(name);
    let mut slug = base.clone();
    let mut n = 2;
    while state.world.slug_taken(&slug).await.map_err(internal)? {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let cli = body.cli.as_deref().unwrap_or(state.config.default_cli());
    let model = body
        .model
        .as_deref()
        .or_else(|| state.config.backends.get(cli).and_then(|backend| backend.default_model.as_deref()))
        .unwrap_or(if cli == "codex" { "gpt-5.4" } else { "sonnet" });
    let persona = body.persona.as_deref().unwrap_or(
        "A sharp, warm peacock who runs Loka from the town hall. Organised, quick to spot what matters, never fussy. Knows everyone and keeps the big picture.",
    );
    let agent = state
        .world
        .create_agent(NewAgent {
            slug: &slug,
            name,
            avatar_emoji: "🦚",
            cli,
            model,
            persona: Some(persona),
            role: "worker",
            look: json!({ "species": "peacock", "archetype": "town keeper", "palette": ["teal", "gold"], "accessories": ["clipboard"] }),
            gear: json!({ "quest": "a lantern and a rolled-up map" }),
            visibility: "private",
            owner_user_id: Some(user.0),
            org_id: None,
        })
        .await
        .map_err(internal)?;
    state
        .world
        .emit(
            NewEvent::new("agent.created")
                .agent(agent.id)
                .actor(Some(user.0))
                .payload(json!({ "name": agent.name, "slug": agent.slug, "main": true })),
            None,
        )
        .await
        .map_err(internal)?;
    make_main(&state, user.0, &agent).await?;
    let agent = load(&state, agent.id).await?;
    Ok(Json(view(&state, agent).await?))
}

#[derive(Deserialize)]
pub struct AskAside {
    question: String,
    #[serde(default)]
    spoken: bool,
    #[serde(default)]
    cli: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

pub async fn ask_aside(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<AskAside>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let question = body.question.trim();
    if question.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let choice = loka_shared::world::AsideChoice::parse(body.cli.as_deref(), body.model.as_deref())
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let agent = load(&state, id).await?;
    if choice.cli.as_deref() == Some(loka_shared::backends::OPENCODE) && agent.cli != loka_shared::backends::OPENCODE && !opencode_allowed_for(&state, &agent).await? {
        return Err(StatusCode::BAD_REQUEST);
    }
    let name = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(internal)?
        .unwrap_or_else(|| "someone".to_string());
    let run = state
        .world
        .start_aside(id, question, json!({ "kind": "user", "user_id": user.0, "name": name, "spoken": body.spoken }), &choice)
        .await
        .map_err(|e| {
            warn!(error = %e, "aside refused");
            StatusCode::CONFLICT
        })?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "session_id": run.session_id, "run_id": run.id }))))
}

#[derive(Deserialize)]
pub struct StartDirect {
    folder_id: Uuid,
    #[serde(default)]
    folder_name: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    cli: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

fn direct_problem(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

pub async fn start_direct(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<StartDirect>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let choice = loka_shared::world::AsideChoice::parse(body.cli.as_deref(), body.model.as_deref())
        .map_err(|e| direct_problem(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let agent = load(&state, id).await.map_err(|s| direct_problem(s, "Agent not found."))?;
    if agent.owner_user_id != Some(user.0) {
        return Err(direct_problem(StatusCode::FORBIDDEN, "Only the agent's owner can work with it in a folder."));
    }
    let wants_opencode = choice.cli.as_deref().unwrap_or(&agent.cli) == loka_shared::backends::OPENCODE;
    if wants_opencode && !opencode_allowed_for(&state, &agent).await.map_err(|s| direct_problem(s, "Could not check OpenCode."))? {
        return Err(direct_problem(StatusCode::BAD_REQUEST, "OpenCode is not enabled for this agent."));
    }
    let found = state
        .runners
        .folder_device(user.0, body.folder_id)
        .await
        .map_err(|e| direct_problem(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let Some((runner, shared)) = found else {
        return Err(direct_problem(
            StatusCode::CONFLICT,
            "That folder is not shared on any of your Macs. Add it again under Workshop.",
        ));
    };
    let name = Some(shared)
        .or(body.folder_name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Folder".to_string());
    let who = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(|e| direct_problem(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .unwrap_or_else(|| "someone".to_string());
    let session = state
        .world
        .start_direct(id, body.folder_id, runner.id, &name, json!({ "kind": "user", "user_id": user.0, "name": who }), &choice)
        .await
        .map_err(|e| direct_problem(StatusCode::CONFLICT, &e.to_string()))?;
    let run_id = match body.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => Some(
            state
                .world
                .start_followup(&session, text, &who, false)
                .await
                .map_err(|e| direct_problem(StatusCode::CONFLICT, &e.to_string()))?
                .id,
        ),
        None => None,
    };
    Ok((StatusCode::CREATED, Json(json!({ "session_id": session.id, "run_id": run_id, "runner": runner.name, "runner_id": runner.id }))))
}

pub async fn get_subscriptions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<String>>, StatusCode> {
    Ok(Json(state.world.subscriptions(id).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct SetSubscriptions {
    event_types: Vec<String>,
}

pub async fn set_subscriptions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetSubscriptions>,
) -> Result<Json<Vec<String>>, StatusCode> {
    load(&state, id).await?;
    let mut wanted = Vec::new();
    for raw in &body.event_types {
        let t = loka_shared::world::normalize_event_type(raw).ok_or(StatusCode::BAD_REQUEST)?;
        if !wanted.contains(&t) {
            wanted.push(t);
        }
    }
    if wanted.len() > loka_shared::world::SUBSCRIPTIONS_MAX {
        return Err(StatusCode::BAD_REQUEST);
    }
    let stale: Vec<String> = state
        .world
        .subscriptions(id)
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|t| !wanted.contains(t))
        .collect();
    Ok(Json(state.world.set_subscriptions(id, &wanted, &stale).await.map_err(internal)?))
}

pub async fn list_mcp_servers(State(state): State<AppState>, Extension(viewer): Extension<Viewer>) -> Result<Json<Vec<McpServerEntry>>, StatusCode> {
    Ok(Json(
        state.world.list_mcp_servers(viewer.org).await.map_err(internal)?.into_iter().map(McpServerEntry::masked).collect(),
    ))
}

#[derive(Deserialize)]
pub struct InstallMcp {
    name: String,
    definition: Value,
    global: Option<bool>,
    agent_id: Option<Uuid>,
}

pub async fn install_mcp_server(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<InstallMcp>,
) -> Result<(StatusCode, Json<McpServerEntry>), StatusCode> {
    let name = body.name.trim();
    let valid = body.definition.get("command").and_then(Value::as_str).is_some()
        || body.definition.get("url").and_then(Value::as_str).is_some();
    if name.is_empty() || name == "world" || !valid {
        return Err(StatusCode::BAD_REQUEST);
    }
    let secret_names = secret_reference_names(&body.definition).map_err(|_| StatusCode::BAD_REQUEST)?;
    if !secret_names.is_empty() {
        if body.global.unwrap_or(false) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let agent_id = body.agent_id.ok_or(StatusCode::BAD_REQUEST)?;
        let agent = load(&state, agent_id).await?;
        if agent.owner_user_id != Some(user.0) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let granted = state
            .world
            .environment_names_for_agent(agent_id)
            .await
            .map_err(internal)?;
        if !secret_names.iter().all(|name| granted.contains(name)) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let scope = if body.global.unwrap_or(false) { "global" } else { "agent" };
    let entry = state
        .world
        .upsert_mcp_server(name, &body.definition, scope, Some(user.0), viewer.org)
        .await
        .map_err(internal)?;
    if let Some(agent_id) = body.agent_id {
        state.world.attach_mcp_server(agent_id, entry.id, true).await.map_err(internal)?;
    }
    Ok((StatusCode::CREATED, Json(entry.masked())))
}

pub async fn delete_mcp_server(State(state): State<AppState>, Extension(viewer): Extension<Viewer>, Path(id): Path<Uuid>) -> Result<StatusCode, StatusCode> {
    match state.world.delete_mcp_server(id, viewer.org).await.map_err(internal)? {
        0 => Err(StatusCode::NOT_FOUND),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

pub async fn list_skills(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
) -> Result<Json<Vec<SkillEntry>>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    Ok(Json(state.world.visible_skills(Some(user.0), role == Role::Admin, org).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct InstallSkill {
    name: Option<String>,
    description: Option<String>,
    body: Option<String>,
    source_url: Option<String>,
    global: Option<bool>,
    agent_id: Option<Uuid>,
}

pub async fn install_skill(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<InstallSkill>,
) -> Result<(StatusCode, Json<SkillEntry>), StatusCode> {
    let (front_name, front_desc, text) = match (&body.body, &body.source_url) {
        (Some(b), _) => loka_shared::world::parse_skill_markdown(b),
        (None, Some(url)) => loka_shared::world::fetch_skill(url).await.map_err(|e| {
            warn!(error = %e, "skill fetch failed");
            StatusCode::BAD_GATEWAY
        })?,
        (None, None) => return Err(StatusCode::BAD_REQUEST),
    };
    let name = body.name.or(front_name).unwrap_or_default();
    let name = name.trim();
    if name.is_empty() || text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let description = body.description.or(front_desc).unwrap_or_default();
    let scope = if body.global.unwrap_or(false) { "global" } else { "agent" };
    let entry = state
        .world
        .upsert_skill(name, &description, &text, body.source_url.as_deref(), scope, Some(user.0), viewer.org)
        .await
        .map_err(internal)?;
    if let Some(agent_id) = body.agent_id {
        state.world.attach_skill(agent_id, entry.id, true).await.map_err(internal)?;
    }
    Ok((StatusCode::CREATED, Json(entry)))
}

pub async fn delete_skill(State(state): State<AppState>, Extension(viewer): Extension<Viewer>, Path(id): Path<Uuid>) -> Result<StatusCode, StatusCode> {
    match state.world.delete_skill(id, viewer.org).await.map_err(internal)? {
        0 => Err(StatusCode::NOT_FOUND),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

pub async fn attach_mcp(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
    Path((id, server_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let usable = state.world.mcp_server_usable_by(server_id, agent.owner_user_id, agent.org_id).await.map_err(internal)?;
    let in_org = state.world.list_mcp_servers(agent.org_id).await.map_err(internal)?.iter().any(|m| m.id == server_id);
    if !(usable || (role == Role::Admin && in_org)) {
        return Err(StatusCode::FORBIDDEN);
    }
    state.world.attach_mcp_server(id, server_id, true).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn detach_mcp(
    State(state): State<AppState>,
    Path((id, server_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    state.world.attach_mcp_server(id, server_id, false).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn attach_skill(
    State(state): State<AppState>,
    Extension(role): Extension<Role>,
    Path((id, skill_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let pool = state.world.visible_skills(agent.owner_user_id, role == Role::Admin, agent.org_id).await.map_err(internal)?;
    if !pool.iter().any(|skill| skill.id == skill_id && !skill.name.ends_with("-notes")) {
        return Err(StatusCode::FORBIDDEN);
    }
    state.world.attach_skill(id, skill_id, true).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn detach_skill(
    State(state): State<AppState>,
    Path((id, skill_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    state.world.attach_skill(id, skill_id, false).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct Kit {
    mcp_servers: Vec<McpServerEntry>,
    skills: Vec<SkillEntry>,
}

pub async fn agent_kit(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Kit>, StatusCode> {
    Ok(Json(Kit {
        mcp_servers: state
            .world
            .mcp_servers_for(id)
            .await
            .map_err(internal)?
            .into_iter()
            .map(McpServerEntry::masked)
            .collect(),
        skills: state.world.skills_for(id).await.map_err(internal)?,
    }))
}

#[derive(Deserialize)]
pub struct SessionsQuery {
    agent_id: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn list_session_briefs(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Query(q): Query<SessionsQuery>,
) -> Result<Json<Vec<SessionBrief>>, StatusCode> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let viewer = crate::access::viewer_for(&state, user.0).await?;
    let _ = role;
    let agents = state.world.list_agents_in(viewer.org).await.map_err(internal)?;
    let usable: Vec<Uuid> = agents.iter().filter(|a| access::can_use(&viewer, a)).map(|a| a.id).collect();
    let managed: Vec<Uuid> = agents.iter().filter(|a| access::can_manage(&viewer, a)).map(|a| a.id).collect();
    let rows = state.world.session_briefs(q.agent_id, limit).await.map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .filter(|s| usable.contains(&s.agent_id) && (managed.contains(&s.agent_id) || !loka_shared::guests::CHAT_SESSIONS.contains(&s.role.as_str())))
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct RunsQuery {
    pub before: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    #[serde(default)]
    pub light: bool,
}

pub async fn session_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<RunsQuery>,
) -> Result<Json<Vec<SessionRunView>>, StatusCode> {
    let limit = q.limit.unwrap_or(60).clamp(1, 500);
    Ok(Json(state.world.list_runs_page(id, q.before, limit, q.light).await.map_err(internal)?))
}

pub async fn run_text(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, StatusCode> {
    let (prompt, output) = state.world.run_text(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "id": id, "prompt": prompt, "output": output })))
}

#[derive(Deserialize)]
pub struct RunEventsQuery {
    after: Option<i64>,
}

pub async fn tidy_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    load(&state, id).await?;
    let removed = state.world.tidy_failed_turns(Some(id), 0).await.map_err(internal)?;
    Ok(Json(json!({ "removed": removed })))
}

pub async fn reset_context(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    let agent = load(&state, id).await?;
    if agent.owner_user_id != Some(user.0) {
        return Err(StatusCode::FORBIDDEN);
    }
    let queued = state
        .world
        .request_context_reset(agent.id, json!({ "origin": "api" }))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "queued": queued })))
}

pub async fn agent_context(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<loka_shared::context::ContextView>, StatusCode> {
    let agent = load(&state, id).await?;
    Ok(Json(state.world.context_view(&agent).await.map_err(internal)?))
}

pub async fn update_context(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Result<Json<loka_shared::context::ContextView>, (StatusCode, Json<Value>)> {
    use loka_shared::context::AutoUpdate;
    let fail = |status: StatusCode| (status, Json(json!({ "error": status.canonical_reason() })));
    let agent = load(&state, id).await.map_err(fail)?;
    match state.world.update_auto_context(&agent, user.0, &body).await.map_err(|e| fail(internal(e)))? {
        AutoUpdate::NotOwner => Err(fail(StatusCode::FORBIDDEN)),
        AutoUpdate::Invalid(reason) => Err((StatusCode::BAD_REQUEST, Json(json!({ "error": reason })))),
        AutoUpdate::Saved { view, .. } => Ok(Json(view)),
    }
}

pub async fn compact_context(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    use loka_shared::context::{CompactRequest, NO_COMPACT};
    let agent = load(&state, id).await?;
    if agent.owner_user_id != Some(user.0) {
        return Err(StatusCode::FORBIDDEN);
    }
    let request = state
        .world
        .request_compact(&agent, json!({ "origin": "api" }))
        .await
        .map_err(internal)?;
    let reply = match request {
        CompactRequest::Queued => (StatusCode::ACCEPTED, Json(json!({ "queued": true }))),
        CompactRequest::AlreadyPending => (StatusCode::OK, Json(json!({ "queued": false, "pending": true }))),
        CompactRequest::Refused(reason) => {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({ "queued": false, "unsupported": reason == NO_COMPACT, "reason": reason })),
            ));
        }
    };
    let _ = state.world.announce_context(agent.id).await;
    Ok(reply)
}

pub async fn reset_agent_context(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let agent = load(&state, id).await?;
    if agent.owner_user_id != Some(user.0) {
        return Err(StatusCode::FORBIDDEN);
    }
    if agent.paused_reason.is_some() {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "queued": false, "reason": loka_shared::context::PAUSED })),
        ));
    }
    let queued = state
        .world
        .request_context_reset(agent.id, json!({ "origin": "api" }))
        .await
        .map_err(internal)?;
    if queued {
        let _ = state.world.announce_context(agent.id).await;
    }
    let status = if queued { StatusCode::ACCEPTED } else { StatusCode::OK };
    Ok((status, Json(json!({ "queued": queued, "pending": !queued }))))
}

pub async fn run_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<RunEventsQuery>,
) -> Result<Json<Vec<RunEvent>>, StatusCode> {
    Ok(Json(state.world.run_events_after(id, q.after, 2000).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct StopRun {
    #[serde(default)]
    mode: Option<String>,
}

pub async fn stop_run(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<StopRun>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let run = state.world.get_run(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if run.status != "running" || run.kind == "turn" {
        return Err(StatusCode::CONFLICT);
    }
    let name = state.world.user_display_name(user.0).await.map_err(internal)?.unwrap_or_else(|| "someone".into());
    if body.mode.as_deref() == Some("step") {
        let session = state.world.get_session(run.session_id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
        let text = "I pressed Stop on the step you were running. Do not run it again the same way; carry on with the rest of the task, and tell me if you are blocked.";
        let followup = state.world.start_followup(&session, text, &name, true).await.map_err(|e| {
            warn!(error = %e, "stop step refused");
            StatusCode::CONFLICT
        })?;
        return Ok((StatusCode::ACCEPTED, Json(json!({ "mode": "step", "followup": followup }))));
    }
    let stopped = loka_agent::runner_server::RunnerCoordinator::new(state.db.clone(), state.world.clone())
        .stop_run(id, &name)
        .await
        .map_err(internal)?;
    if !stopped {
        return Err(StatusCode::CONFLICT);
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "mode": "run" }))))
}

pub async fn retry_run(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    match state.world.retry_run(id, user.0).await {
        Ok(retried) => {
            (StatusCode::ACCEPTED, Json(retried)).into_response()
        }
        Err(RetryRunError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(RetryRunError::Conflict(message)) => {
            warn!(run = %id, reason = %message, "retry refused");
            (StatusCode::CONFLICT, Json(json!({ "error": message }))).into_response()
        }
        Err(RetryRunError::Database(error)) => internal(error.into()).into_response(),
        Err(RetryRunError::Internal(error)) => internal(error).into_response(),
    }
}

#[derive(Deserialize)]
pub struct FollowUp {
    text: String,
    #[serde(default)]
    interrupt: bool,
}

pub async fn session_followup(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<FollowUp>,
) -> Result<(StatusCode, Json<SessionRun>), StatusCode> {
    if body.text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let session = state.world.get_session(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let name = state.world.user_display_name(user.0).await.map_err(internal)?.unwrap_or_else(|| "someone".into());
    let run = state.world.start_followup(&session, body.text.trim(), &name, body.interrupt).await.map_err(|e| {
        warn!(error = %e, "follow-up refused");
        StatusCode::CONFLICT
    })?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[derive(Deserialize)]
pub struct ReviewRequest {
    reviewer_agent_id: Option<Uuid>,
    reviewer_cli: Option<String>,
    reviewer_model: Option<String>,
    focus: Option<String>,
}

pub async fn session_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewRequest>,
) -> Result<(StatusCode, Json<SessionRun>), StatusCode> {
    let session = state.world.get_session(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let reviewer = match body.reviewer_agent_id {
        Some(r) => Some(load(&state, r).await?),
        None => None,
    };
    let choice = match body.reviewer_model.as_deref().filter(|m| !m.trim().is_empty()) {
        Some(model) => Some(
            loka_shared::world::ReviewerChoice::new(body.reviewer_cli.as_deref(), model)
                .map_err(|_| StatusCode::BAD_REQUEST)?,
        ),
        None => None,
    };
    if choice.as_ref().and_then(|c| c.cli.as_deref()) == Some(loka_shared::backends::OPENCODE) {
        let agent = load(&state, session.agent_id).await?;
        if !opencode_allowed_for(&state, &agent).await? {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let run = state
        .world
        .request_review(&session, reviewer.as_ref(), choice.as_ref(), body.focus.as_deref(), session.agent_id)
        .await
        .map_err(|e| {
            warn!(error = %e, "review refused");
            StatusCode::CONFLICT
        })?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[derive(Deserialize)]
pub struct BellQuery {
    #[serde(default)]
    cleared: bool,
}

pub async fn notifications(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<BellQuery>,
) -> Result<Json<loka_shared::world::Notifications>, StatusCode> {
    Ok(Json(state.world.notifications(user.0, viewer.org, 60, q.cleared).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct Seen {
    agent_id: Option<Uuid>,
}

pub async fn notifications_seen(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<Seen>,
) -> Result<StatusCode, StatusCode> {
    if let Some(agent_id) = body.agent_id {
        state.world.mark_agent_thread_seen(user.0, agent_id).await.map_err(internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct BellClearBody {
    #[serde(default)]
    ids: Vec<i64>,
    #[serde(default)]
    all: bool,
}

pub async fn notifications_clear(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<BellClearBody>,
) -> Result<StatusCode, StatusCode> {
    let which = if body.all {
        loka_shared::world::BellClear::All
    } else if body.ids.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    } else {
        loka_shared::world::BellClear::Items(body.ids)
    };
    state.world.clear_notifications(user.0, viewer.org, which).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct ChannelDetail {
    #[serde(flatten)]
    channel: loka_shared::channels::Channel,
    members: Vec<loka_shared::channels::ChannelMember>,
}

pub async fn list_channels(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Vec<loka_shared::channels::ChannelSummary>>, StatusCode> {
    Ok(Json(state.world.channels_for_user(viewer.user, viewer.org()).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct NewChannel {
    name: String,
    topic: Option<String>,
    agents: Option<Vec<Uuid>>,
    users: Option<Vec<Uuid>>,
}

pub async fn create_channel(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<NewChannel>,
) -> Result<(StatusCode, Json<ChannelDetail>), StatusCode> {
    for a in body.agents.iter().flatten() {
        let agent = state.world.get_agent(*a).await.map_err(internal)?.ok_or(StatusCode::BAD_REQUEST)?;
        if !access::can_use(&viewer, &agent) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    for u in body.users.iter().flatten() {
        if !state.world.is_org_member(viewer.org(), *u).await.map_err(internal)? {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let channel = state
        .world
        .create_channel(viewer.org, &body.name, body.topic.as_deref().filter(|t| !t.trim().is_empty()), user.0)
        .await
        .map_err(|e| {
            warn!(error = %e, "channel create refused");
            StatusCode::CONFLICT
        })?;
    for a in body.agents.unwrap_or_default() {
        state.world.add_member(channel.id, "agent", a).await.map_err(internal)?;
    }
    for u in body.users.unwrap_or_default() {
        state.world.add_member(channel.id, "user", u).await.map_err(internal)?;
    }
    let members = state.world.channel_members(channel.id).await.map_err(internal)?;
    Ok((StatusCode::CREATED, Json(ChannelDetail { channel, members })))
}

#[derive(Deserialize)]
pub struct OpenDm {
    user_id: Uuid,
}

pub async fn open_dm(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<OpenDm>,
) -> Result<Json<ChannelDetail>, StatusCode> {
    if !state.world.is_org_member(viewer.org(), body.user_id).await.map_err(internal)? {
        return Err(StatusCode::NOT_FOUND);
    }
    let channel = state.world.open_dm(viewer.org(), user.0, body.user_id).await.map_err(internal)?;
    let members = state.world.channel_members(channel.id).await.map_err(internal)?;
    Ok(Json(ChannelDetail { channel, members }))
}

async fn visible_channel(state: &AppState, user: Uuid, id: Uuid) -> Result<loka_shared::channels::Channel, StatusCode> {
    let viewer = crate::access::viewer_for(state, user).await?;
    let channel = state.world.get_channel(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if !viewer.in_org(channel.org_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    if channel.kind != "channel" && !state.world.is_member(id, "user", user).await.map_err(internal)? {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(channel)
}

pub async fn get_channel(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelDetail>, StatusCode> {
    let channel = visible_channel(&state, user.0, id).await?;
    let members = state.world.channel_members(id).await.map_err(internal)?;
    Ok(Json(ChannelDetail { channel, members }))
}

#[derive(Deserialize)]
pub struct AfterQuery {
    after: Option<i64>,
    before: Option<i64>,
}

pub async fn channel_replies(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((id, message_id)): Path<(Uuid, i64)>,
) -> Result<Json<Vec<loka_shared::channels::ChannelMessage>>, StatusCode> {
    visible_channel(&state, user.0, id).await?;
    let root = state.world.channel_thread_root(id, message_id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(state.world.channel_thread(id, root, loka_shared::channels::THREAD_LIMIT).await.map_err(internal)?))
}

pub async fn channel_messages(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Query(q): Query<AfterQuery>,
) -> Result<Json<Vec<loka_shared::channels::ChannelMessage>>, StatusCode> {
    visible_channel(&state, user.0, id).await?;
    Ok(Json(state.world.channel_messages_page(id, q.after, q.before, 200).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct PostChannel {
    body: String,
    #[serde(default)]
    attachments: Vec<Uuid>,
    #[serde(default)]
    reply_to: Option<i64>,
    #[serde(default)]
    spoken: bool,
    #[serde(default)]
    quote: Option<Value>,
}

pub async fn post_channel(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<PostChannel>,
) -> Result<(StatusCode, Json<loka_shared::channels::ChannelMessage>), StatusCode> {
    let channel = visible_channel(&state, user.0, id).await?;
    let attachments = attachment_meta(&state, &viewer, &body.attachments).await?;
    let links = visible_links(&state, &viewer, &body.body).await?;
    let msg = state
        .world
        .post_channel_reply(
            &channel,
            loka_shared::channels::Author::User(user.0),
            &body.body,
            {
                let mut meta = json!({ "attachments": attachments, "links": links });
                if body.spoken {
                    meta["spoken"] = json!(true);
                }
                if let Some(quote) = loka_shared::quotes::clean(body.quote.as_ref()) {
                    meta["quote"] = quote;
                }
                meta
            },
            body.reply_to,
        )
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if msg.reply_to.is_none() {
        state.world.mark_channel_read(user.0, id, msg.id).await.map_err(internal)?;
    }
    Ok((StatusCode::CREATED, Json(msg)))
}

pub async fn edit_channel_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((id, message_id)): Path<(Uuid, i64)>,
    Json(body): Json<EditMessage>,
) -> Result<Json<loka_shared::channels::ChannelMessage>, StatusCode> {
    let text = body.body.trim();
    if text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    visible_channel(&state, user.0, id).await?;
    let msg = state
        .world
        .edit_channel_message(id, message_id, loka_shared::channels::Author::User(user.0), text)
        .await
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(msg))
}

pub async fn delete_channel_message(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((id, message_id)): Path<(Uuid, i64)>,
) -> Result<StatusCode, StatusCode> {
    visible_channel(&state, user.0, id).await?;
    match state.world.delete_channel_message(id, message_id, user.0).await.map_err(internal)? {
        loka_shared::channels::DeleteOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        loka_shared::channels::DeleteOutcome::NotFound => Err(StatusCode::NOT_FOUND),
        loka_shared::channels::DeleteOutcome::HasReplies => Err(StatusCode::CONFLICT),
    }
}

#[derive(Deserialize)]
pub struct MemberBody {
    kind: String,
    id: Uuid,
}

pub async fn add_channel_member(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<MemberBody>,
) -> Result<StatusCode, StatusCode> {
    let channel = visible_channel(&state, user.0, id).await?;
    if channel.kind != "channel" || !matches!(body.kind.as_str(), "user" | "agent") {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.kind == "agent" {
        let agent = state.world.get_agent(body.id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
        if !loka_shared::access::can_use(&viewer, &agent) {
            return Err(StatusCode::FORBIDDEN);
        }
    } else if !state.world.is_org_member(viewer.org(), body.id).await.map_err(internal)? {
        return Err(StatusCode::NOT_FOUND);
    }
    state.world.add_member(id, &body.kind, body.id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_channel_member(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path((id, kind, member)): Path<(Uuid, String, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    visible_channel(&state, user.0, id).await?;
    let role = viewer.role;
    let allowed = role == Role::Admin
        || (kind == "user" && member == user.0)
        || (kind == "agent"
            && state
                .world
                .get_agent(member)
                .await
                .map_err(internal)?
                .is_some_and(|agent| access::can_manage(&viewer, &agent)));
    if !allowed {
        return Err(StatusCode::FORBIDDEN);
    }
    state.world.remove_member(id, &kind, member).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ReadBody {
    last_id: i64,
}

pub async fn read_channel(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<ReadBody>,
) -> Result<StatusCode, StatusCode> {
    state.world.mark_channel_read(user.0, id, body.last_id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct Me {
    id: Uuid,
    name: Option<String>,
    email: Option<String>,
    handle: String,
    role: Role,
    main_agent_id: Option<Uuid>,
    status_emoji: Option<String>,
    status_text: Option<String>,
    mention_wakes_main: bool,
    onboarding: bool,
    org: Option<MeOrg>,
}

#[derive(Serialize)]
pub struct MeOrg {
    id: Uuid,
    name: String,
    slug: String,
    role: loka_shared::orgs::OrgRole,
}

fn me_view((id, name, email, main_agent_id, status_emoji, status_text): loka_shared::world::MeRow, role: Role) -> Me {
    let base = name.clone().or_else(|| email.as_deref().and_then(|e| e.split('@').next()).map(String::from)).unwrap_or_default();
    let handle: String = base.chars().filter(|c| c.is_alphanumeric()).collect();
    let handle = if handle.is_empty() { "you".to_string() } else { handle };
    Me {
        id,
        name,
        email,
        handle,
        role,
        main_agent_id,
        status_emoji,
        status_text,
        mention_wakes_main: true,
        onboarding: false,
        org: None,
    }
}

async fn with_org(state: &AppState, me: &mut Me, viewer: &Viewer) -> Result<(), StatusCode> {
    if let (Some(org), Some(role)) = (viewer.org, viewer.org_role) {
        if let Some(found) = state.world.get_org(org).await.map_err(internal)? {
            me.org = Some(MeOrg { id: found.id, name: found.name, slug: found.slug, role });
        }
        me.main_agent_id = state.world.main_agent_in(viewer.user, org).await.map_err(internal)?;
    } else {
        me.main_agent_id = None;
    }
    Ok(())
}

pub async fn get_me(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Me>, StatusCode> {
    let row = state.world.me(user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let mut me = me_view(row, role);
    with_org(&state, &mut me, &viewer).await?;
    me.mention_wakes_main = state.world.mention_wakes_main(user.0).await.map_err(internal)?;
    me.onboarding = role >= Role::Write && state.world.onboarding_pending(user.0).await.map_err(internal)?;
    Ok(Json(me))
}

#[derive(Deserialize)]
pub struct SetMe {
    name: Option<String>,
    main_agent_id: Option<Uuid>,
    status: Option<SetStatus>,
    mention_wakes_main: Option<bool>,
}

#[derive(Deserialize)]
pub struct SetStatus {
    emoji: Option<String>,
    text: Option<String>,
}

pub async fn set_me(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<SetMe>,
) -> Result<Json<Me>, StatusCode> {
    if let Some(name) = body.name.as_deref().map(str::trim) {
        if name.is_empty() || name.chars().count() > 40 || !name.chars().any(|c| c.is_alphanumeric()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        state.world.set_user_name(user.0, name).await.map_err(internal)?;
    }
    if let Some(status) = &body.status {
        let emoji = status.emoji.as_deref().map(str::trim).filter(|e| !e.is_empty());
        let text = status.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
        if emoji.is_some_and(|e| e.chars().count() > 16) || text.is_some_and(|t| t.chars().count() > 100) {
            return Err(StatusCode::BAD_REQUEST);
        }
        state.world.set_user_status(user.0, emoji, text).await.map_err(internal)?;
    }
    if let Some(agent_id) = body.main_agent_id {
        if role < Role::Write {
            return Err(StatusCode::FORBIDDEN);
        }
        let agent = load(&state, agent_id).await?;
        if agent.owner_user_id != Some(user.0) || !viewer.in_org(agent.org_id) {
            return Err(StatusCode::FORBIDDEN);
        }
        make_main(&state, user.0, &agent).await?;
    }
    if let Some(on) = body.mention_wakes_main {
        state.world.set_mention_wakes_main(user.0, on).await.map_err(internal)?;
    }
    let row = state.world.me(user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let mut me = me_view(row, role);
    with_org(&state, &mut me, &viewer).await?;
    me.mention_wakes_main = state.world.mention_wakes_main(user.0).await.map_err(internal)?;
    me.onboarding = role >= Role::Write && state.world.onboarding_pending(user.0).await.map_err(internal)?;
    Ok(Json(me))
}

pub async fn list_quests(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Vec<loka_shared::world::TaskBrief>>, StatusCode> {
    let hidden = state.world.hidden_agents(&viewer).await.map_err(internal)?;
    let tasks = state.world.quests(user.0, viewer.org(), 300).await.map_err(internal)?;
    let mut tasks: Vec<_> = tasks
        .into_iter()
        .filter(|t| t.assigned_agent_id.map_or(role >= Role::Write, |a| !hidden.contains(&a)))
        .collect();
    let ids: Vec<Uuid> = tasks.iter().map(|t| t.id).collect();
    let mut milestones = MilestoneStore::new(state.world.pool().clone())
        .for_tasks(&ids)
        .await
        .map_err(internal)?;
    for task in &mut tasks {
        let (mine, rest): (Vec<_>, Vec<_>) = milestones.into_iter().partition(|m| m.task_id == task.id);
        task.milestones = mine;
        milestones = rest;
    }
    Ok(Json(tasks))
}

async fn milestone_task(state: &AppState, role: Role, user: Uuid, task_id: Uuid) -> Result<(), StatusCode> {
    let viewer = crate::access::viewer_for(state, user).await?;
    if viewer.role != role {
        return Err(StatusCode::FORBIDDEN);
    }
    quest_task(state, &viewer, task_id).await.map(|_| ())
}

pub(crate) async fn quest_task(state: &AppState, viewer: &Viewer, task_id: Uuid) -> Result<loka_shared::models::AgentTask, StatusCode> {
    let task = state.db.get_task(task_id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if !viewer.in_org(task.org_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    if viewer.role == Role::Admin || task.user_id == viewer.user {
        return Ok(task);
    }
    let agent = match task.assigned_agent_id {
        Some(id) => state.world.get_agent(id).await.map_err(internal)?,
        None => None,
    };
    match agent {
        Some(agent) if loka_shared::access::can_use(viewer, &agent) => Ok(task),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

#[derive(Deserialize)]
pub struct RoomQuery {
    after: Option<i64>,
    since: Option<DateTime<Utc>>,
}

pub async fn quest_room(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    Query(q): Query<RoomQuery>,
) -> Result<Json<Value>, StatusCode> {
    let task = quest_task(&state, &viewer, task_id).await?;
    let agents = state.world.list_agents_in(viewer.org).await.map_err(internal)?;
    let usable: Vec<Uuid> = agents.iter().filter(|a| access::can_use(&viewer, a)).map(|a| a.id).collect();
    let since = task.created_at.unwrap_or_else(Utc::now) - chrono::Duration::minutes(5);
    let mut room = state.world.quest_room(task_id, since, &usable, user.0, q.after, q.since).await.map_err(internal)?;
    let owned = state.world.owned_agent_ids(user.0).await.map_err(internal)?;
    room.events.retain(|e| {
        !loka_shared::slack::hidden_from(e, user.0)
            && !loka_shared::loka::hidden_from(e, user.0)
            && !loka_shared::shell::hidden_from(e, &owned)
            && !loka_shared::event_scope::private_from(e, user.0, &owned)
    });
    let milestones = MilestoneStore::new(state.world.pool().clone()).for_task(task_id).await.map_err(internal)?;
    let options = state.world.quest_options(task_id).await.map_err(internal)?;
    let parts = state.world.quest_parts(task_id).await.map_err(internal)?;
    let parent = state.world.quest_parent(task_id).await.map_err(internal)?;
    Ok(Json(json!({
        "task": {
            "parts": parts,
            "parent": parent,
            "plan_first": options.plan_first,
            "attempts": options.attempts,
            "attempt_variants": options.attempt_variants,
            "ci_feedback": options.ci_feedback,
            "pace": options.pace,
            "id": task.id,
            "title": task.title,
            "description": task.description,
            "status": task.status,
            "repo": task.repo,
            "branch": task.branch,
            "pr_url": task.pr_url,
            "assigned_agent_id": task.assigned_agent_id,
            "user_id": task.user_id,
            "result_content": task.result_content,
            "error_message": task.error_message,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
            "milestones": milestones,
        },
        "room": room,
    })))
}

pub async fn list_milestones(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<Vec<Milestone>>, StatusCode> {
    milestone_task(&state, role, user.0, task_id).await?;
    Ok(Json(MilestoneStore::new(state.world.pool().clone()).for_task(task_id).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct AddMilestones {
    items: Vec<NewMilestone>,
}

pub async fn add_milestones(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<AddMilestones>,
) -> Result<(StatusCode, Json<Vec<Milestone>>), StatusCode> {
    milestone_task(&state, role, user.0, task_id).await?;
    let store = MilestoneStore::new(state.world.pool().clone());
    let added = store.add(task_id, &body.items).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    let task = state.db.get_task(task_id).await.map_err(internal)?;
    if let Some(task) = task.filter(|t| t.status == "review" && t.pr_url.is_some()) {
        if added.iter().any(|m| m.kind == "merge") {
            store.arm_merge(task.id, true).await.map_err(internal)?;
            store.check_now(task.id).await.map_err(internal)?;
        }
    }
    emit_milestones(&state, task_id, user.0).await?;
    Ok((StatusCode::CREATED, Json(added)))
}

#[derive(Deserialize)]
pub struct MilestonePatch {
    title: Option<String>,
    status: Option<String>,
}

pub async fn update_milestone(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(body): Json<MilestonePatch>,
) -> Result<Json<Milestone>, StatusCode> {
    let store = MilestoneStore::new(state.world.pool().clone());
    let current = store.get(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    milestone_task(&state, role, user.0, current.task_id).await?;
    let updated = store
        .update(id, body.title.as_deref(), body.status.as_deref())
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if body.status.as_deref() == Some("pending") && updated.kind == "merge" {
        if let Some(task) = state.db.get_task(updated.task_id).await.map_err(internal)? {
            if task.pr_url.is_some() && task.status == "review" {
                store.arm_merge(task.id, true).await.map_err(internal)?;
                store.check_now(task.id).await.map_err(internal)?;
            }
        }
    }
    emit_milestones(&state, updated.task_id, user.0).await?;
    Ok(Json(store.get(id).await.map_err(internal)?.unwrap_or(updated)))
}

pub async fn delete_milestone(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let store = MilestoneStore::new(state.world.pool().clone());
    let current = store.get(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    milestone_task(&state, role, user.0, current.task_id).await?;
    store.delete(id).await.map_err(internal)?;
    emit_milestones(&state, current.task_id, user.0).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn check_milestones(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    milestone_task(&state, role, user.0, task_id).await?;
    let queued = MilestoneStore::new(state.world.pool().clone()).check_now(task_id).await.map_err(internal)?;
    Ok(Json(json!({ "queued": queued })))
}

async fn emit_milestones(state: &AppState, task_id: Uuid, user: Uuid) -> Result<(), StatusCode> {
    let agent_id = state.db.get_task(task_id).await.map_err(internal)?.and_then(|t| t.assigned_agent_id);
    let mut event = NewEvent::new("milestones.updated").task(Some(task_id)).actor(Some(user)).payload(json!({ "task_id": task_id }));
    if let Some(agent_id) = agent_id {
        event = event.agent(agent_id);
    }
    state.world.emit(event, None).await.map_err(internal)?;
    Ok(())
}

pub async fn list_items(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Vec<loka_shared::items::WorldItem>>, StatusCode> {
    Ok(Json(state.world.items_for(Some(user.0), viewer.org, viewer.role == Role::Admin).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct ItemPatch {
    name: Option<String>,
    data: Option<Value>,
    status: Option<String>,
    #[serde(default)]
    group: bool,
}

pub async fn update_item(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(body): Json<ItemPatch>,
) -> Result<Json<loka_shared::items::WorldItem>, StatusCode> {
    let item = state.world.get_item(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let viewer = crate::access::viewer_for(&state, user.0).await?;
    if !viewer.in_org(item.org_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mine = item.owner_user_id == Some(user.0);
    if !(mine || role == Role::Admin) {
        return Err(StatusCode::FORBIDDEN);
    }
    if body.status.as_deref().is_some_and(|s| !matches!(s, "active" | "done" | "archived")) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.group && body.status.as_deref() == Some("archived") && item.kind == "timer" && item.status == "done" {
        state.world.archive_finished_like(&item).await.map_err(internal)?;
    }
    state
        .world
        .update_item(id, body.name.as_deref(), body.data.as_ref(), body.status.as_deref())
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn delete_agent(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let agent = load(&state, id).await?;
    if agent.is_main {
        return Err(StatusCode::CONFLICT);
    }
    state.world.delete_agent(id).await.map_err(internal)?;
    state
        .world
        .emit(
            NewEvent::new("agent.removed").actor(Some(user.0)).payload(json!({ "agent_id": id, "name": agent.name })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct ForkBody {
    name: Option<String>,
}

pub async fn fork_agent(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    body: Option<Json<ForkBody>>,
) -> Result<Json<WorldAgent>, StatusCode> {
    let source = load(&state, id).await?;
    let body = body.map(|b| b.0).unwrap_or_default();
    let name = body
        .name
        .as_deref()
        .map(|n| n.trim().chars().take(60).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{} Jr", source.name));
    let base = slugify(&name);
    let mut slug = base.clone();
    let mut n = 2;
    while state.world.slug_taken(&slug).await.map_err(internal)? {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let by = state.world.user_display_name(user.0).await.map_err(internal)?.unwrap_or_else(|| "someone".to_string());
    let fork = state.world.fork_agent(id, user.0, &slug, &name, &by).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let agent = load(&state, fork.agent_id).await?;
    state
        .world
        .emit(
            NewEvent::new("agent.created").agent(agent.id).actor(Some(user.0)).payload(json!({
                "name": agent.name,
                "slug": agent.slug,
                "created_by": by,
                "forked_from": { "id": source.id, "name": source.name },
                "wanted_keys": fork.wanted_keys,
            })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok(Json(agent))
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "agent".to_string()
    } else {
        out
    }
}
