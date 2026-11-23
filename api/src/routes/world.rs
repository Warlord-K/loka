use std::convert::Infallible;
use std::collections::HashSet;
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
use loka_shared::quests::{ClarificationAnswer, ClarificationReply, PlanAnswer};
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
    if disk_full(&e) {
        return StatusCode::INSUFFICIENT_STORAGE;
    }
    StatusCode::INTERNAL_SERVER_ERROR
}

fn disk_full(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        if let Some(sqlx::Error::Database(db)) = cause.downcast_ref::<sqlx::Error>() {
            if db.code().as_deref() == Some("53100") {
                return true;
            }
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if io.raw_os_error() == Some(28) || io.kind() == std::io::ErrorKind::StorageFull {
                return true;
            }
        }
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("no space left on device") || text.contains("could not extend file")
    })
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
    if body.quest_context.as_deref().is_some_and(|c| loka_shared::context::QuestContext::parse(c).is_err()) {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid_quest_context", Some(loka_shared::context::QUEST_CONTEXT_ERROR)));
    }
    if body.asking.as_deref().is_some_and(|a| loka_shared::quest_questions::Asking::parse(a).is_err()) {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid_asking", Some(loka_shared::quest_questions::ASKING_ERROR)));
    }
    if let Some(Err(error)) = body.crew_variants.as_deref().map(|v| loka_shared::quest_questions::clean_crew_variants(v, None)) {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid_crew_variants", Some(&error)));
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
    if let Some(session) = state.world.quick_thread_session(id, root).await.map_err(internal)? {
        crate::routes::quick::deliver_lost_replies(&state, session.id).await;
    }
    let mut rows = state
        .world
        .message_replies_with_reply(id, root, user.0, loka_shared::replies::REPLY_LIMIT, (message_id != root).then_some(message_id))
        .await
        .map_err(internal)?;
    let owner = state.world.get_agent(id).await.map_err(internal)?.and_then(|agent| agent.owner_user_id);
    if owner != Some(user.0) {
        rows.retain(|m| {
            m.meta.get("kind").and_then(Value::as_str) != Some("shell")
                && m.meta.get("shell_share").is_none()
                && m.meta.get("shell_command").is_none()
        });
        for message in &mut rows {
            if message.meta.get("clarify_request").is_some_and(Value::is_object) {
                message.meta["clarify_request"] = Value::Bool(true);
            }
        }
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
        for message in &mut rows {
            if message.meta.get("clarify_request").is_some_and(Value::is_object) {
                message.meta["clarify_request"] = Value::Bool(true);
            }
        }
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
    #[serde(default)]
    changes: Option<Uuid>,
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
        let id = attachment.get("id").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
        let mut file = json!({ "name": name, "mime": mime, "id": id });
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
        if !state.world.sees_message_as(id, viewer).await.map_err(internal)? {
            return Ok(false);
        }
        if link.task_id.is_some() {
            return Ok(true);
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

fn private_clarification_handoff(source: &Value) -> Option<&Value> {
    source.get("clarification_continuation")?.as_str()?.parse::<Uuid>().ok()?;
    let handoff = source.get("clarification_handoff")?;
    let fields = handoff.as_object()?;
    fields.get("request")?.as_str()?;
    fields.get("question")?.as_str()?;
    fields.get("answer")?.as_str()?;
    Some(handoff)
}

async fn clarification_source_resources(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    origin: &Value,
    agent_id: Uuid,
    user_id: Uuid,
    org_id: Option<Uuid>,
    task_id: Option<Uuid>,
) -> Result<Value, sqlx::Error> {
    let inbox_id = origin.get("source_inbox_id").and_then(Value::as_i64);
    let supplied_message_id = origin.get("message_id").filter(|value| !value.is_null());
    let message_reference = supplied_message_id.and_then(|value| match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    });
    let invalid_message_reference = supplied_message_id.is_some() && message_reference.is_none();
    let message_id = message_reference.as_deref().and_then(|id| Uuid::parse_str(id).ok());
    let channel_route = origin.get("clarification_channel").filter(|route| route.is_object());
    let source = if invalid_message_reference {
        None
    } else if let (Some(inbox_id), Some(route)) = (inbox_id, channel_route) {
        if !loka_shared::world::clarification_channel_allowed(tx, agent_id, route).await? {
            None
        } else {
            sqlx::query_scalar::<_, Value>(
                "SELECT i.payload
                 FROM agent_inbox i
                 JOIN world_events e ON e.seq = i.event_seq
                 WHERE i.id = $1 AND i.agent_id = $2 AND e.actor_user_id = $3
                   AND e.org_id IS NOT DISTINCT FROM $4
                   AND i.payload->>'task_id' IS NULL
                   AND ($6::text IS NULL OR i.payload->>'message_id' = $6)
                   AND (
                     (i.kind = 'channel.mention'
                       AND i.payload->>'channel_id' = ($5::jsonb)->>'channel_id'
                       AND i.payload->>'message_id' = ($5::jsonb)->>'message_id'
                       AND COALESCE(i.payload->>'reply_to', i.payload->>'message_id') = ($5::jsonb)->>'root_id')
                     OR
                     (i.kind = 'message'
                       AND i.payload->'clarification_channel' = $5::jsonb
                       AND world_safe_uuid(i.payload->>'clarification_continuation') IS NOT NULL
                       AND jsonb_typeof(i.payload->'clarification_handoff') = 'object'
                       AND jsonb_typeof(i.payload->'clarification_handoff'->'request') = 'string'
                       AND jsonb_typeof(i.payload->'clarification_handoff'->'question') = 'string'
                       AND jsonb_typeof(i.payload->'clarification_handoff'->'answer') = 'string')
                   )",
            )
            .bind(inbox_id)
            .bind(agent_id)
            .bind(user_id)
            .bind(org_id)
            .bind(route)
            .bind(message_reference)
            .fetch_optional(&mut **tx)
            .await?
        }
    } else if channel_route.is_some() {
        None
    } else if let Some(inbox_id) = inbox_id {
        sqlx::query_scalar::<_, Value>(
            "SELECT i.payload
             FROM agent_inbox i
             JOIN world_events e ON e.seq = i.event_seq
             WHERE i.id = $1 AND i.agent_id = $2 AND e.actor_user_id = $3
               AND e.org_id IS NOT DISTINCT FROM $4
               AND i.kind IN ('message', 'task.assigned')
               AND i.payload->>'task_id' IS NOT DISTINCT FROM $5::uuid::text
               AND ($6::text IS NULL OR i.payload->>'message_id' = $6)",
        )
        .bind(inbox_id)
        .bind(agent_id)
        .bind(user_id)
        .bind(org_id)
        .bind(task_id)
        .bind(message_reference)
        .fetch_optional(&mut **tx)
        .await?
    } else if let Some(message_id) = message_id {
        let snapshots = sqlx::query_as::<_, (Value, Option<Value>, bool)>(
            "SELECT e.payload, i.payload,
                    e.actor_user_id IS NOT DISTINCT FROM $3
                    AND e.org_id IS NOT DISTINCT FROM $4
                    AND m.author_user_id IS NOT DISTINCT FROM $3
                    AND (i.id IS NULL OR i.payload->>'task_id' IS NOT DISTINCT FROM $5::uuid::text)
                    AND (
                      (e.task_id IS NOT DISTINCT FROM $5
                        AND m.meta->>'task_id' IS NOT DISTINCT FROM $5::uuid::text)
                      OR COALESCE(
                        i.id IS NOT NULL
                        AND world_safe_uuid(i.payload->>'clarification_continuation') IS NOT NULL
                        AND jsonb_typeof(i.payload->'clarification_handoff') = 'object'
                        AND jsonb_typeof(i.payload->'clarification_handoff'->'request') = 'string'
                        AND jsonb_typeof(i.payload->'clarification_handoff'->'question') = 'string'
                        AND jsonb_typeof(i.payload->'clarification_handoff'->'answer') = 'string'
                        AND m.meta->>'answers' = i.payload->>'clarification_continuation',
                        FALSE
                      )
                    )
                    AND (i.id IS NULL OR i.payload->>'message_id' IS NOT DISTINCT FROM e.payload->>'message_id')
                    AND ($6::uuid IS NULL OR e.payload->>'message_id' = $6::text) AS scoped
             FROM world_events e
             JOIN agent_messages m ON m.id::text = e.payload->>'message_id'
               AND m.agent_id = e.agent_id AND m.author_kind = 'user'
             LEFT JOIN agent_inbox i ON i.event_seq = e.seq AND i.agent_id = $2 AND i.kind = 'message'
             WHERE e.agent_id = $2 AND e.type = 'message.created'
               AND e.payload->>'message_id' = $1::text
             LIMIT 2",
        )
        .bind(message_id)
        .bind(agent_id)
        .bind(user_id)
        .bind(org_id)
        .bind(task_id)
        .bind(message_id)
        .fetch_all(&mut **tx)
        .await?;
        if snapshots.len() == 1 && snapshots[0].2 {
            Some(snapshots[0].1.clone().unwrap_or_else(|| snapshots[0].0.clone()))
        } else if snapshots.is_empty() {
            sqlx::query_scalar::<_, Value>(
                "SELECT m.meta || jsonb_build_object('body', m.body)
                 FROM agent_messages m
                 JOIN agent_profiles a ON a.id = m.agent_id
                 WHERE m.id = $1 AND m.agent_id = $2 AND m.author_kind = 'user'
                   AND m.author_user_id = $3 AND a.org_id IS NOT DISTINCT FROM $4
                   AND m.meta->>'task_id' IS NOT DISTINCT FROM $5::uuid::text",
            )
            .bind(message_id)
            .bind(agent_id)
            .bind(user_id)
            .bind(org_id)
            .bind(task_id)
            .fetch_optional(&mut **tx)
            .await?
        } else {
            None
        }
    } else {
        None
    };
    let Some(mut source) = source else {
        return Ok(json!({}));
    };
    if let Some(route) = channel_route {
        let source_message = route.get("message_id").and_then(Value::as_i64).unwrap_or_default();
        let root = route.get("root_id").and_then(Value::as_i64).unwrap_or_default();
        let entry = format!(
            "[msg:ch{source_message}] {}: {}",
            source.get("author").and_then(Value::as_str).unwrap_or("?"),
            source.get("body").and_then(Value::as_str).unwrap_or_default()
        );
        let transcript = source.get("transcript").and_then(Value::as_str).unwrap_or_default();
        let thread = match transcript.rfind(&entry) {
            Some(start) => format!("{}{}", &transcript[..start], &transcript[start + entry.len()..]).trim().to_string(),
            None => transcript.trim().to_string(),
        };
        source["reply_to"] = json!(format!("ch{root}"));
        source["thread"] = json!(thread);
        source["clarification_channel"] = route.clone();
    }
    let private_handoff = private_clarification_handoff(&source);
    let source_limit = if private_handoff.is_some() { 2 } else { 1 };
    let mut requested = Vec::new();
    for id in source
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attachment| attachment.get("id").and_then(Value::as_str).and_then(|id| Uuid::parse_str(id).ok()))
    {
        if !requested.contains(&id) && requested.len() < loka_shared::media::MAX_PER_CALL * source_limit {
            requested.push(id);
        }
    }
    let allowed = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, mime, name FROM world_uploads
         WHERE id = ANY($1) AND uploaded_by = $2 AND org_id IS NOT DISTINCT FROM $3",
    )
    .bind(&requested)
    .bind(user_id)
    .bind(org_id)
    .fetch_all(&mut **tx)
    .await?;
    let attachments: Vec<Value> = requested
        .iter()
        .filter_map(|id| {
            allowed.iter().find(|(found, _, _)| found == id).map(|(_, mime, name)| {
                json!({ "id": id, "mime": mime, "name": name, "url": format!("/api/world/uploads/{id}") })
            })
        })
        .collect();
    let files: Vec<Value> = source
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|file| {
            file.get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_some_and(|id| allowed.iter().any(|(found, _, _)| *found == id))
        })
        .take(loka_shared::media::MAX_ATTACHED_FILES * source_limit)
        .cloned()
        .collect();
    Ok(json!({
        "body": source.get("body").cloned().unwrap_or(Value::Null),
        "attachments": attachments,
        "files": files,
        "links": source.get("links").cloned().unwrap_or_else(|| json!([])),
        "quote": source.get("quote").cloned().unwrap_or(Value::Null),
        "reply_to": source.get("reply_to").cloned().unwrap_or(Value::Null),
        "thread": source.get("thread").cloned().unwrap_or(Value::Null),
        "clarification_handoff": private_handoff.cloned().unwrap_or(Value::Null),
        "clarification_channel": source.get("clarification_channel").cloned().unwrap_or(Value::Null),
    }))
}

fn merge_resource_list(answer: &Value, source: &Value, field: &str, identity: &str, limit: usize) -> Value {
    let mut merged = Vec::new();
    for item in answer
        .as_array()
        .into_iter()
        .flatten()
        .chain(source.get(field).and_then(Value::as_array).into_iter().flatten())
    {
        let duplicate = item.get(identity).is_some_and(|value| merged.iter().any(|existing: &Value| existing.get(identity) == Some(value)));
        if !duplicate && merged.len() < limit {
            merged.push(item.clone());
        }
    }
    Value::Array(merged)
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MessageRoute<'a> {
    Structured,
    Shell(&'a str),
    Plain,
}

pub(crate) fn message_route(answers: Option<Uuid>, changes: Option<Uuid>, text: &str) -> MessageRoute<'_> {
    if answers.is_some() || changes.is_some() {
        return MessageRoute::Structured;
    }
    match text.strip_prefix('!') {
        Some(command) => MessageRoute::Shell(command.trim()),
        None => MessageRoute::Plain,
    }
}

fn correction_agent_allowed(agent_id: Uuid, question: &loka_shared::quest_questions::ChangeableQuestion) -> bool {
    agent_id == question.agent_id || question.lead_agent_id == Some(agent_id)
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
    if let MessageRoute::Shell(command) = message_route(body.answers, body.changes, text) {
        if command.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let agent = load(&state, id).await?;
        let msg = crate::routes::shell::start_shell(&state, user.0, &agent, command, body.share_output, body.run_on)
            .await
            .map_err(|(status, _)| status)?;
        return Ok((StatusCode::CREATED, Json(msg)));
    }
    load(&state, id).await?;
    if let Some(card) = body.answers {
        if !state.world.can_answer_card(id, card, user.0).await.map_err(internal)? {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let changes = match body.changes {
        Some(decision) => {
            let question = state.world.changeable_question_context(decision, user.0).await.map_err(internal)?.ok_or(StatusCode::FORBIDDEN)?;
            if !correction_agent_allowed(id, &question) {
                return Err(StatusCode::FORBIDDEN);
            }
            if let Some(task_id) = body.task_id {
                let supplied_room = state.world.room_of(task_id).await.map_err(internal)?;
                if question.task_id != Some(supplied_room) {
                    return Err(StatusCode::FORBIDDEN);
                }
            }
            Some(question)
        }
        None => None,
    };
    if changes.is_some() && text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let clarification_origin = match body.answers {
        Some(card) => sqlx::query_scalar::<_, Option<Value>>(&format!(
            "SELECT meta->'clarify_request' FROM agent_messages m WHERE m.id = $1 AND m.agent_id = $2 AND {}",
            loka_shared::guests::thread_filter("$2", "$3")
        ))
        .bind(card)
        .bind(id)
        .bind(user.0)
        .fetch_optional(state.world.pool())
        .await
        .map_err(|e| internal(e.into()))?
        .flatten(),
        None => None,
    };
    let clarification_task = clarification_origin
        .as_ref()
        .filter(|origin| origin.is_object())
        .and_then(loka_shared::world::main_clarification_task);
    let clarification_context = clarification_origin
        .as_ref()
        .filter(|origin| origin.is_object())
        .and_then(loka_shared::world::main_clarification_context_task);
    if clarification_task.is_some() && clarification_context.is_some() {
        return Err(StatusCode::CONFLICT);
    }
    if let Some(task) = body.task_id {
        if clarification_task.or(clarification_context).is_some_and(|scope| scope != task) {
            return Err(StatusCode::CONFLICT);
        }
        if clarification_context != Some(task) {
            quest_task(&state, &viewer, task).await?;
        }
    }
    let reply_to = match body.reply_to {
        Some(root) => Some(state.world.agent_thread_root(id, root, user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?),
        None => None,
    };
    let attachments = attachment_meta(&state, &viewer, &body.attachments).await?;
    let mut quote = state.world.clean_quote(&viewer, body.quote.as_ref()).await.map_err(internal)?;
    if let (Some(q), Some(org)) = (quote.as_mut(), viewer.org) {
        state.world.expand_canvas_quote(user.0, org, q, loka_shared::canvas::QuoteCards::Resolved).await.map_err(internal)?;
        state.world.expand_practice_quote(user.0, viewer.role, org, q).await.map_err(internal)?;
    }
    if let (Some(root), None, None) = (reply_to, body.answers, body.task_id) {
        let agent = load(&state, id).await?;
        let said = crate::routes::quick::Said { text, attachments: &attachments, spoken: body.spoken, quote: quote.as_ref() };
        if let Some(msg) = crate::routes::quick::reply_in_thread(&state, &viewer, &agent, root, &said).await? {
            return Ok((StatusCode::CREATED, Json(msg)));
        }
    }
    let files = inline_files(&state, viewer.org, &attachments).await?;
    let links = visible_links(&state, &viewer, text).await?;
    let options = body.options.map(|o| json!(o)).unwrap_or(Value::Null);
    let author = state
        .world
        .user_display_name(user.0)
        .await
        .map_err(internal)?
        .unwrap_or_else(|| "someone".to_string());
    let thread = match reply_to {
        Some(root) => state.world.agent_thread_transcript(id, root, 12).await.map_err(internal)?,
        None => String::new(),
    };
    if let Some(ask) = body.answers {
        let origin = clarification_origin;
        let card_task = clarification_task;
        let context_task = clarification_context;
        let resumed = origin.as_ref().is_some_and(loka_shared::world::main_clarification_resumes);
        let mut tx = state.world.pool().begin().await.map_err(|e| internal(e.into()))?;
        if let Some(task_id) = context_task {
            let valid = sqlx::query_scalar::<_, bool>(
                "SELECT TRUE FROM agent_tasks
                 WHERE id = $1 AND status = 'done' AND assigned_agent_id = $2 AND org_id IS NOT DISTINCT FROM $3
                 FOR UPDATE",
            )
            .bind(task_id)
            .bind(id)
            .bind(viewer.org)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| internal(e.into()))?
            .unwrap_or(false);
            if !valid {
                return Err(StatusCode::CONFLICT);
            }
        }
        let clarification_status = if let Some(task_id) = card_task {
            let task = sqlx::query_as::<_, (String, Option<Uuid>, Option<Uuid>)>(
                "SELECT status, assigned_agent_id, org_id FROM agent_tasks WHERE id = $1 FOR NO KEY UPDATE",
            )
            .bind(task_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| internal(e.into()))?;
            match task {
                Some((status, assigned, org))
                    if org == viewer.org && loka_shared::world::main_clarification_open(&status, assigned, id, resumed) =>
                {
                    Some(status)
                }
                _ => return Err(StatusCode::CONFLICT),
            }
        } else {
            None
        };
        let card = sqlx::query_as::<_, (String, Option<Value>)>(&format!(
            "SELECT body, meta->'clarify_request' FROM agent_messages m WHERE m.id = $1 AND m.agent_id = $2 AND {} FOR UPDATE",
            loka_shared::guests::thread_filter("$2", "$3")
        ))
        .bind(ask)
        .bind(id)
        .bind(user.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| internal(e.into()))?;
        if let Some((question, Some(origin @ Value::Object(_)))) = card {
            let locked_task = loka_shared::world::main_clarification_task(&origin);
            let locked_context = loka_shared::world::main_clarification_context_task(&origin);
            if locked_task != card_task
                || locked_context != context_task
                || loka_shared::world::main_clarification_resumes(&origin) != resumed
            {
                return Err(StatusCode::CONFLICT);
            }
            if let Some(task_id) = card_task {
                let current = loka_shared::world::quest_clarification_with(&mut tx, task_id, id, user.0)
                    .await
                    .map_err(internal)?;
                if current.as_ref().map(|card| card.id) != Some(ask) {
                    return Err(StatusCode::CONFLICT);
                }
            }
            let answered_task = if context_task.is_some() { None } else { card_task.or(body.task_id) };
            let answered = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM agent_messages WHERE agent_id = $1 AND meta->>'answers' = $2)",
            )
            .bind(id)
            .bind(ask.to_string())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| internal(e.into()))?;
            if answered {
                return Err(StatusCode::CONFLICT);
            }
            let source_resources = clarification_source_resources(
                &mut tx,
                &origin,
                id,
                user.0,
                viewer.org,
                card_task.or(context_task),
            )
                .await
                .map_err(|e| internal(e.into()))?;
            let canonical_source = source_resources.get("body").is_some();
            if origin.get("clarification_channel").is_some() && !canonical_source {
                return Err(StatusCode::CONFLICT);
            }
            let clarification_channel = source_resources
                .get("clarification_channel")
                .filter(|route| route.is_object())
                .cloned();
            let mut visible_origin = origin.clone();
            if let Some(fields) = visible_origin.as_object_mut() {
                fields.remove("clarification_channel");
            }
            let question: String = question.chars().take(1200).collect();
            let source_quote = loka_shared::quotes::render(&source_resources);
            let source_quote = source_quote.trim_end();
            let prior_handoff = source_resources.get("clarification_handoff").and_then(Value::as_object).and_then(|handoff| {
                Some((
                    handoff.get("request")?.as_str()?,
                    handoff.get("question")?.as_str()?,
                    handoff.get("answer")?.as_str()?,
                ))
            });
            let handoff_request = if let Some((request, prior_question, prior_answer)) = prior_handoff {
                let prior_answer = [prior_answer, source_quote]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                format!("{request}\n\nQuestion:\n{prior_question}\n\nTheir answer:\n{prior_answer}")
            } else {
                let request = source_resources
                    .get("body")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|body| !body.is_empty())
                    .or_else(|| origin.get("request").and_then(Value::as_str))
                    .unwrap_or_default();
                let source_thread = loka_shared::replies::reply_note(&source_resources);
                [request, source_quote, source_thread.trim()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            let clarification_handoff = json!({ "request": handoff_request, "question": question, "answer": text });
            let continuation_attachments = merge_resource_list(
                &attachments,
                &source_resources,
                "attachments",
                "id",
                loka_shared::media::MAX_PER_CALL * 2,
            );
            let continuation_files = merge_resource_list(
                &files,
                &source_resources,
                "files",
                "id",
                loka_shared::media::MAX_ATTACHED_FILES * 2,
            );
            let continuation_links = merge_resource_list(&links, &source_resources, "links", "ref", 16);
            let private_continuation = card_task.is_none()
                && canonical_source
                && origin.get("onboarding").and_then(Value::as_bool) != Some(true);
            let msg = state
                .world
                .insert_reply_with(
                    &mut *tx,
                    NewMessage {
                        agent_id: id,
                        author_kind: "user",
                        author_user_id: Some(user.0),
                        author_agent_id: None,
                        body: text,
                        session_id: None,
                        run_id: None,
                        meta: {
                            let mut meta = json!({ "attachments": attachments, "options": options, "answers": ask });
                            if body.spoken {
                                meta["spoken"] = json!(true);
                            }
                            if let Some(task) = answered_task {
                                meta["task_id"] = json!(task);
                            }
                            if let Some(task) = context_task {
                                meta["context_task_id"] = json!(task);
                            }
                            if let Some(quote) = &quote {
                                meta["quote"] = quote.clone();
                            }
                            meta
                        },
                    },
                    reply_to,
                )
                .await
                .map_err(internal)?;
            let event = NewEvent::new("message.created")
                .agent(id)
                .actor(Some(user.0))
                .task(answered_task.or(context_task))
                .payload(json!({
                    "message_id": msg.id,
                    "author": author,
                    "author_kind": "user",
                    "body": text,
                    "attachments": attachments,
                    "files": files,
                    "links": links,
                    "options": options,
                    "answers": question,
                    "clarify_origin": visible_origin,
                    "spoken": body.spoken,
                    "quote": quote,
                    "reply_to": reply_to,
                    "thread": thread,
                    "answer_card": ask,
                }));
            let seq = loka_shared::world::emit_in(
                &mut tx,
                &event,
                (card_task.is_none() && !private_continuation).then_some((id, "message")),
            )
            .await
            .map_err(internal)?;
            if private_continuation {
                let mut continuation = json!({
                    "message_id": msg.id,
                    "author": author,
                    "author_kind": "user",
                    "body": text,
                    "task_id": card_task,
                    "attachments": continuation_attachments,
                    "files": continuation_files,
                    "links": continuation_links,
                    "options": options,
                    "answers": question,
                    "clarify_origin": origin,
                    "clarification_continuation": ask,
                    "clarification_handoff": clarification_handoff,
                    "spoken": body.spoken,
                    "quote": quote,
                    "reply_to": reply_to,
                    "thread": thread,
                    "answer_card": ask,
                });
                if let Some(route) = clarification_channel {
                    continuation["clarification_channel"] = route;
                }
                sqlx::query(
                    "INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
                     VALUES ($1, 'message', $2, $3)",
                )
                .bind(id)
                .bind(seq)
                .bind(continuation)
                .execute(&mut *tx)
                .await
                .map_err(|e| internal(e.into()))?;
            }
            if let (Some(task_id), Some(status)) = (card_task, clarification_status) {
                let brief = format!("Original request:\n{handoff_request}\n\nQuestion:\n{question}\n\nTheir answer:\n{text}");
                sqlx::query(
                    "INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
                     VALUES ($1, 'task.assigned', $2, $3)",
                )
                .bind(id)
                .bind(seq)
                .bind(json!({
                    "task_id": task_id,
                    "title": origin.get("title").and_then(Value::as_str).unwrap_or_default(),
                    "brief": brief,
                    "resume": true,
                    "clarification_continuation": ask,
                    "clarification_handoff": clarification_handoff,
                    "clarification_status": status,
                    "clarification_resume": resumed,
                    "attachments": continuation_attachments,
                    "files": continuation_files,
                    "links": continuation_links,
                    "quote": quote,
                }))
                .execute(&mut *tx)
                .await
                .map_err(|e| internal(e.into()))?;
                loka_shared::world::settle_task_clarification_siblings(
                    &mut tx,
                    task_id,
                    id,
                    ask,
                    None,
                    "Answered another clarification",
                )
                .await
                .map_err(internal)?;
                loka_shared::world::adopt_saved_clarifications_into_queued_run(&mut tx, id, task_id)
                    .await
                    .map_err(internal)?;
            }
            tx.commit().await.map_err(|e| internal(e.into()))?;
            return Ok((StatusCode::CREATED, Json(msg)));
        }
        if origin.is_some_and(|origin| origin.is_object()) {
            return Err(StatusCode::CONFLICT);
        }
    }
    let question = match body.answers {
        Some(ask) => sqlx::query_as::<_, (String, Option<Uuid>)>(&format!(
            "SELECT body, NULLIF(meta->>'task_id', '')::uuid FROM agent_messages m WHERE m.id = $1 AND m.agent_id = $2 AND {}",
            loka_shared::guests::thread_filter("$2", "$3")
        ))
        .bind(ask)
        .bind(id)
        .bind(user.0)
        .fetch_optional(state.world.pool())
        .await
        .map_err(|e| internal(e.into()))?
        .map(|(body, task)| (body.chars().take(1200).collect::<String>(), task)),
        None => None,
    };
    let answered_task = body.task_id.or_else(|| changes.as_ref().and_then(|question| question.task_id)).or_else(|| question.as_ref().and_then(|(_, task)| *task));
    let setup_card = match text.is_empty() {
        true => None,
        false => crate::routes::setup_scan::card_kind(&state, id, body.answers).await?,
    };
    let quest_card = match body.answers {
        Some(card) if !text.is_empty() && setup_card.is_none() => state.world.quest_card(id, card).await.map_err(internal)?,
        _ => None,
    };
    let apps_card = quest_card.is_some();
    let clarification = match body.answers {
        Some(ask) if (!text.is_empty() || !body.attachments.is_empty()) && setup_card.is_none() && !apps_card => {
            let mut meta = json!({
                "attachments": attachments.clone(),
                "files": files.clone(),
                "links": links.clone(),
                "options": options.clone(),
                "answers": ask,
            });
            if body.spoken {
                meta["spoken"] = json!(true);
            }
            if let Some(task) = answered_task {
                meta["task_id"] = json!(task);
            }
            if let Some(quote) = &quote {
                meta["quote"] = quote.clone();
            }
            let event_payload = json!({
                "author": author,
                "author_kind": "user",
                "body": text,
                "attachments": attachments.clone(),
                "files": files.clone(),
                "links": links.clone(),
                "options": options.clone(),
                "answers": question.as_ref().map(|(body, _)| body),
                "spoken": body.spoken,
                "quote": quote.clone(),
                "reply_to": reply_to,
                "thread": thread,
                "answer_card": body.answers,
            });
            let (answer, message) = state
                .world
                .answer_clarification(
                    id,
                    ask,
                    ClarificationReply {
                        user_id: user.0,
                        author: &author,
                        text,
                        meta,
                        reply_to,
                        event_payload,
                    },
                )
                .await
                .map_err(internal)?;
            if let (ClarificationAnswer::Answered { .. }, Some(message)) = (&answer, message) {
                return Ok((StatusCode::CREATED, Json(message)));
            }
            answer
        }
        _ => ClarificationAnswer::NotAClarification,
    };
    if clarification == ClarificationAnswer::Stale {
        return Err(StatusCode::CONFLICT);
    }
    let clarification_answered = !matches!(clarification, ClarificationAnswer::NotAClarification);
    let plan = match body.answers {
        Some(ask) if !text.is_empty() && setup_card.is_none() && !apps_card && !clarification_answered => {
            Some(state.world.answer_plan(id, ask, text, &author).await.map_err(internal)?)
        }
        _ => None,
    };
    let plan_answered = matches!(plan, Some(PlanAnswer::Approved { .. } | PlanAnswer::Redirected { .. }));
    let plan_undecided = matches!(plan, Some(PlanAnswer::Undecided));
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
                if let Some(ask) = body.answers.filter(|_| !plan_undecided) {
                    meta["answers"] = json!(ask);
                }
                if body.spoken {
                    meta["spoken"] = json!(true);
                }
                if let Some(task) = answered_task {
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
                .task(answered_task)
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
                    "answers": question.as_ref().map(|(body, _)| body),
                    "spoken": body.spoken,
                    "quote": quote,
                    "reply_to": reply_to,
                    "thread": thread,
                    "answer_card": body.answers,
                    "task_id": body.task_id,
                })),
            (!onboarding && !plan_answered && !clarification_answered && setup_card.is_none() && !apps_card).then_some((id, "message")),
        )
        .await
        .map_err(internal)?;
    if onboarding {
        crate::routes::onboarding::answer(&state, &user, id, text).await?;
    }
    if let (Some(kind), Some(card)) = (quest_card, body.answers) {
        state.world.answer_quest_card(id, card, kind, text, &author).await.map_err(internal)?;
    }
    if let Some(question) = changes {
        let sources = quote.as_ref().and_then(|q| q.get("source")).and_then(Value::as_str).map(|s| vec![s.to_string()]).unwrap_or_default();
        state.world.change_decision(question.question_id, user.0, &author, text, sources).await.map_err(internal)?;
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

pub async fn get_schedule(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    let schedule = state.world.schedule_by_id(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let agent = state
        .world
        .get_agent(schedule.agent_id)
        .await
        .map_err(internal)?
        .filter(|agent| access::can_manage(&viewer, agent))
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "schedule": schedule, "agent": { "id": agent.id, "name": agent.name } })))
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
    let room_ids: Vec<Uuid> = events.iter().filter_map(loka_shared::rooms::scoped_room).collect();
    let part_ids: Vec<Uuid> = events.iter().filter_map(loka_shared::rooms::scoped_part).collect();
    let open_rooms = state.world.open_rooms(&room_ids, &viewer).await.map_err(internal)?;
    let open_parts = state.world.open_tasks(&part_ids, &viewer).await.map_err(internal)?;
    Ok(Json(
        events
            .into_iter()
            .filter_map(|event| {
                visible_event(event, user.0, &hidden, &owned, viewer.role == Role::Admin, &channels, &open_rooms, &open_parts)
            })
            .collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn visible_event(
    event: WorldEvent,
    viewer: Uuid,
    hidden: &[Uuid],
    owned: &HashSet<Uuid>,
    admin: bool,
    channels: &HashSet<Uuid>,
    open_rooms: &HashSet<Uuid>,
    open_parts: &HashSet<Uuid>,
) -> Option<WorldEvent> {
    let room = loka_shared::rooms::scoped_room(&event);
    let part = loka_shared::rooms::scoped_part(&event);
    let hidden_subject = event.agent_id.is_some_and(|agent| hidden.contains(&agent));
    let routes_card = loka_shared::event_scope::audience_scoped_card_invalidation(&event)
        && event.quest_scope.is_some()
        && loka_shared::event_scope::quest_scope_allows(&event, viewer);
    if (hidden_subject && room.is_none() && !routes_card && !loka_shared::event_scope::creator_can_refresh(&event, viewer))
        || room.is_some_and(|room| !open_rooms.contains(&room))
        || part.is_some_and(|part| !open_parts.contains(&part))
        || loka_shared::slack::hidden_from(&event, viewer)
        || loka_shared::loka::hidden_from(&event, viewer)
        || loka_shared::shell::hidden_from(&event, owned)
        || (room.is_none() && loka_shared::event_scope::private_from_admin(&event, viewer, owned, admin))
        || event.in_channel(channels)
    {
        return None;
    }
    Some(loka_shared::event_scope::card_invalidation_for_viewer(event, viewer, hidden_subject, admin))
}

fn last_seen(after: Option<i64>, headers: &axum::http::HeaderMap) -> Option<i64> {
    let header: Option<i64> = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    match (after, header) {
        (Some(query), Some(header)) => Some(query.max(header)),
        (query, header) => query.or(header),
    }
}

fn wire_event(event: &loka_shared::world::WorldEvent, replay_until: Option<i64>) -> Option<String> {
    let mut value = serde_json::to_value(event).ok()?;
    if replay_until.is_some_and(|head| event.seq <= head) {
        value["replayed"] = json!(true);
    }
    serde_json::to_string(&value).ok()
}

pub async fn stream(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(scope): Extension<Viewer>,
    Query(q): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    let viewer = user.0;
    let org = scope.org();
    let mut resync = None;
    let mut replay_until = None;
    let mut cursor = match last_seen(q.after, &headers) {
        Some(after) => match state.world.replay_gap(org, after, loka_shared::world::REPLAY_MAX_EVENTS).await.map_err(internal)?.resume_from(after) {
            loka_shared::world::Resume::Replay(from) => {
                replay_until = state.world.recent_events(None, 1).await.map_err(internal)?.last().map(|e| e.seq);
                from
            }
            loka_shared::world::Resume::Resync(head) => {
                resync = Some(head);
                head
            }
        },
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
    let mut signals = state.signal_tx.subscribe();
    let store = state.world.clone();
    let stream = async_stream::stream! {
        let mut member_checked = tokio::time::Instant::now();
        let _open = crate::metrics::Open::new("world");
        if let Some(head) = resync {
            yield Ok::<Event, Infallible>(Event::default().event("resync").data(json!({ "head": head }).to_string()));
        }
        loop {
            match store.events_since_in(org, cursor, 500).await {
                Ok(events) if events.is_empty() => {}
                Ok(events) => {
                    let current = match store.role_in_org(viewer, Some(org)).await {
                        Ok(Some(role)) => {
                            member_checked = tokio::time::Instant::now();
                            Viewer { role: role.ladder(), org_role: Some(role), ..scope.clone() }
                        }
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
                    let room_ids: Vec<Uuid> = events.iter().filter_map(loka_shared::rooms::scoped_room).collect();
                    let part_ids: Vec<Uuid> = events.iter().filter_map(loka_shared::rooms::scoped_part).collect();
                    let open_rooms = match store.open_rooms(&room_ids, &current).await {
                        Ok(rooms) => rooms,
                        Err(e) => {
                            warn!(error = %e, "world stream room scope failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    let open_parts = match store.open_tasks(&part_ids, &current).await {
                        Ok(tasks) => tasks,
                        Err(e) => {
                            warn!(error = %e, "world stream part scope failed");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };
                    for event in events {
                        cursor = event.seq;
                        let Some(event) = visible_event(
                            event,
                            viewer,
                            &hidden,
                            &owned,
                            current.role == Role::Admin,
                            &channels,
                            &open_rooms,
                            &open_parts,
                        ) else {
                            continue;
                        };
                        if let Some(data) = wire_event(&event, replay_until) {
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
                let next = tokio::select! {
                    woke = tokio::time::timeout_at(deadline, rx.recv()) => match woke {
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => Waited::Closed,
                        Ok(Ok(wake)) if !wake.concerns(org) => continue,
                        _ => Waited::Woke,
                    },
                    signal = signals.recv() => match signal {
                        Ok(event) if loka_shared::signals::concerns(&event, org) => Waited::Signal(event),
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => Waited::Closed,
                    },
                };
                match next {
                    Waited::Closed => break true,
                    Waited::Woke => break false,
                    Waited::Signal(event) => match live_signal(&store, &event, viewer, org, &scope, &mut member_checked).await {
                        Live::Send(data) => yield Ok::<Event, Infallible>(Event::default().event("world").data(data)),
                        Live::Skip => {}
                        Live::Gone => break true,
                    },
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

enum Waited {
    Closed,
    Woke,
    Signal(std::sync::Arc<WorldEvent>),
}

enum Live {
    Send(String),
    Skip,
    Gone,
}

const MEMBER_RECHECK: Duration = Duration::from_secs(10);

async fn live_signal(
    store: &loka_shared::world::WorldStore,
    event: &WorldEvent,
    viewer: Uuid,
    org: Uuid,
    scope: &Viewer,
    member_checked: &mut tokio::time::Instant,
) -> Live {
    let scoped = loka_shared::signals::needs_scope(event);
    let mut current = None;
    let mut send = event.clone();
    if scoped || member_checked.elapsed() > MEMBER_RECHECK {
        match store.role_in_org(viewer, Some(org)).await {
            Ok(Some(role)) => {
                *member_checked = tokio::time::Instant::now();
                current = Some(Viewer { role: role.ladder(), org_role: Some(role), ..scope.clone() });
            }
            Ok(None) => return Live::Gone,
            Err(_) => return Live::Skip,
        }
    }
    if loka_shared::slack::hidden_from(event, viewer) || loka_shared::loka::hidden_from(event, viewer) {
        return Live::Skip;
    }
    if let (true, Some(current)) = (scoped, current) {
        let (Ok(hidden), Ok(channels), Ok(owned)) = (
            store.hidden_agents(&current).await,
            store.hidden_channels_among(Some(viewer), None, std::slice::from_ref(event)).await,
            store.owned_agent_ids(viewer).await,
        ) else {
            return Live::Skip;
        };
        let rooms: Vec<Uuid> = loka_shared::rooms::scoped_room(event).into_iter().collect();
        let parts: Vec<Uuid> = loka_shared::rooms::scoped_part(event).into_iter().collect();
        let (Ok(open_rooms), Ok(open_parts)) = (store.open_rooms(&rooms, &current).await, store.open_tasks(&parts, &current).await) else {
            return Live::Skip;
        };
        let Some(visible) = visible_event(
            event.clone(),
            viewer,
            &hidden,
            &owned,
            current.role == Role::Admin,
            &channels,
            &open_rooms,
            &open_parts,
        ) else {
            return Live::Skip;
        };
        send = visible;
    }
    serde_json::to_string(&send).map(Live::Send).unwrap_or(Live::Skip)
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
    github_repos: Option<Vec<String>>,
    #[serde(default)]
    model: Option<Value>,
}

pub const RULE_REPOS_ONLY_ON_APPROVED: &str = "Only approved actions can name repositories for pull request actions.";

pub async fn update_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateRule>,
) -> Result<Json<WorldRule>, (StatusCode, Json<Value>)> {
    let refuse = |status: StatusCode, error: &str| (status, Json(json!({ "error": error })));
    let failed = |error: anyhow::Error| (internal(error), Json(json!({})));
    if body.scope.as_deref().is_some_and(|s| !valid_scope(s)) || body.body.as_deref().is_some_and(|b| b.trim().is_empty()) {
        return Err(refuse(StatusCode::BAD_REQUEST, "A rule needs some text and a known scope."));
    }
    if let Some(Err(error)) = body.github_repos.as_deref().map(loka_shared::pr_actions::clean_repos) {
        return Err(refuse(StatusCode::BAD_REQUEST, &error.to_string()));
    }
    let pin = match body.model.as_ref().map(loka_shared::variety::rule_model).transpose() {
        Ok(pin) => pin,
        Err(error) => return Err(refuse(StatusCode::BAD_REQUEST, &error)),
    };
    let global_org = viewer.org.filter(|_| role == Role::Admin);
    let rule = state
        .world
        .update_rule(id, user.0, global_org, body.scope.as_deref(), body.body.as_deref().map(str::trim), body.enabled)
        .await
        .map_err(failed)?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, "That rule is gone."))?;
    let rule = match pin {
        Some(pin) => state
            .world
            .set_rule_model(rule.id, user.0, global_org, pin.as_ref())
            .await
            .map_err(failed)?
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "Approved actions cannot pin a model."))?,
        None => rule,
    };
    let Some(repos) = body.github_repos.as_deref() else {
        return Ok(Json(rule));
    };
    state
        .world
        .set_rule_repos(rule.id, user.0, global_org, repos)
        .await
        .map_err(failed)?
        .map(Json)
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, RULE_REPOS_ONLY_ON_APPROVED))
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
    id: Option<Uuid>,
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
    let rows = state.world.session_briefs(q.agent_id, q.id, limit).await.map_err(internal)?;
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
    let stepping = body.mode.as_deref() == Some("step");
    let withdrawing = body.mode.as_deref() == Some("withdraw") || run.status == "queued" && !stepping;
    if run.kind == "turn" || (withdrawing && run.status != "queued") || (!withdrawing && run.status != "running") {
        return Err(StatusCode::CONFLICT);
    }
    let name = state.world.user_display_name(user.0).await.map_err(internal)?.unwrap_or_else(|| "someone".into());
    if withdrawing {
        let withdrawn = loka_agent::runner_server::RunnerCoordinator::new(state.db.clone(), state.world.clone())
            .withdraw_queued_run(id, &name)
            .await
            .map_err(internal)?;
        if !withdrawn {
            return Err(StatusCode::CONFLICT);
        }
        return Ok((StatusCode::ACCEPTED, Json(json!({ "mode": "withdraw", "withdrawn": true }))));
    }
    if stepping {
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
    Ok((StatusCode::ACCEPTED, Json(json!({ "mode": "run", "withdrawn": false }))))
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
) -> axum::response::Response {
    if body.text.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let session = match state.world.get_session(id).await {
        Ok(Some(session)) => session,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error).into_response(),
    };
    let name = match state.world.user_display_name(user.0).await {
        Ok(name) => name.unwrap_or_else(|| "someone".into()),
        Err(error) => return internal(error).into_response(),
    };
    match state.world.start_followup(&session, body.text.trim(), &name, body.interrupt).await {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(error) => refusal(error, "follow-up"),
    }
}

fn refusal(error: anyhow::Error, what: &str) -> axum::response::Response {
    if error.downcast_ref::<sqlx::Error>().is_some() {
        return internal(error).into_response();
    }
    warn!(error = %error, "{what} refused");
    (StatusCode::CONFLICT, Json(json!({ "error": error.to_string() }))).into_response()
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
) -> Result<axum::response::Response, StatusCode> {
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
    match state
        .world
        .request_review(&session, reviewer.as_ref(), choice.as_ref(), body.focus.as_deref(), session.agent_id)
        .await
    {
        Ok(run) => Ok((StatusCode::ACCEPTED, Json(run)).into_response()),
        Err(error) => Ok(refusal(error, "review")),
    }
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
    Ok(Json(state.world.channel_thread_with_reply(id, root, loka_shared::channels::THREAD_LIMIT, Some(message_id)).await.map_err(internal)?))
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
                if let Some(mut quote) = state.world.clean_quote(&viewer, body.quote.as_ref()).await.map_err(internal)? {
                    if let Some(org) = viewer.org {
                        state.world.expand_canvas_quote(user.0, org, &mut quote, loka_shared::canvas::QuoteCards::RefsOnly).await.map_err(internal)?;
                        state.world.expand_practice_quote(user.0, loka_shared::access::Role::Write, org, &mut quote).await.map_err(internal)?;
                    }
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
    before_big_work: loka_shared::world::BeforeBigWork,
    new_art: bool,
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

fn me_view((id, name, email, main_agent_id, status_emoji, status_text, before_big_work): loka_shared::world::MeRow, role: Role) -> Me {
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
        before_big_work: loka_shared::world::BeforeBigWork::parse(&before_big_work).unwrap_or_default(),
        new_art: false,
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
    me.new_art = state.world.new_art(user.0).await.map_err(internal)?;
    me.onboarding = role >= Role::Write && state.world.onboarding_pending(user.0).await.map_err(internal)?;
    Ok(Json(me))
}

#[derive(Deserialize)]
pub struct SetMe {
    name: Option<String>,
    main_agent_id: Option<Uuid>,
    status: Option<SetStatus>,
    mention_wakes_main: Option<bool>,
    before_big_work: Option<loka_shared::world::BeforeBigWork>,
    new_art: Option<bool>,
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
    if let Some(value) = body.before_big_work {
        state.world.set_before_big_work(user.0, value).await.map_err(internal)?;
    }
    if let Some(on) = body.new_art {
        state.world.set_new_art(user.0, on).await.map_err(internal)?;
    }
    let row = state.world.me(user.0).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let mut me = me_view(row, role);
    with_org(&state, &mut me, &viewer).await?;
    me.mention_wakes_main = state.world.mention_wakes_main(user.0).await.map_err(internal)?;
    me.new_art = state.world.new_art(user.0).await.map_err(internal)?;
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
    let mut decisions = state.world.decision_summaries_for(&ids, &viewer).await.map_err(internal)?;
    for task in &mut tasks {
        let (mine, rest): (Vec<_>, Vec<_>) = milestones.into_iter().partition(|m| m.task_id == task.id);
        task.milestones = mine;
        milestones = rest;
        task.decisions = decisions.remove(&task.id);
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
    focus: Option<Uuid>,
}

#[derive(Deserialize, Default)]
pub struct ResumeBody {
    #[serde(default)]
    text: Option<String>,
}

pub async fn quest_resume(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    body: Option<Json<ResumeBody>>,
) -> axum::response::Response {
    let task = match quest_task(&state, &viewer, task_id).await {
        Ok(task) => task,
        Err(status) => return status.into_response(),
    };
    let agent = match task.assigned_agent_id {
        Some(id) => match state.world.get_agent(id).await {
            Ok(agent) => agent,
            Err(error) => return internal(error).into_response(),
        },
        None => None,
    };
    if !agent.as_ref().is_some_and(|agent| access::can_manage(&viewer, agent)) && viewer.role != Role::Admin {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "Only people who can manage this quest's agent can resume it." }))).into_response();
    }
    let name = match state.world.user_display_name(user.0).await {
        Ok(name) => name.unwrap_or_else(|| "someone".into()),
        Err(error) => return internal(error).into_response(),
    };
    let note = body.and_then(|Json(body)| body.text);
    match state.world.request_resume(task.id, user.0, &name, note.as_deref()).await {
        Ok(outcome) => (StatusCode::ACCEPTED, Json(outcome)).into_response(),
        Err(loka_shared::quests::ResumeRefusal::Refused(reason)) => {
            warn!(task = %task.id, reason = %reason, "resume refused");
            (StatusCode::CONFLICT, Json(json!({ "error": reason }))).into_response()
        }
        Err(loka_shared::quests::ResumeRefusal::Internal(error)) => internal(error).into_response(),
    }
}

pub async fn quest_room(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(task_id): Path<Uuid>,
    Query(q): Query<RoomQuery>,
) -> Result<Json<Value>, StatusCode> {
    let (task, requested_member_only) = crate::routes::rooms::room_access(&state, &viewer, task_id).await?;
    let canonical = state.world.room_of(task_id).await.map_err(internal)?;
    let (room_task, member_only) = if canonical == task_id {
        (task.clone(), requested_member_only)
    } else {
        let (room_task, canonical_member_only) = crate::routes::rooms::room_access(&state, &viewer, canonical).await?;
        (room_task, requested_member_only || canonical_member_only)
    };
    let agents = state.world.list_agents_in(viewer.org).await.map_err(internal)?;
    let usable: Vec<Uuid> = agents.iter().filter(|a| access::can_use(&viewer, a)).map(|a| a.id).collect();
    let since = room_task.created_at.unwrap_or_else(Utc::now) - chrono::Duration::minutes(5);
    let focus = match q.focus {
        Some(focus) if state.world.sees_message_as(focus, &viewer).await.map_err(internal)? => Some(focus),
        _ => None,
    };
    let mut room = state.world.quest_room(canonical, since, &usable, user.0, q.after, q.since, focus).await.map_err(internal)?;
    let mut message_scope = room.messages.clone();
    message_scope.extend(room.needs_you_messages.iter().cloned());
    let open_messages = state.world.open_room_messages(&message_scope, &viewer).await.map_err(internal)?;
    room.messages.retain(|message| open_messages.contains(&message.id));
    room.needs_you_messages.retain(|message| open_messages.contains(&message.id));
    room.decisions = state.world.decision_summary_for(canonical, &viewer).await.map_err(internal)?;
    room.members = state.world.room_members_for(canonical, &usable).await.map_err(internal)?;
    let mut room_parts = state.world.quest_parts(canonical).await.map_err(internal)?;
    if viewer.role != Role::Admin && room_task.user_id != viewer.user {
        room_parts.retain(|part| part.assigned_agent_id.is_some_and(|agent| usable.contains(&agent)));
    }
    let visible_parts: std::collections::HashSet<Uuid> = room_parts.iter().map(|part| part.id).collect();
    if member_only {
        room.sessions.clear();
        room.runs.clear();
        room.events.clear();
    }
    let owned = state.world.owned_agent_ids(user.0).await.map_err(internal)?;
    room.events.retain(|e| {
        let scoped = loka_shared::rooms::scoped_event(e);
        let part = loka_shared::rooms::event_part(e, canonical);
        !loka_shared::slack::hidden_from(e, user.0)
            && !loka_shared::loka::hidden_from(e, user.0)
            && !loka_shared::shell::hidden_from(e, &owned)
            && part.is_none_or(|part| visible_parts.contains(&part))
            && (scoped || !loka_shared::event_scope::private_from_admin(e, user.0, &owned, viewer.role == Role::Admin))
    });
    let milestones = MilestoneStore::new(state.world.pool().clone()).for_task(task.id).await.map_err(internal)?;
    let options = state.world.quest_options(task.id).await.map_err(internal)?;
    let parent = state.world.quest_parent(task.id).await.map_err(internal)?;
    let plan_waiting = state.world.plan_waiting(task.id).await.map_err(internal)?;
    let mut parts = if task.id == canonical { room_parts } else { state.world.quest_parts(task.id).await.map_err(internal)? };
    if task.id != canonical && viewer.role != Role::Admin && task.user_id != viewer.user {
        parts.retain(|part| part.assigned_agent_id.is_some_and(|agent| usable.contains(&agent)));
    }
    let manages = |agent_id: Uuid| viewer.role == Role::Admin || agents.iter().any(|agent| agent.id == agent_id && access::can_manage(&viewer, agent));
    let resume = match task.assigned_agent_id {
        Some(agent_id) if parts.is_empty() && manages(agent_id) => state.world.resume_preview(agent_id, task.id).await.ok().map(|plan| plan.view()),
        _ => None,
    };
    let clarification = match task.assigned_agent_id {
        Some(agent_id) => match state.world.quest_clarification(task_id, agent_id, user.0).await.map_err(internal)? {
            Some(clarification) => Some(clarification),
            None => state
                .world
                .contextual_quest_clarification(task_id, agent_id, user.0)
                .await
                .map_err(internal)?,
        },
        None => None,
    };
    Ok(Json(json!({
        "task": {
            "resume": resume,
            "parts": parts,
            "parent": parent,
            "plan_first": options.plan_first,
            "plan_waiting": plan_waiting,
            "attempts": options.attempts,
            "attempt_variants": options.attempt_variants,
            "ci_feedback": options.ci_feedback,
            "pace": options.pace,
            "asking": options.asking,
            "crew_size": options.crew_size,
            "crew_variants": options.crew_variants,
            "id": task.id,
            "title": task.title,
            "description": task.description,
            "status": task.status,
            "repo": task.repo,
            "branch": task.branch,
            "pr_url": task.pr_url,
            "past_prs": task.past_prs.unwrap_or_else(|| json!([])),
            "assigned_agent_id": task.assigned_agent_id,
            "user_id": task.user_id,
            "result_content": task.result_content,
            "error_message": task.error_message,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
            "milestones": milestones,
        },
        "clarification": clarification,
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
    if let Some(task) = task.filter(|t| t.pr_url.is_some()) {
        if added.iter().any(|m| m.kind == "merge") {
            store.arm_merge(task.id).await.map_err(internal)?;
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
            if task.pr_url.is_some() {
                store.arm_merge(task.id).await.map_err(internal)?;
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
    let queued = MilestoneStore::new(state.world.pool().clone()).retry_check_now(task_id).await.map_err(internal)?;
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
pub struct NewListBody {
    title: String,
    #[serde(default)]
    items: Vec<String>,
}

pub async fn create_list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<NewListBody>,
) -> Result<(StatusCode, Json<loka_shared::items::WorldItem>), StatusCode> {
    if body.title.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let item = state
        .world
        .create_list(loka_shared::items::NewList {
            owner: user.0,
            creator: None,
            org: viewer.org(),
            title: &body.title,
            items: body.items,
            note: None,
            region: None,
        })
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(item)))
}

pub async fn get_item(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<Json<loka_shared::items::WorldItem>, StatusCode> {
    state
        .world
        .item_for_viewer(id, user.0, viewer.org(), viewer.role == Role::Admin)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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

#[cfg(test)]
mod disk_full_tests {
    use super::*;

    #[test]
    fn a_full_disk_answers_insufficient_storage() {
        let io = std::io::Error::from_raw_os_error(28);
        assert_eq!(internal(anyhow::Error::new(io).context("saving message")), StatusCode::INSUFFICIENT_STORAGE);
        let pg = anyhow::anyhow!("error returned from database: could not extend file \"base/16384/2619\": No space left on device");
        assert_eq!(internal(pg), StatusCode::INSUFFICIENT_STORAGE);
    }

    #[test]
    fn other_failures_stay_internal() {
        assert_eq!(internal(anyhow::anyhow!("pool timed out while waiting for an open connection")), StatusCode::INTERNAL_SERVER_ERROR);
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert_eq!(internal(anyhow::Error::new(io)), StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[cfg(test)]
mod stream_resume_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};
    use futures_util::{Stream, StreamExt};

    async fn state(url: &str) -> anyhow::Result<AppState> {
        let db = loka_shared::db::Database::connect(url).await?;
        let pool = db.pool().clone();
        Ok(AppState {
            db,
            config: crate::config::ApiConfig { repos: vec![], backends: indexmap::IndexMap::new(), skills: vec![], mcp_servers: vec![] },
            world: loka_shared::world::WorldStore::new(pool.clone()),
            runners: loka_shared::runners::RunnerStore::new(pool),
            world_tx: tokio::sync::broadcast::channel(8).0,
            signal_tx: tokio::sync::broadcast::channel(8).0,
            runner_tx: crate::runner_wakes::RunnerWakes::default(),
            term: crate::terminal::TermHub::new(),
        })
    }

    async fn next_task_event<S, E>(body: &mut S, task: Uuid) -> anyhow::Result<Value>
    where
        S: Stream<Item = Result<axum::body::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        while let Some(chunk) = body.next().await {
            let text = String::from_utf8(chunk.map_err(|error| anyhow::anyhow!(error.to_string()))?.to_vec())?;
            for block in text.split("\n\n") {
                let Some(data) = block.lines().find_map(|line| line.strip_prefix("data: ")) else {
                    continue;
                };
                let event: Value = serde_json::from_str(data)?;
                if event["type"].as_str().is_some_and(|kind| kind.starts_with("task.")) && event["payload"]["task_id"] == json!(task) {
                    return Ok(event);
                }
            }
        }
        anyhow::bail!("world stream closed before the quest event")
    }

    async fn quest_visible(state: &AppState, viewer: &Viewer, task: Uuid) -> bool {
        list_quests(
            State(state.clone()),
            Extension(UserId(viewer.user)),
            Extension(viewer.role),
            Extension(viewer.clone()),
        )
        .await
        .is_ok_and(|Json(tasks)| tasks.iter().any(|candidate| candidate.id == task))
    }

    async fn room_status(state: &AppState, viewer: &Viewer, task: Uuid) -> Result<(), StatusCode> {
        quest_room(
            State(state.clone()),
            Extension(UserId(viewer.user)),
            Extension(viewer.clone()),
            Path(task),
            Query(RoomQuery { after: None, since: None, focus: None }),
        )
        .await
        .map(|_| ())
    }

    #[test]
    fn events_replayed_after_a_reconnect_say_so_and_live_ones_do_not() {
        let event = |seq: i64| loka_shared::world::WorldEvent {
            seq,
            event_type: "report.updated".into(),
            agent_id: None,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: None,
            payload: json!({ "report_id": "r" }),
            created_at: chrono::Utc::now(),
            org_id: None,
            quest_scope: None,
        };
        let read = |seq: i64, head: Option<i64>| serde_json::from_str::<Value>(&wire_event(&event(seq), head).unwrap()).unwrap();
        assert_eq!(read(5, Some(9))["replayed"], json!(true));
        assert_eq!(read(9, Some(9))["replayed"], json!(true));
        assert!(read(10, Some(9)).get("replayed").is_none());
        assert!(read(5, None).get("replayed").is_none());
        assert_eq!(read(5, None)["type"], json!("report.updated"));
    }

    #[test]
    fn scoped_card_changes_route_without_exposing_the_hidden_subject() {
        let viewer = Uuid::new_v4();
        let other = Uuid::new_v4();
        let agent = Uuid::new_v4();
        let task = Uuid::new_v4();
        let event = loka_shared::world::WorldEvent {
            seq: 1,
            event_type: "task.changed".into(),
            agent_id: Some(agent),
            session_id: None,
            run_id: None,
            task_id: Some(task),
            actor_user_id: Some(other),
            payload: json!({ "task_id": task, "quiet": true }),
            created_at: chrono::Utc::now(),
            org_id: None,
            quest_scope: Some(json!({ "delivery": { "audience": [viewer] } })),
        };
        let routed = visible_event(
            event.clone(),
            viewer,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        )
        .expect("routed card change");
        assert_eq!((routed.agent_id, routed.actor_user_id, routed.task_id), (None, None, Some(task)));
        assert_eq!(routed.payload, event.payload);
        assert!(visible_event(
            event.clone(),
            other,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        )
        .is_none());
        let mut ordinary = event;
        ordinary.quest_scope = None;
        assert!(visible_event(
            ordinary,
            viewer,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        )
        .is_none());
    }

    #[test]
    fn room_events_require_room_and_part_access() {
        let viewer = Uuid::new_v4();
        let agent = Uuid::new_v4();
        let room = Uuid::new_v4();
        let part = Uuid::new_v4();
        let event = loka_shared::world::WorldEvent {
            seq: 1,
            event_type: "question.asked".into(),
            agent_id: Some(agent),
            session_id: None,
            run_id: None,
            task_id: Some(room),
            actor_user_id: None,
            payload: json!({ "room": { "task_id": room }, "part_task_id": part }),
            created_at: chrono::Utc::now(),
            org_id: None,
            quest_scope: None,
        };
        let rooms = HashSet::from([room]);
        let parts = HashSet::from([part]);
        assert!(visible_event(
            event.clone(),
            viewer,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &rooms,
            &parts,
        )
        .is_some());
        assert!(visible_event(
            event.clone(),
            viewer,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &HashSet::new(),
            &parts,
        )
        .is_none());
        assert!(visible_event(
            event,
            viewer,
            &[agent],
            &HashSet::new(),
            false,
            &HashSet::new(),
            &rooms,
            &HashSet::new(),
        )
        .is_none());
    }

    #[test]
    fn the_newest_of_the_query_and_the_last_event_id_header_wins() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("41"));
        assert_eq!(last_seen(Some(7), &headers), Some(41));
        assert_eq!(last_seen(Some(90), &headers), Some(90));
        assert_eq!(last_seen(None, &headers), Some(41));
        assert_eq!(last_seen(Some(7), &HeaderMap::new()), Some(7));
    }

    #[test]
    fn a_missing_or_unreadable_last_event_id_starts_live() {
        let mut headers = HeaderMap::new();
        assert_eq!(last_seen(None, &headers), None);
        headers.insert("last-event-id", HeaderValue::from_static("abc"));
        assert_eq!(last_seen(None, &headers), None);
    }

    #[tokio::test]
    async fn visibility_changes_refresh_an_open_quest_stream_and_its_reads() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Visibility stream') RETURNING id")
            .bind(format!("visibility-stream-{}", &tag[..16]))
            .fetch_one(state.db.pool())
            .await?;
        let creator: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, active_org_id) VALUES ($1, 'Creator', $2) RETURNING id")
            .bind(format!("creator-{tag}@test.dev"))
            .bind(org)
            .fetch_one(state.db.pool())
            .await?;
        let viewer_user: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, active_org_id) VALUES ($1, 'Viewer', $2) RETURNING id")
            .bind(format!("viewer-{tag}@test.dev"))
            .bind(org)
            .fetch_one(state.db.pool())
            .await?;
        for user in [creator, viewer_user] {
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')")
                .bind(org)
                .bind(user)
                .execute(state.db.pool())
                .await?;
        }
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility, org_id)
             VALUES ($1, 'Visibility agent', 'claude', 'haiku', $2, 'public', $3) RETURNING id",
        )
        .bind(format!("visibility-agent-{tag}"))
        .bind(creator)
        .bind(org)
        .fetch_one(state.db.pool())
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Visibility quest', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(creator)
        .bind(agent)
        .bind(org)
        .fetch_one(state.db.pool())
        .await?;
        let viewer = Viewer {
            user: viewer_user,
            org: Some(org),
            role: Role::Write,
            org_role: Some(loka_shared::orgs::OrgRole::Member),
            instance_admin: false,
        };
        assert!(quest_visible(&state, &viewer, task).await);
        assert_eq!(room_status(&state, &viewer, task).await, Ok(()));

        let mut listener = sqlx::postgres::PgListener::connect(&url).await?;
        listener.listen(loka_shared::world::CHANNEL_EVENTS).await?;
        let world_tx = state.world_tx.clone();
        let relay = tokio::spawn(async move {
            while let Ok(notification) = listener.recv().await {
                let _ = world_tx.send(loka_shared::world::EventWake::from_payload(notification.payload()));
            }
        });
        let response = stream(
            State(state.clone()),
            Extension(UserId(viewer_user)),
            Extension(viewer.clone()),
            Query(EventsQuery { after: None, agent_id: None, limit: None }),
            HeaderMap::new(),
        )
        .await
        .expect("world stream")
        .into_response();
        let mut body = response.into_body().into_data_stream();
        assert!(tokio::time::timeout(Duration::from_secs(1), body.next()).await.is_err());

        state
            .world
            .update_agent(agent, &AgentUpdate { visibility: Some("private".into()), ..Default::default() })
            .await?
            .expect("agent");
        let hidden = tokio::time::timeout(Duration::from_secs(5), next_task_event(&mut body, task)).await??;
        assert_eq!(hidden["agent_id"], Value::Null);
        assert_eq!(hidden["actor_user_id"], Value::Null);
        assert_eq!(hidden["payload"], json!({ "task_id": task, "quiet": true }));
        assert!(!quest_visible(&state, &viewer, task).await);
        assert_eq!(room_status(&state, &viewer, task).await, Err(StatusCode::FORBIDDEN));

        state
            .world
            .update_agent(agent, &AgentUpdate { visibility: Some("public".into()), ..Default::default() })
            .await?
            .expect("agent");
        let restored = tokio::time::timeout(Duration::from_secs(5), next_task_event(&mut body, task)).await??;
        assert_eq!(restored["agent_id"], Value::Null);
        assert_eq!(restored["actor_user_id"], Value::Null);
        assert_eq!(restored["payload"], json!({ "task_id": task, "quiet": true }));
        assert!(quest_visible(&state, &viewer, task).await);
        assert_eq!(room_status(&state, &viewer, task).await, Ok(()));

        let delete_after: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM world_events")
            .fetch_one(state.db.pool())
            .await?;
        let mut delete = state.db.pool().begin().await?;
        sqlx::query("SELECT world_emit_task_deleted(t, $2) FROM agent_tasks t WHERE t.id = $1")
            .bind(task)
            .bind(json!({ "reason": "Removed after handoff" }))
            .execute(&mut *delete)
            .await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(&mut *delete).await?;
        delete.commit().await?;
        state
            .world
            .update_agent(agent, &AgentUpdate { visibility: Some("private".into()), ..Default::default() })
            .await?
            .expect("agent");
        let deleted = tokio::time::timeout(Duration::from_secs(5), next_task_event(&mut body, task)).await??;
        assert_eq!(deleted["type"], json!("task.deleted"));
        assert_eq!(deleted["agent_id"], Value::Null);
        assert_eq!(deleted["actor_user_id"], Value::Null);
        assert_eq!(deleted["payload"], json!({ "task_id": task, "quiet": true }));
        assert!(!quest_visible(&state, &viewer, task).await);
        assert_eq!(room_status(&state, &viewer, task).await, Err(StatusCode::NOT_FOUND));

        let Json(viewer_events) = list_events(
            State(state.clone()),
            Extension(UserId(viewer_user)),
            Extension(viewer.clone()),
            Query(EventsQuery { after: Some(delete_after), agent_id: None, limit: None }),
        )
        .await
        .expect("viewer events");
        let viewer_deleted = viewer_events.into_iter().find(|event| event.event_type == "task.deleted" && event.payload["task_id"] == json!(task)).expect("viewer deletion");
        assert_eq!((viewer_deleted.seq, viewer_deleted.agent_id, viewer_deleted.payload), (deleted["seq"].as_i64().expect("seq"), None, json!({ "task_id": task, "quiet": true })));

        let owner = Viewer {
            user: creator,
            org: Some(org),
            role: Role::Write,
            org_role: Some(loka_shared::orgs::OrgRole::Member),
            instance_admin: false,
        };
        let Json(owner_events) = list_events(
            State(state.clone()),
            Extension(UserId(creator)),
            Extension(owner),
            Query(EventsQuery { after: Some(delete_after), agent_id: None, limit: None }),
        )
        .await
        .expect("owner events");
        let owner_deleted = owner_events.into_iter().find(|event| event.event_type == "task.deleted" && event.payload["task_id"] == json!(task)).expect("owner deletion");
        assert_eq!((owner_deleted.seq, owner_deleted.agent_id, owner_deleted.payload), (deleted["seq"].as_i64().expect("seq"), Some(agent), json!({ "reason": "Removed after handoff", "task_id": task })));
        relay.abort();
        Ok(())
    }
}

#[cfg(test)]
mod milestone_route_tests {
    use super::*;

    async fn state(url: &str) -> anyhow::Result<AppState> {
        let db = loka_shared::db::Database::connect(url).await?;
        let pool = db.pool().clone();
        Ok(AppState {
            db,
            config: crate::config::ApiConfig { repos: vec![], backends: indexmap::IndexMap::new(), skills: vec![], mcp_servers: vec![] },
            world: loka_shared::world::WorldStore::new(pool.clone()),
            runners: loka_shared::runners::RunnerStore::new(pool),
            world_tx: tokio::sync::broadcast::channel(8).0,
            signal_tx: tokio::sync::broadcast::channel(8).0,
            runner_tx: crate::runner_wakes::RunnerWakes::default(),
            term: crate::terminal::TermHub::new(),
        })
    }

    #[tokio::test]
    async fn explicit_check_retries_the_same_pr_after_it_closed() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("MILESTONES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("route-{tag}@milestones.test"))
            .fetch_one(state.db.pool())
            .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')")
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .bind(user)
            .execute(state.db.pool())
            .await?;
        sqlx::query("UPDATE users SET active_org_id = $2 WHERE id = $1")
            .bind(user)
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .execute(state.db.pool())
            .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, pr_url) VALUES ($1, 'Retry close', 'failed', 'https://github.com/o/r/pull/9') RETURNING id",
        )
        .bind(user)
        .fetch_one(state.db.pool())
        .await?;
        let store = MilestoneStore::new(state.world.pool().clone());
        let milestone = store
            .add(task, &[NewMilestone { title: "Merge it".into(), kind: "merge".into() }])
            .await?
            .pop()
            .expect("merge milestone");
        sqlx::query(
            "UPDATE task_milestones SET status = 'failed', detail = 'Closed without merging', data = jsonb_build_object('closed_pr_url', 'https://github.com/o/r/pull/9') WHERE id = $1",
        )
        .bind(milestone.id)
        .execute(state.db.pool())
        .await?;

        let Json(result) = check_milestones(State(state.clone()), Extension(UserId(user)), Extension(Role::Write), Path(task))
            .await
            .map_err(|status| anyhow::anyhow!("check failed with {status}"))?;

        assert_eq!(result["queued"], json!(true));
        let rearmed = store.get(milestone.id).await?.expect("rearmed milestone");
        assert_eq!(rearmed.status, "active");
        assert_eq!(rearmed.detail.as_deref(), Some("Waiting for review"));
        assert!(rearmed.next_check_at.is_some());

        sqlx::query(
            "UPDATE task_milestones SET status = 'skipped', detail = 'No pull request was opened', next_check_at = NULL WHERE id = $1",
        )
        .bind(milestone.id)
        .execute(state.db.pool())
        .await?;
        let Json(result) = check_milestones(State(state.clone()), Extension(UserId(user)), Extension(Role::Write), Path(task))
            .await
            .map_err(|status| anyhow::anyhow!("check failed with {status}"))?;
        assert_eq!(result["queued"], json!(true));
        let rearmed = store.get(milestone.id).await?.expect("late PR skip rearmed");
        assert_eq!((rearmed.status.as_str(), rearmed.detail.as_deref()), ("active", Some("Waiting for review")));
        assert!(rearmed.next_check_at.is_some());

        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(task).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM org_members WHERE user_id = $1").bind(user).execute(state.db.pool()).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(state.db.pool()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod message_task_tests {
    use super::*;

    async fn state(url: &str) -> anyhow::Result<AppState> {
        let db = loka_shared::db::Database::connect(url).await?;
        let pool = db.pool().clone();
        Ok(AppState {
            db,
            config: crate::config::ApiConfig { repos: vec![], backends: indexmap::IndexMap::new(), skills: vec![], mcp_servers: vec![] },
            world: loka_shared::world::WorldStore::new(pool.clone()),
            runners: loka_shared::runners::RunnerStore::new(pool),
            world_tx: tokio::sync::broadcast::channel(8).0,
            signal_tx: tokio::sync::broadcast::channel(8).0,
            runner_tx: crate::runner_wakes::RunnerWakes::default(),
            term: crate::terminal::TermHub::new(),
        })
    }

    #[tokio::test]
    async fn a_clarification_answer_keeps_the_original_quest_resources() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, name, world_role, active_org_id) VALUES ($1, 'Ana', 'write', $2) RETURNING id",
        )
        .bind(format!("task-message-{tag}@example.test"))
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .bind(user)
            .execute(state.db.pool())
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id)
             VALUES ($1, 'Pri', 'claude', 'sonnet', $2, $3) RETURNING id",
        )
        .bind(format!("task-message-{tag}"))
        .bind(user)
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Keep task context', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        let viewer = Viewer {
            user,
            org: Some(loka_shared::orgs::DEFAULT_ORG),
            role: Role::Write,
            org_role: Some(loka_shared::orgs::OrgRole::Member),
            instance_admin: false,
        };
        let original_tail = "ORIGINAL_ROOM_TEXT_TAIL";
        let answer_tail = "ACCEPTED_ANSWER_TEXT_TAIL";
        let original_body = format!("{} {original_tail}", "original request context ".repeat(300));
        let answer_body = format!("{} {answer_tail}", "accepted answer context ".repeat(300));
        assert!(original_body.len() > 6000);
        assert!(answer_body.len() > 6000);
        let upload = state.world.insert_upload("text/plain", "request.txt", b"original attachment", Some(user)).await?;
        let result = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: original_body,
                attachments: vec![upload.id],
                options: None,
                answers: None,
                spoken: false,
                task_id: Some(task),
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: Some(json!({ "text": "original quoted context" })),
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("post_message returned {status}"))?;
        assert_eq!(result.0, StatusCode::CREATED);
        let (source_inbox_id, mut payload): (i64, Value) = sqlx::query_as(
            "SELECT id, payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message' AND status = 'pending'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(agent)
        .fetch_one(state.db.pool())
        .await?;
        assert_eq!(payload["task_id"], json!(task));
        let stranger: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, name, world_role, active_org_id) VALUES ($1, 'Eve', 'write', $2) RETURNING id",
        )
        .bind(format!("foreign-task-message-{tag}@example.test"))
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .bind(stranger)
            .execute(state.db.pool())
            .await?;
        let foreign = state.world.insert_upload("text/plain", "foreign.txt", b"foreign attachment", Some(stranger)).await?;
        payload["attachments"].as_array_mut().unwrap().push(json!({
            "id": foreign.id,
            "mime": "text/plain",
            "name": "foreign.txt",
            "url": format!("/api/world/uploads/{}", foreign.id),
        }));
        payload["files"].as_array_mut().unwrap().push(json!({
            "id": foreign.id,
            "mime": "text/plain",
            "name": "foreign.txt",
            "text": "foreign attachment",
        }));
        sqlx::query("UPDATE agent_inbox SET payload = $2 WHERE id = $1")
            .bind(source_inbox_id)
            .bind(&payload)
            .execute(state.db.pool())
            .await?;
        let card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which database should this use?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Stored origin request is intentionally shorter",
                            "task_id": task,
                            "title": "Keep task context",
                            "source_inbox_id": source_inbox_id,
                            "message_id": result.1.id,
                        },
                    }),
                },
                None,
            )
            .await?;
        let answer = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: answer_body,
                attachments: vec![],
                options: None,
                answers: Some(card.id),
                spoken: false,
                task_id: Some(task),
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("answer post_message returned {status}"))?;
        assert_eq!(answer.0, StatusCode::CREATED);
        assert_eq!(answer.1.meta["attachments"], json!([]));
        assert!(answer.1.meta.get("quote").is_none());
        let continuations: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'task.assigned'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(card.id.to_string())
        .fetch_all(state.db.pool())
        .await?;
        assert_eq!(continuations.len(), 1);
        let continuation = &continuations[0];
        assert_eq!(continuation["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["files"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["attachments"][0]["id"], json!(upload.id));
        assert_eq!(continuation["files"][0]["id"], json!(upload.id));
        assert_eq!(continuation["files"][0]["text"], "original attachment");
        assert!(continuation["brief"].as_str().unwrap().contains("original quoted context"));
        assert!(continuation["brief"].as_str().unwrap().contains(original_tail));
        assert!(continuation["brief"].as_str().unwrap().contains(answer_tail));
        assert!(continuation["clarification_handoff"]["request"].as_str().unwrap().contains(original_tail));
        assert!(continuation["clarification_handoff"]["request"].as_str().unwrap().contains("original quoted context"));
        assert_eq!(continuation["clarification_handoff"]["question"], "Which database should this use?");
        assert!(continuation["clarification_handoff"]["answer"].as_str().unwrap().contains(answer_tail));
        Ok(())
    }

    #[tokio::test]
    async fn an_unbound_clarification_queues_one_canonical_private_handoff() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let state = state(&url).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, name, world_role, active_org_id) VALUES ($1, 'Mira', 'write', $2) RETURNING id",
        )
        .bind(format!("unbound-clarification-{tag}@example.test"))
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
            .bind(loka_shared::orgs::DEFAULT_ORG)
            .bind(user)
            .execute(state.db.pool())
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id)
             VALUES ($1, 'Rio', 'claude', 'sonnet', $2, $3) RETURNING id",
        )
        .bind(format!("unbound-clarification-{tag}"))
        .bind(user)
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        let caller_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Unrelated room', 'in_progress', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(loka_shared::orgs::DEFAULT_ORG)
        .fetch_one(state.db.pool())
        .await?;
        let viewer = Viewer {
            user,
            org: Some(loka_shared::orgs::DEFAULT_ORG),
            role: Role::Write,
            org_role: Some(loka_shared::orgs::OrgRole::Member),
            instance_admin: false,
        };
        let linked = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "The linked deployment context",
                    session_id: None,
                    run_id: None,
                    meta: Value::Null,
                },
                None,
            )
            .await?;
        let upload_bytes = b"private original attachment";
        let upload = state.world.insert_upload("text/plain", "private.txt", upload_bytes, Some(user)).await?;
        let original_tail = "UNBOUND_ORIGINAL_TAIL";
        let answer_tail = "UNBOUND_ANSWER_TAIL";
        let original_body = format!(
            "Use msg:{} as context. {} {original_tail}",
            linked.id,
            "unbound original context ".repeat(260),
        );
        let answer_body = format!("{} {answer_tail}", "unbound accepted answer ".repeat(260));
        assert!(original_body.len() > 5500);
        assert!(answer_body.len() > 5500);
        let original = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: original_body,
                attachments: vec![upload.id],
                options: None,
                answers: None,
                spoken: false,
                task_id: None,
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: Some(json!({ "text": "unbound original quote" })),
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("original post_message returned {status}"))?;
        let (source_inbox_id, source_payload): (i64, Value) = sqlx::query_as(
            "SELECT id, payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message' AND payload->>'message_id' = $2
             ORDER BY id DESC LIMIT 1",
        )
        .bind(agent)
        .bind(original.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert_eq!(source_payload["links"].as_array().unwrap().len(), 1);
        let card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which environment should I use?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Stored preview is shorter",
                            "source_inbox_id": source_inbox_id,
                            "message_id": original.1.id,
                        },
                    }),
                },
                None,
            )
            .await?;
        let answer = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: answer_body,
                attachments: vec![],
                options: None,
                answers: Some(card.id),
                spoken: false,
                task_id: Some(caller_task),
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: Some(json!({ "text": "unbound answer quote" })),
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("answer post_message returned {status}"))?;
        let continuations: Vec<(i64, Value)> = sqlx::query_as(
            "SELECT id, payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(card.id.to_string())
        .fetch_all(state.db.pool())
        .await?;
        assert_eq!(continuations.len(), 1);
        let continuation_id = continuations[0].0;
        let continuation = &continuations[0].1;
        assert_eq!(continuation["message_id"], json!(answer.1.id));
        assert!(continuation["body"].as_str().unwrap().contains(answer_tail));
        assert!(continuation["task_id"].is_null());
        assert!(continuation["clarification_handoff"]["request"].as_str().unwrap().contains(original_tail));
        assert!(continuation["clarification_handoff"]["request"].as_str().unwrap().contains("unbound original quote"));
        assert_eq!(continuation["clarification_handoff"]["question"], "Which environment should I use?");
        assert!(continuation["clarification_handoff"]["answer"].as_str().unwrap().contains(answer_tail));
        assert_eq!(continuation["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["attachments"][0]["id"], json!(upload.id));
        assert_eq!(continuation["attachments"][0]["url"], json!(format!("/api/world/uploads/{}", upload.id)));
        assert_eq!(continuation["files"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["files"][0]["text"], "private original attachment");
        assert_eq!(continuation["links"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["links"][0]["ref"], json!(linked.id));
        assert_eq!(continuation["from_owner"], true);
        assert!(continuation.get("clarification_channel").is_none());
        assert_eq!(continuation["quote"]["text"], "unbound answer quote");
        let carried_bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM world_uploads WHERE id = $1")
            .bind(continuation["attachments"][0]["id"].as_str().unwrap().parse::<Uuid>()?)
            .fetch_one(state.db.pool())
            .await?;
        assert_eq!(carried_bytes.as_slice(), upload_bytes);
        let visible_event: Value = sqlx::query_scalar(
            "SELECT payload FROM world_events WHERE type = 'message.created' AND payload->>'message_id' = $1",
        )
        .bind(answer.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert_eq!(visible_event["attachments"], json!([]));
        assert_eq!(visible_event["files"], json!([]));
        assert_eq!(visible_event["links"], json!([]));
        assert!(visible_event.get("clarification_handoff").is_none());
        let mut tx = state.db.pool().begin().await?;
        let legacy_prior = clarification_source_resources(
            &mut tx,
            &json!({ "message_id": answer.1.id }),
            agent,
            user,
            viewer.org,
            None,
        )
        .await?;
        assert_eq!(legacy_prior["clarification_handoff"], continuation["clarification_handoff"]);
        drop(tx);

        let completed_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, org_id)
             VALUES ($1, 'Completed source', 'todo', $2, $3) RETURNING id",
        )
        .bind(user)
        .bind(agent)
        .bind(viewer.org)
        .fetch_one(state.db.pool())
        .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(completed_task)
            .execute(state.db.pool())
            .await?;
        let completed_source = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: "Use the completed quest as context".into(),
                attachments: vec![upload.id],
                options: None,
                answers: None,
                spoken: false,
                task_id: Some(completed_task),
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("completed source post_message returned {status}"))?;
        let completed_source_inbox: i64 = sqlx::query_scalar(
            "SELECT id FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message' AND payload->>'message_id' = $2
             ORDER BY id DESC LIMIT 1",
        )
        .bind(agent)
        .bind(completed_source.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        let completed_card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which matching format?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Use the completed quest as context",
                            "source_inbox_id": completed_source_inbox,
                            "message_id": completed_source.1.id,
                            "context_task_id": completed_task,
                        },
                    }),
                },
                None,
            )
            .await?;
        let completed_room = quest_room(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(viewer.clone()),
            Path(completed_task),
            Query(RoomQuery { after: None, since: None, focus: None }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("completed room returned {status}"))?;
        assert_eq!(completed_room.0["clarification"]["id"], completed_card.id.to_string());
        assert!(completed_room.0["room"]["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| message["id"] == completed_card.id.to_string())));
        let completed_answer = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: "Use CSV".into(),
                attachments: vec![],
                options: None,
                answers: Some(completed_card.id),
                spoken: false,
                task_id: Some(completed_task),
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("completed answer post_message returned {status}"))?;
        let completed_continuation: Value = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(completed_card.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert!(completed_continuation["task_id"].is_null());
        assert_eq!(completed_continuation["attachments"][0]["id"], json!(upload.id));
        let completed_event_task: Option<Uuid> = sqlx::query_scalar(
            "SELECT task_id FROM world_events WHERE type = 'message.created' AND payload->>'message_id' = $1",
        )
        .bind(completed_answer.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert_eq!(completed_event_task, Some(completed_task));
        let answered_room = quest_room(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(viewer.clone()),
            Path(completed_task),
            Query(RoomQuery { after: None, since: None, focus: None }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("answered completed room returned {status}"))?;
        assert!(answered_room.0["clarification"].is_null());
        assert!(answered_room.0["room"]["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| {
                message["id"] == completed_answer.1.id.to_string()
                    && message["meta"]["context_task_id"] == completed_task.to_string()
            })));

        let stale_card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which stale format?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Use the completed quest as context",
                            "source_inbox_id": completed_source_inbox,
                            "message_id": completed_source.1.id,
                            "context_task_id": completed_task,
                        },
                    }),
                },
                None,
            )
            .await?;
        state.world.delete_task(completed_task, "Mira", None).await?;
        let inbox_before_stale_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(state.db.pool())
            .await?;
        for task_id in [None, Some(completed_task)] {
            let stale_answer = post_message(
                State(state.clone()),
                Extension(UserId(user)),
                Extension(Role::Write),
                Extension(viewer.clone()),
                Path(agent),
                Json(PostMessage {
                    body: "Use JSON".into(),
                    attachments: vec![],
                    options: None,
                    answers: Some(stale_card.id),
                    spoken: false,
                    task_id,
                    reply_to: None,
                    share_output: false,
                    run_on: None,
                    quote: None,
                    changes: None,
                }),
            )
            .await;
            assert_eq!(stale_answer.expect_err("deleted contextual quest"), StatusCode::CONFLICT);
        }
        let stale_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND meta->>'answers' = $2")
            .bind(agent)
            .bind(stale_card.id.to_string())
            .fetch_one(state.db.pool())
            .await?;
        assert_eq!(stale_answers, 1);
        let inbox_after_stale_answers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(state.db.pool())
            .await?;
        assert_eq!(inbox_after_stale_answers, inbox_before_stale_answers);

        let second_question = "Which deployment window should I use?";
        let second_answer_tail = "SECOND_ACCEPTED_ANSWER_TAIL";
        let second_answer = format!("{} {second_answer_tail}", "second accepted answer ".repeat(280));
        let second_card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: second_question,
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "SECOND_ROUND_VISIBLE_PREVIEW_MUST_NOT_APPEAR",
                            "source_inbox_id": continuation_id,
                            "message_id": answer.1.id,
                        },
                    }),
                },
                None,
            )
            .await?;
        let second_post = || PostMessage {
            body: second_answer.clone(),
            attachments: vec![],
            options: None,
            answers: Some(second_card.id),
            spoken: false,
            task_id: None,
            reply_to: None,
            share_output: false,
            run_on: None,
            quote: Some(json!({ "text": "second answer quote" })),
            changes: None,
        };
        let second_result = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(second_post()),
        )
        .await
        .map_err(|status| anyhow::anyhow!("second answer post_message returned {status}"))?;
        assert_eq!(second_result.0, StatusCode::CREATED);
        let second_visible_event: Value = sqlx::query_scalar(
            "SELECT payload FROM world_events WHERE type = 'message.created' AND payload->>'message_id' = $1",
        )
        .bind(second_result.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert!(second_visible_event.get("clarification_handoff").is_none());
        let duplicate = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(second_post()),
        )
        .await;
        assert!(matches!(duplicate, Err(StatusCode::CONFLICT)));
        let second_continuations: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(second_card.id.to_string())
        .fetch_all(state.db.pool())
        .await?;
        assert_eq!(second_continuations.len(), 1);
        let second_continuation = &second_continuations[0];
        let second_handoff = &second_continuation["clarification_handoff"];
        let canonical_brief = loka_shared::quests::clarification_exchange(
            second_handoff.get("request").and_then(Value::as_str),
            second_handoff["question"].as_str().unwrap(),
            second_handoff["answer"].as_str().unwrap(),
            second_continuation,
        );
        for marker in [
            original_tail,
            "Which environment should I use?",
            answer_tail,
            second_question,
            second_answer_tail,
            "unbound original quote",
            "unbound answer quote",
            "second answer quote",
        ] {
            assert_eq!(canonical_brief.matches(marker).count(), 1, "{marker}");
        }
        assert!(!canonical_brief.contains("SECOND_ROUND_VISIBLE_PREVIEW_MUST_NOT_APPEAR"));
        assert_eq!(second_continuation["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(second_continuation["attachments"][0]["id"], json!(upload.id));
        assert_eq!(second_continuation["files"][0]["text"], "private original attachment");
        let second_bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM world_uploads WHERE id = $1")
            .bind(second_continuation["attachments"][0]["id"].as_str().unwrap().parse::<Uuid>()?)
            .fetch_one(state.db.pool())
            .await?;
        assert_eq!(second_bytes.as_slice(), upload_bytes);

        let legacy_card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Can I recover the old message reference?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Legacy preview",
                            "message_id": original.1.id,
                        },
                    }),
                },
                None,
            )
            .await?;
        let (status, _) = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: "Recover it".into(),
                attachments: vec![],
                options: None,
                answers: Some(legacy_card.id),
                spoken: false,
                task_id: None,
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("legacy answer post_message returned {status}"))?;
        assert_eq!(status, StatusCode::CREATED);
        let legacy: Value = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(legacy_card.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert!(legacy["clarification_handoff"]["request"].as_str().unwrap().contains(original_tail));
        assert!(legacy["clarification_handoff"]["request"].as_str().unwrap().contains("unbound original quote"));
        assert_eq!(legacy["attachments"][0]["id"], json!(upload.id));
        assert_eq!(legacy["attachments"][0]["url"], json!(format!("/api/world/uploads/{}", upload.id)));

        let root = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "user",
                    author_user_id: Some(user),
                    author_agent_id: None,
                    body: "Thread root",
                    session_id: None,
                    run_id: None,
                    meta: Value::Null,
                },
                None,
            )
            .await?;
        state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "THREAD_FINAL_REQUIREMENT",
                    session_id: None,
                    run_id: None,
                    meta: Value::Null,
                },
                Some(root.id),
            )
            .await?;
        let thread_upload = state.world.insert_upload("text/plain", "thread.txt", b"thread attachment", Some(user)).await?;
        let thread_source = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: String::new(),
                attachments: vec![thread_upload.id],
                options: None,
                answers: None,
                spoken: false,
                task_id: None,
                reply_to: Some(root.id),
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("thread source post_message returned {status}"))?;
        let (thread_source_inbox, thread_source_payload): (i64, Value) = sqlx::query_as(
            "SELECT id, payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message' AND payload->>'message_id' = $2
             ORDER BY id DESC LIMIT 1",
        )
        .bind(agent)
        .bind(thread_source.1.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        assert_eq!(thread_source_payload["reply_to"], json!(root.id));
        assert!(thread_source_payload["thread"].as_str().unwrap().contains("THREAD_FINAL_REQUIREMENT"));
        sqlx::query("DELETE FROM agent_inbox WHERE id = $1")
            .bind(thread_source_inbox)
            .execute(state.db.pool())
            .await?;
        state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "LATER_THREAD_CHANGE_MUST_NOT_APPEAR",
                    session_id: None,
                    run_id: None,
                    meta: Value::Null,
                },
                Some(root.id),
            )
            .await?;
        let thread_card = state
            .world
            .insert_reply(
                NewMessage {
                    agent_id: agent,
                    author_kind: "agent",
                    author_user_id: None,
                    author_agent_id: Some(agent),
                    body: "Which final requirement should I use?",
                    session_id: None,
                    run_id: None,
                    meta: json!({
                        "kind": "ask",
                        "choices": [],
                        "clarify_request": {
                            "request": "Attachment-only thread request",
                            "message_id": thread_source.1.id,
                        },
                    }),
                },
                None,
            )
            .await?;
        let (status, _) = post_message(
            State(state.clone()),
            Extension(UserId(user)),
            Extension(Role::Write),
            Extension(viewer.clone()),
            Path(agent),
            Json(PostMessage {
                body: "Use the attached final requirement".into(),
                attachments: vec![],
                options: None,
                answers: Some(thread_card.id),
                spoken: false,
                task_id: None,
                reply_to: None,
                share_output: false,
                run_on: None,
                quote: None,
                changes: None,
            }),
        )
        .await
        .map_err(|status| anyhow::anyhow!("thread answer post_message returned {status}"))?;
        assert_eq!(status, StatusCode::CREATED);
        let thread_continuation: Value = sqlx::query_scalar(
            "SELECT payload FROM agent_inbox
             WHERE agent_id = $1 AND kind = 'message'
               AND payload->>'clarification_continuation' = $2",
        )
        .bind(agent)
        .bind(thread_card.id.to_string())
        .fetch_one(state.db.pool())
        .await?;
        let thread_request = thread_continuation["clarification_handoff"]["request"].as_str().unwrap();
        assert!(thread_request.contains("Attachment-only thread request"));
        assert!(thread_request.contains("THREAD_FINAL_REQUIREMENT"));
        assert!(thread_request.contains(&root.id.simple().to_string()[..8]));
        assert!(!thread_request.contains("LATER_THREAD_CHANGE_MUST_NOT_APPEAR"));
        assert_eq!(thread_continuation["attachments"][0]["id"], json!(thread_upload.id));
        assert_eq!(thread_continuation["files"][0]["text"], "thread attachment");

        let mut tx = state.db.pool().begin().await?;
        let mismatched = clarification_source_resources(
            &mut tx,
            &json!({ "source_inbox_id": source_inbox_id, "message_id": thread_source.1.id }),
            agent,
            user,
            viewer.org,
            None,
        )
        .await?;
        assert_eq!(mismatched, json!({}));
        let wrong_actor = clarification_source_resources(
            &mut tx,
            &json!({ "source_inbox_id": source_inbox_id, "message_id": original.1.id }),
            agent,
            Uuid::new_v4(),
            viewer.org,
            None,
        )
        .await?;
        assert_eq!(wrong_actor, json!({}));
        let wrong_task = clarification_source_resources(
            &mut tx,
            &json!({ "source_inbox_id": source_inbox_id, "message_id": original.1.id }),
            agent,
            user,
            viewer.org,
            Some(caller_task),
        )
        .await?;
        assert_eq!(wrong_task, json!({}));
        drop(tx);

        let numeric_seq = state
            .world
            .emit(
                NewEvent::new("message.updated")
                    .agent(agent)
                    .actor(Some(user))
                    .payload(json!({ "message_id": 42, "body": "Edited channel requirement", "task_id": Value::Null })),
                Some((agent, "message")),
            )
            .await?;
        let numeric_inbox: i64 = sqlx::query_scalar("SELECT id FROM agent_inbox WHERE event_seq = $1")
            .bind(numeric_seq)
            .fetch_one(state.db.pool())
            .await?;
        let assignment_seq = state
            .world
            .emit(
                NewEvent::new("task.updated")
                    .agent(agent)
                    .actor(Some(user))
                    .task(Some(caller_task))
                    .payload(json!({ "task_id": caller_task, "brief": "Preserve this assignment snapshot" })),
                Some((agent, "task.assigned")),
            )
            .await?;
        let assignment_inbox: i64 = sqlx::query_scalar("SELECT id FROM agent_inbox WHERE event_seq = $1")
            .bind(assignment_seq)
            .fetch_one(state.db.pool())
            .await?;
        let mut tx = state.db.pool().begin().await?;
        let numeric = clarification_source_resources(
            &mut tx,
            &json!({ "source_inbox_id": numeric_inbox, "message_id": 42 }),
            agent,
            user,
            viewer.org,
            None,
        )
        .await?;
        assert_eq!(numeric["body"], "Edited channel requirement");
        let assignment = clarification_source_resources(
            &mut tx,
            &json!({ "source_inbox_id": assignment_inbox, "task_id": caller_task }),
            agent,
            user,
            viewer.org,
            Some(caller_task),
        )
        .await?;
        assert!(assignment.get("attachments").is_some());
        drop(tx);

        state
            .world
            .emit(
                NewEvent::new("message.created")
                    .agent(agent)
                    .actor(Some(user))
                    .payload(source_payload),
                None,
            )
            .await?;
        let mut tx = state.db.pool().begin().await?;
        let ambiguous = clarification_source_resources(
            &mut tx,
            &json!({ "message_id": original.1.id }),
            agent,
            user,
            viewer.org,
            None,
        )
        .await?;
        assert_eq!(ambiguous, json!({}));
        Ok(())
    }
}

#[cfg(test)]
mod message_route_tests {
    use super::*;

    #[test]
    fn structured_fields_route_before_the_shell_prefix() {
        let card = Some(Uuid::new_v4());
        assert_eq!(message_route(card, None, "!rm -rf ~"), MessageRoute::Structured, "a bang-prefixed answer is an answer, checked as a card answer");
        assert_eq!(message_route(None, card, "!rm -rf ~"), MessageRoute::Structured, "a bang-prefixed correction is a correction, checked as one");
        assert_eq!(message_route(card, card, "!ls"), MessageRoute::Structured);
        assert_eq!(message_route(None, None, "! ls -la "), MessageRoute::Shell("ls -la"), "a plain bang message still runs as a shell command");
        assert_eq!(message_route(None, None, "!"), MessageRoute::Shell(""), "an empty command is refused by the caller");
        assert_eq!(message_route(None, None, "hello !there"), MessageRoute::Plain);
    }

    #[test]
    fn corrections_reach_only_the_room_lead_or_answer_target() {
        let lead = Uuid::new_v4();
        let target = Uuid::new_v4();
        let question = loka_shared::quest_questions::ChangeableQuestion {
            question_id: Uuid::new_v4(),
            agent_id: target,
            task_id: Some(Uuid::new_v4()),
            lead_agent_id: Some(lead),
        };
        assert!(correction_agent_allowed(lead, &question));
        assert!(correction_agent_allowed(target, &question));
        assert!(!correction_agent_allowed(Uuid::new_v4(), &question));
    }
}
