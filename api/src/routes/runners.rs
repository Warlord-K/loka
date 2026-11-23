use std::convert::Infallible;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Extension, Json,
};
use loka_agent::runner_server::RunnerCoordinator;
use loka_shared::runners::{
    random_token, AssignRunner, AssignRunnerError, BeginCompletion, CreateRunner, HandoffRunner,
    HandoffRunnerError, LeaseBeat, LeaseSteers, RunnerCompletion, RevokeRunner, RunnerEvents, RunnerHeartbeat, RunnerPoll, RunnerPolled,
    RunnerPrCheckResult, RunnerRegistration, RunnerView, UpdateRunner,
};
use base64::Engine;
use ring::digest;
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{auth::UserId, AppState};
use loka_shared::readiness::SigningNotice;

type ApiError = (StatusCode, Json<Value>);

#[derive(Serialize)]
pub struct AgentRunner {
    runner: Option<RunnerView>,
}

fn problem(status: StatusCode, code: &str, message: impl Into<String>) -> ApiError {
    (
        status,
        Json(json!({ "error": code, "message": message.into() })),
    )
}

fn internal(error: anyhow::Error) -> ApiError {
    tracing::error!(error = %error, "runner route failed");
    problem(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Runner operation failed",
    )
}

pub async fn finish_stopped(state: &AppState, stopped: &loka_shared::withdraw::StoppedWork) {
    if stopped.on_runners.is_empty() {
        return;
    }
    let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
    if let Err(error) = coordinator.handle_expired(&stopped.on_runners).await {
        tracing::warn!(%error, runs = stopped.on_runners.len(), "could not finish runs stopped when access was withdrawn");
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Vec<RunnerView>>, ApiError> {
    Ok(Json(state.runners.list(user.0).await.map_err(internal)?))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(request): Json<CreateRunner>,
) -> Result<(StatusCode, Json<RunnerRegistration>), ApiError> {
    if request.name.trim().is_empty()
        || request.name.trim().chars().count() > 80
        || request.platform.trim().is_empty()
        || request.platform.trim().chars().count() > 40
    {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid_runner",
            "Runner name or platform is invalid",
        ));
    }
    let registration = state.runners.register(user.0, &request).await.map_err(|error| {
        if error.downcast_ref::<loka_shared::runners::OneRunner>().is_some() {
            problem(StatusCode::CONFLICT, "one_runner", loka_shared::runners::ONE_RUNNER)
        } else {
            internal(error)
        }
    })?;
    Ok((StatusCode::CREATED, Json(registration)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateRunner>,
) -> Result<Json<RunnerView>, ApiError> {
    if request
        .name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty() || name.trim().chars().count() > 80)
    {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid_runner",
            "Runner name is invalid",
        ));
    }
    let (runner, stopped) = state
        .runners
        .update(user.0, id, &request)
        .await
        .map_err(internal)?;
    forget_runner(id);
    if !stopped.is_empty() {
        RunnerCoordinator::new(state.db.clone(), state.world.clone())
            .handle_expired(&stopped)
            .await
            .map_err(internal)?;
    }
    runner.map(Json).ok_or_else(|| {
        problem(
            StatusCode::NOT_FOUND,
            "runner_not_found",
            "Runner not found",
        )
    })
}

pub async fn revoke(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    forget_runner(id);
    let stopped = match state.runners.revoke(user.0, id).await.map_err(internal)? {
        RevokeRunner::Revoked(stopped) => stopped,
        RevokeRunner::NotFound => {
            return Err(problem(
                StatusCode::NOT_FOUND,
                "runner_not_found",
                "Runner not found",
            ));
        }
        RevokeRunner::InUse => {
            return Err(problem(
                StatusCode::CONFLICT,
                "runner_in_use",
                "Move this runner's agents before removing the device",
            ));
        }
    };
    if !stopped.is_empty() {
        RunnerCoordinator::new(state.db.clone(), state.world.clone())
            .handle_expired(&stopped)
            .await
            .map_err(internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_agent_runner(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<AgentRunner>, ApiError> {
    ensure_agent_owner(&state, user.0, agent_id).await?;
    let runner = state
        .runners
        .agent_runner(user.0, agent_id)
        .await
        .map_err(internal)?;
    Ok(Json(AgentRunner { runner }))
}

pub async fn set_agent_runner(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(agent_id): Path<Uuid>,
    Json(request): Json<AssignRunner>,
) -> Result<Json<AgentRunner>, ApiError> {
    ensure_agent_owner(&state, user.0, agent_id).await?;
    state
        .runners
        .assign_agent(user.0, agent_id, request.runner_id)
        .await
        .map_err(|error| match error {
            AssignRunnerError::AgentNotFound | AssignRunnerError::RunnerNotFound => problem(
                StatusCode::NOT_FOUND,
                "runner_not_found",
                "Agent or runner not found",
            ),
            AssignRunnerError::ActiveHistory => problem(
                StatusCode::CONFLICT,
                "session_handoff_required",
                "This agent already has session history. Move it with an explicit handoff.",
            ),
        })?;
    let runner = state
        .runners
        .agent_runner(user.0, agent_id)
        .await
        .map_err(internal)?;
    Ok(Json(AgentRunner { runner }))
}

pub async fn handoff_agent(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(agent_id): Path<Uuid>,
    Json(request): Json<HandoffRunner>,
) -> Result<Json<AgentRunner>, ApiError> {
    ensure_agent_owner(&state, user.0, agent_id).await?;
    state
        .runners
        .handoff_agent(user.0, agent_id, &request)
        .await
        .map_err(|error| match error {
            HandoffRunnerError::AgentNotFound | HandoffRunnerError::RunnerNotFound => problem(
                StatusCode::NOT_FOUND,
                "runner_not_found",
                "Agent or runner not found",
            ),
            HandoffRunnerError::Busy => problem(
                StatusCode::CONFLICT,
                "agent_busy",
                "Wait for the agent's current inbox or session work to finish, then try the handoff again",
            ),
            HandoffRunnerError::InvalidRequest(message) => {
                problem(StatusCode::BAD_REQUEST, "invalid_handoff", message)
            }
            HandoffRunnerError::Internal(error) => internal(error),
        })?;
    let runner = state
        .runners
        .agent_runner(user.0, agent_id)
        .await
        .map_err(internal)?;
    Ok(Json(AgentRunner { runner }))
}

pub async fn heartbeat(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RunnerHeartbeat>,
) -> Result<Json<RunnerView>, ApiError> {
    authenticate_runner(&state, &headers, Some(runner_id)).await?;
    Ok(Json(beat(&state, runner_id, &request).await?))
}

type ToldSigning = std::sync::Mutex<Option<std::collections::HashSet<String>>>;

fn claim_signing_notice(told: &ToldSigning, key: &str) -> bool {
    told.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_or_insert_with(Default::default).insert(key.to_string())
}

fn settle_signing_notice(told: &ToldSigning, key: &str, outcome: &anyhow::Result<SigningNotice>) {
    if matches!(outcome, Ok(SigningNotice::Sent | SigningNotice::AlreadySent | SigningNotice::NothingToSay)) {
        return;
    }
    if let Some(told) = told.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_mut() {
        told.remove(key);
    }
}

fn tell_signing_problem_once(state: &AppState, view: &RunnerView) {
    static TOLD: ToldSigning = std::sync::Mutex::new(None);
    let Some((key, _)) = loka_shared::readiness::signing_notice(view) else {
        return;
    };
    if !claim_signing_notice(&TOLD, &key) {
        return;
    }
    let world = state.world.clone();
    let view = view.clone();
    tokio::spawn(async move {
        let outcome = world.tell_signing_problem(&view).await;
        if let Err(error) = &outcome {
            tracing::warn!(%error, runner_id = %view.id, "could not tell the owner about commit signing");
        }
        settle_signing_notice(&TOLD, &key, &outcome);
    });
}

async fn beat(state: &AppState, runner_id: Uuid, request: &RunnerHeartbeat) -> Result<RunnerView, ApiError> {
    if !request.capabilities.is_object() {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid_capabilities",
            "Runner capabilities must be an object",
        ));
    }
    let held: Vec<Uuid> = request.leases.iter().take(64).copied().collect();
    state
        .runners
        .heartbeat_leases(runner_id, &held)
        .await
        .map_err(internal)?;
    let view = state
        .runners
        .heartbeat(runner_id, &request.capabilities)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            problem(
                StatusCode::CONFLICT,
                "runner_unavailable",
                "Runner is revoked",
            )
        })?;
    tell_signing_problem_once(state, &view);
    match state.runners.set_disk_wait(runner_id, request.disk_low).await {
        Ok(0) => {}
        Ok(runs) => tracing::info!(%runner_id, runs, low = request.disk_low, "Marked queued runs as waiting for disk space or released them"),
        Err(error) => tracing::warn!(%error, %runner_id, "disk wait update failed during runner heartbeat"),
    }
    match state.runners.adopt_stranded(runner_id, None).await {
        Ok(adopted) if !adopted.is_empty() => {
            tracing::info!(%runner_id, count = adopted.len(), "Put agents with no device on this runner");
            state.runner_tx.wake(runner_id);
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not place agents on a runner"),
    }
    Ok(view)
}

pub async fn poll(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RunnerPoll>,
) -> Result<Json<RunnerPolled>, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    let status = match &request.heartbeat {
        Some(heartbeat) => beat(&state, runner_id, heartbeat).await?.status,
        None => runner.status,
    };
    let mut polled = RunnerPolled { status, ..RunnerPolled::default() };
    if request.heartbeat.is_some() && runner.platform == "macos" {
        match state.world.github_feed_runner(runner_id).await {
            Ok(on) => polled.github_feed = Some(on),
            Err(error) => tracing::warn!(%error, %runner_id, "github feed lookup failed during runner poll"),
        }
        match state.runners.device_access(runner_id).await {
            Ok((screen, apps)) => {
                polled.screen = Some(screen);
                polled.apps = Some(apps);
            }
            Err(error) => tracing::warn!(%error, %runner_id, "screen access lookup failed during runner poll"),
        }
    }
    if request.heartbeat.is_some() {
        match state.runners.take_workspace_cleanup(runner_id).await {
            Ok(true) => polled.clean_workspaces = Some(true),
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %runner_id, "workspace cleanup lookup failed during runner poll"),
        }
    }
    if request.heartbeat.is_some() {
        match state.runners.take_cache_cleanup(runner_id).await {
            Ok(true) => polled.clean_caches = Some(true),
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %runner_id, "cache cleanup lookup failed during runner poll"),
        }
    }
    if polled.status == "paused" {
        return Ok(Json(polled));
    }
    let (shell, terminal, job, pr_check) = tokio::join!(
        async { if request.shell { Some(state.runners.claim_shell(runner_id).await) } else { None } },
        async {
            let room = loka_shared::terminal::TerminalRoom { shells: request.terminal, views: request.views };
            if room.any() { Some(state.runners.claim_terminal(runner_id, room).await) } else { None }
        },
        async { if request.kinds.is_empty() { None } else { Some(claim_job(&state, runner_id, &request.kinds).await) } },
        async {
            if request.pr_checks && request.heartbeat.is_some() {
                Some(state.runners.claim_pr_check(runner_id).await)
            } else {
                None
            }
        },
    );
    match shell {
        Some(Ok(claimed)) => polled.shell = claimed.map(shell_job),
        Some(Err(error)) => {
            tracing::warn!(%error, %runner_id, "shell claim failed during runner poll");
            polled.retry = true;
        }
        None => {}
    }
    match terminal {
        Some(Ok(claimed)) => {
            polled.terminal = claimed
        }
        Some(Err(error)) => {
            tracing::warn!(%error, %runner_id, "terminal claim failed during runner poll");
            polled.retry = true;
        }
        None => {}
    }
    match job {
        Some(Ok(job)) => polled.job = job,
        Some(Err(error)) => {
            tracing::warn!(%error, %runner_id, "job claim failed during runner poll");
            polled.retry = true;
        }
        None => {}
    }
    match pr_check {
        Some(Ok(pr_check)) => polled.pr_check = pr_check,
        Some(Err(error)) => {
            tracing::warn!(%error, %runner_id, "PR check claim failed during runner poll");
            polled.retry = true;
        }
        None => {}
    }
    Ok(Json(polled))
}

pub async fn pr_check(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(result): Json<RunnerPrCheckResult>,
) -> Result<StatusCode, ApiError> {
    authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if loka_shared::git_names::validate_repo(&result.repo).is_err()
        || !loka_shared::runners::pr_url_matches(&result.pr_url, &result.repo, result.pr_number)
    {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_pr", "Pull request does not match the quest repository"));
    }
    let merged = match result.state.as_str() {
        "merged" => true,
        "closed" => false,
        _ => return Err(problem(StatusCode::BAD_REQUEST, "invalid_state", "Pull request state is not final")),
    };
    state
        .world
        .settle_pr_from_runner_observed(result.task_id, &result.pr_url, merged, runner_id, result.observed_at)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct SlackBatch {
    notifications: Vec<loka_shared::slack::SlackNotification>,
}

pub async fn slack(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(batch): Json<SlackBatch>,
) -> Result<Json<Value>, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if batch.notifications.len() > 50 {
        return Err(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_notifications",
            "Send at most 50 Slack notifications at a time",
        ));
    }
    for notification in &batch.notifications {
        notification
            .validate()
            .map_err(|reason| problem(StatusCode::BAD_REQUEST, "invalid_notification", reason))?;
    }
    let mut created = 0;
    for notification in &batch.notifications {
        if state
            .world
            .record_slack_notification(runner.owner_user_id, notification)
            .await
            .map_err(internal)?
            .is_some()
        {
            created += 1;
        }
    }
    Ok(Json(json!({ "created": created })))
}

pub async fn slack_watches(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<loka_shared::slack::SlackWatchTargets>, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    Ok(Json(state.world.slack_watch_targets(runner.owner_user_id).await.map_err(internal)?))
}

#[derive(serde::Deserialize)]
pub struct GithubBatch {
    #[serde(default)]
    notifications: Vec<loka_shared::github_feed::GithubNotification>,
}

pub async fn github(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(batch): Json<GithubBatch>,
) -> Result<Json<Value>, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if runner.platform != "macos" {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "github_feed_desktop_only",
            "The GitHub feed runs only on the owner's Mac",
        ));
    }
    if batch.notifications.len() > loka_shared::github_feed::BATCH_MAX {
        return Err(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_notifications",
            "Send at most 100 GitHub notifications at a time",
        ));
    }
    for notification in &batch.notifications {
        notification
            .validate()
            .map_err(|reason| problem(StatusCode::BAD_REQUEST, "invalid_notification", reason))?;
    }
    let recorded = state
        .world
        .record_github(runner.owner_user_id, &batch.notifications)
        .await
        .map_err(internal)?;
    Ok(Json(json!(recorded)))
}

#[derive(serde::Deserialize)]
pub struct WorkspaceTasks {
    #[serde(default)]
    tasks: Vec<Uuid>,
    #[serde(default)]
    sessions: Vec<Uuid>,
    #[serde(default)]
    pressure: bool,
    #[serde(default)]
    idle: bool,
    #[serde(default)]
    prefixes: Vec<String>,
}

pub async fn workspaces(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<WorkspaceTasks>,
) -> Result<Json<Value>, ApiError> {
    authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if !request.prefixes.is_empty() {
        let quests = state.runners.quests_by_prefix(runner_id, &request.prefixes).await.map_err(internal)?;
        let quests: Vec<Value> = quests.into_iter().map(|(prefix, id)| json!({ "prefix": prefix, "id": id })).collect();
        return Ok(Json(json!({ "quests": quests })));
    }
    if !request.sessions.is_empty() {
        let closed = state.runners.closed_sessions(runner_id, &request.sessions).await.map_err(internal)?;
        return Ok(Json(json!({ "closed_sessions": closed })));
    }
    let entries = state
        .runners
        .closed_tasks(runner_id, &request.tasks, request.pressure)
        .await
        .map_err(internal)?;
    let closed: Vec<Uuid> = entries.iter().map(|entry| entry.id).collect();
    let idle = if request.idle {
        state.runners.idle_tasks(runner_id, &request.tasks).await.map_err(internal)?
    } else {
        Vec::new()
    };
    Ok(Json(json!({ "closed": closed, "entries": entries, "idle": idle })))
}

pub async fn disk_low(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    Json(notice): Json<loka_shared::runners::DiskNotice>,
) -> Result<Json<Value>, ApiError> {
    authenticate_runner(&state, &headers, Some(runner_id)).await?;
    let sent = state.runners.warn_disk_low(runner_id, &notice).await.map_err(internal)?;
    Ok(Json(json!({ "sent": sent })))
}

#[derive(Default, serde::Deserialize)]
struct ClaimRequest {
    #[serde(default)]
    kinds: Vec<String>,
}

pub async fn claim(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: ClaimRequest = if body.is_empty() {
        ClaimRequest::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if runner.status == "paused" {
        return Err(problem(
            StatusCode::CONFLICT,
            "runner_paused",
            "Runner is paused",
        ));
    }
    match claim_job(&state, runner_id, &request.kinds).await.map_err(internal)? {
        Some(job) => Ok(Json(job).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

async fn claim_job(state: &AppState, runner_id: Uuid, kinds: &[String]) -> anyhow::Result<Option<loka_shared::runners::RunnerJob>> {
    let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
    if kinds.is_empty() || kinds.iter().any(|kind| kind == "turn") {
        coordinator.prepare_turn(runner_id).await?;
    }
    let mcp_token = random_token()?;
    let Some(claimed) = state.runners.claim_kinds(runner_id, &mcp_token, kinds).await? else {
        return Ok(None);
    };
    let lease_id = claimed.lease_id;
    let run_id = claimed.run_id;
    match coordinator.build_job(claimed, mcp_token).await {
        Ok(job) => {
            if job.kind == "direct" {
                let _ = state
                    .world
                    .emit(
                        loka_shared::world::NewEvent::new("run.started")
                            .agent(job.agent_id)
                            .session(job.session_id)
                            .run(job.run_id)
                            .payload(json!({ "run_kind": job.kind, "quiet": true })),
                        None,
                    )
                    .await;
            }
            Ok(Some(job))
        }
        Err(error) => {
            state.runners.fail_claim(runner_id, lease_id, &error.to_string()).await?;
            coordinator.handle_expired(&[run_id]).await?;
            Err(error)
        }
    }
}

pub async fn stream(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if runner.status == "paused" {
        return Err(problem(
            StatusCode::CONFLICT,
            "runner_paused",
            "Runner is paused",
        ));
    }
    let mut wakes = state.runner_tx.subscribe(runner_id);
    let pending = state.runners.has_work(runner_id).await.map_err(internal)?;
    let event_stream = async_stream::stream! {
        let _open = crate::metrics::Open::new("runner");
        if pending {
            yield Ok::<Event, Infallible>(Event::default().event("wake").data("{}"));
        }
        while wakes.woken().await {
            yield Ok::<Event, Infallible>(Event::default().event("wake").data("{}"));
        }
    };
    Ok((
        [
            (axum::http::header::CACHE_CONTROL, "no-cache, no-transform"),
            (
                axum::http::HeaderName::from_static("x-accel-buffering"),
                "no",
            ),
        ],
        Sse::new(event_stream).keep_alive(KeepAlive::default()),
    ))
}

pub async fn lease_heartbeat(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let runner = authenticate_runner(&state, &headers, None).await?;
    let Some(renewed) = state
        .runners
        .heartbeat_lease(runner.id, lease_id)
        .await
        .map_err(internal)?
    else {
        return Err(problem(
            StatusCode::CONFLICT,
            "lease_stopped",
            "Stop the local job",
        ));
    };
    let Ok(beat) = serde_json::from_slice::<LeaseBeat>(&body) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let lease = &renewed.lease;
    let timeout_secs = matches!(lease.kind.as_str(), "task" | "followup" | "review")
        .then(|| renewed.timeout_secs.max(1) as u64);
    let steerable = match lease.kind.as_str() {
        "task" | "followup" => renewed.session_role.as_deref() == Some("task"),
        "direct" => renewed.session_role.as_deref() == Some("direct"),
        _ => false,
    };
    let steering = async {
        if !beat.steer {
            return Ok(Vec::new());
        }
        mark_steered(&state, lease.agent_id, lease.run_id, &beat.delivered).await?;
        if steerable {
            state.world.pending_steers(lease.session_id).await.map_err(internal)
        } else {
            Ok(Vec::new())
        }
    };
    let keys = async { state.world.key_version(lease.agent_id).await.map_err(internal) };
    let apps = async {
        if matches!(lease.kind.as_str(), "task" | "followup") {
            state.world.apps_allowed_for_run(lease.run_id).await.map(Some).map_err(internal)
        } else {
            Ok(None)
        }
    };
    let (steers, keys, apps) = tokio::try_join!(steering, keys, apps)?;
    Ok(Json(LeaseSteers { steers, timeout_secs, keys: Some(keys), apps }).into_response())
}

async fn mark_steered(state: &AppState, agent_id: Uuid, run_id: Uuid, delivered: &[Uuid]) -> Result<(), ApiError> {
    let mut delivered: Vec<Uuid> = delivered.iter().take(50).copied().collect();
    let mut seen = std::collections::HashSet::new();
    delivered.retain(|id| seen.insert(*id));
    let marked = state.world.mark_steered_many(&delivered, run_id).await.map_err(internal)?;
    for followup in delivered.iter().filter(|id| marked.contains(id)) {
        let _ = state
            .world
            .emit(
                loka_shared::world::NewEvent::new("run.steered")
                    .agent(agent_id)
                    .payload(json!({ "run_id": run_id, "followup_id": followup })),
                None,
            )
            .await;
    }
    Ok(())
}

pub async fn events(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RunnerEvents>,
) -> Result<StatusCode, ApiError> {
    let runner = authenticate_runner(&state, &headers, None).await?;
    if request.events.len() > 100 {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "too_many_events",
            "Send at most 100 runner events at a time",
        ));
    }
    let lease = state
        .runners
        .lease_for_runner(runner.id, lease_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::CONFLICT, "lease_stopped", "Stop the local job"))?;
    let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
    coordinator
        .ingest_events(&lease, request.events)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn complete(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
    headers: HeaderMap,
    Json(completion): Json<RunnerCompletion>,
) -> Result<Response, ApiError> {
    let runner = authenticate_runner(&state, &headers, None).await?;
    if !loka_shared::runners::plausible_cost(completion.cost_usd)
        || !completion.usage.plausible()
        || completion
            .pr_url
            .as_deref()
            .is_some_and(|url| !loka_shared::runners::plausible_pr_url(url))
        || completion
            .session_id
            .as_deref()
            .is_some_and(|session_id| session_id.len() > 512)
    {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid_completion",
            "Runner completion contains invalid usage or session data",
        ));
    }
    let Some(submission) = state.runners.lease_for_submission(runner.id, lease_id).await.map_err(internal)? else {
        match state.runners.lease_state(runner.id, lease_id).await {
            Ok(Some(lease)) => tracing::warn!(
                %lease_id,
                runner = %runner.id,
                lease_status = %lease.lease_status,
                run_status = %lease.run_status,
                kind = %lease.kind,
                cause = ?lease.cause,
                stopped_by = ?lease.stop_requested_by,
                error = ?lease.error,
                settled_secs_ago = ?lease.settled_secs_ago,
                "Runner completion was stale"
            ),
            Ok(None) => tracing::warn!(%lease_id, runner = %runner.id, "Runner completion named an unknown lease"),
            Err(error) => tracing::warn!(%error, %lease_id, "Could not read a stale lease"),
        }
        let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
        if let Err(error) = coordinator.keep_late_spend(runner.id, lease_id, &completion).await {
            tracing::warn!(error = %format!("{error:#}"), %lease_id, "Could not keep the spend of a completion that arrived after its run was closed");
            return Err(problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "late_spend_failed",
                "The spend of a stopped run could not be saved yet, try again",
            ));
        }
        return Err(problem(StatusCode::CONFLICT, "lease_stopped", "Completion is stale"));
    };
    mark_steered(&state, submission.agent_id, submission.run_id, &completion.steered).await?;
    let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
    let completion = coordinator
        .redact_completion(&submission, &completion)
        .await
        .map_err(internal)?;
    let encoded = serde_json::to_vec(&completion).map_err(|error| internal(error.into()))?;
    let completion_hash = digest::digest(&digest::SHA256, &encoded);
    match state
        .runners
        .begin_completion(runner.id, lease_id, completion_hash.as_ref(), &completion)
        .await
        .map_err(internal)?
    {
        BeginCompletion::AlreadyCompleted => return Ok(StatusCode::OK.into_response()),
        BeginCompletion::Busy => {
            return Err(problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "completion_in_progress",
                "Completion is already being applied",
            ))
        }
        BeginCompletion::Conflict => {
            return Err(problem(
                StatusCode::CONFLICT,
                "lease_stopped",
                "Completion is stale or does not match",
            ))
        }
        BeginCompletion::Begun => {}
    }
    let lease = state
        .runners
        .lease_for_completion(runner.id, lease_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::CONFLICT, "lease_stopped", "Completion is stale"))?;
    if let Err(error) = coordinator.complete(&lease, &completion).await {
        return Err(internal(error));
    }
    if !state
        .runners
        .finish_completion(runner.id, lease_id)
        .await
        .map_err(internal)?
    {
        return Err(problem(
            StatusCode::CONFLICT,
            "lease_stopped",
            "Completion was cancelled",
        ));
    }
    if let Err(error) = state.runners.apply_pending_move(lease.agent_id).await {
        tracing::error!(%error, agent_id = %lease.agent_id, "failed to apply pending device move");
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn mcp(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Result<Response, ApiError> {
    let token = authorization(&headers, "RunnerMcp")?;
    let lease = state
        .runners
        .lease_for_mcp(token)
        .await
        .map_err(internal)?
        .filter(|lease| lease.lease_id == lease_id)
        .ok_or_else(|| {
            problem(
                StatusCode::UNAUTHORIZED,
                "invalid_mcp_lease",
                "MCP lease is invalid",
            )
    })?;
    let coordinator = RunnerCoordinator::new(state.db.clone(), state.world.clone());
    match coordinator.mcp(&lease, &message).await.map_err(internal)? {
        Some(response) => Ok(Json(response).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

#[derive(serde::Deserialize)]
pub struct MediaQuery {
    name: Option<String>,
}

pub async fn media(
    State(state): State<AppState>,
    Path(lease_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<MediaQuery>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let token = authorization(&headers, "RunnerMcp")?;
    let lease = state
        .runners
        .lease_for_mcp(token)
        .await
        .map_err(internal)?
        .filter(|lease| lease.lease_id == lease_id && matches!(lease.kind.as_str(), "task" | "followup"))
        .ok_or_else(|| problem(StatusCode::UNAUTHORIZED, "invalid_mcp_lease", "MCP lease is invalid"))?;
    let agent = state
        .world
        .get_agent(lease.agent_id)
        .await
        .map_err(internal)?
        .filter(|agent| agent.owner_user_id == Some(lease.owner_user_id))
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "agent_not_found", "The agent is gone"))?;
    let name = q.name.unwrap_or_default();
    match state.world.store_agent_media(&agent, &name, &bytes).await {
        Ok(up) => Ok((StatusCode::CREATED, Json(json!({ "id": up.id, "mime": up.mime, "name": up.name }))).into_response()),
        Err(error) => Err(problem(StatusCode::UNPROCESSABLE_ENTITY, "media_refused", error.to_string())),
    }
}

pub async fn lease_upload(
    State(state): State<AppState>,
    Path((lease_id, upload_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let token = authorization(&headers, "RunnerMcp")?;
    let lease = state
        .runners
        .lease_for_mcp(token)
        .await
        .map_err(internal)?
        .filter(|lease| lease.lease_id == lease_id && matches!(lease.kind.as_str(), "task" | "followup"))
        .ok_or_else(|| problem(StatusCode::UNAUTHORIZED, "invalid_mcp_lease", "MCP lease is invalid"))?;
    let agent = state
        .world
        .get_agent(lease.agent_id)
        .await
        .map_err(internal)?
        .filter(|agent| agent.owner_user_id == Some(lease.owner_user_id))
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "agent_not_found", "The agent is gone"))?;
    let viewer = loka_shared::access::Viewer {
        user: lease.owner_user_id,
        org: agent.org_id,
        role: loka_shared::access::Role::Write,
        org_role: None,
        instance_admin: false,
    };
    let (mime, bytes) = state
        .world
        .upload_for_viewer(upload_id, &viewer)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "upload_not_found", "No upload with that id is visible to your owner"))?;
    if bytes.len() > loka_shared::media::MAX_VIDEO {
        return Err(problem(StatusCode::PAYLOAD_TOO_LARGE, "upload_too_large", "The upload is larger than 25 MB"));
    }
    let name = state.world.upload_name(upload_id).await.map_err(internal)?.unwrap_or_default();
    Ok(Json(json!({ "id": upload_id, "name": name, "mime": mime, "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes) })).into_response())
}

const RUNNER_AUTH_TTL: std::time::Duration = std::time::Duration::from_secs(10);
const RUNNER_AUTH_MAX: usize = 4096;

type AuthKey = (Option<Uuid>, Vec<u8>);

static RUNNER_AUTH: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<AuthKey, (RunnerView, std::time::Instant)>>> =
    std::sync::LazyLock::new(Default::default);

fn cached_runner(key: &AuthKey) -> Option<RunnerView> {
    let cache = RUNNER_AUTH.lock().unwrap_or_else(|e| e.into_inner());
    cache.get(key).filter(|(_, at)| at.elapsed() < RUNNER_AUTH_TTL).map(|(runner, _)| runner.clone())
}

fn remember_runner(key: AuthKey, runner: &RunnerView) {
    let mut cache = RUNNER_AUTH.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= RUNNER_AUTH_MAX {
        cache.retain(|_, (_, at)| at.elapsed() < RUNNER_AUTH_TTL);
        if cache.len() >= RUNNER_AUTH_MAX {
            cache.clear();
        }
    }
    cache.insert(key, (runner.clone(), std::time::Instant::now()));
}

pub(crate) fn forget_runner(id: Uuid) {
    RUNNER_AUTH.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, (runner, _)| runner.id != id);
}

pub(crate) async fn authenticate_runner(
    state: &AppState,
    headers: &HeaderMap,
    expected: Option<Uuid>,
) -> Result<RunnerView, ApiError> {
    let token = authorization(headers, "Runner")?;
    let key = (expected, loka_shared::runners::token_hash(token));
    let found = match cached_runner(&key) {
        Some(runner) => Some(runner),
        None => {
            let fresh = match expected {
                Some(id) => state.runners.authenticate(id, token).await,
                None => state.runners.authenticate_token(token).await,
            }
            .map_err(internal)?;
            if let Some(runner) = &fresh {
                remember_runner(key, runner);
            }
            fresh
        }
    };
    let runner = found.ok_or_else(|| {
        problem(
            StatusCode::UNAUTHORIZED,
            "invalid_runner",
            "Runner credential is invalid",
        )
    })?;
    if expected.is_some_and(|id| id != runner.id) {
        return Err(problem(
            StatusCode::UNAUTHORIZED,
            "invalid_runner",
            "Runner credential is invalid",
        ));
    }
    Ok(runner)
}

fn authorization<'a>(headers: &'a HeaderMap, scheme: &str) -> Result<&'a str, ApiError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            problem(
                StatusCode::UNAUTHORIZED,
                "missing_authorization",
                "Authorization is required",
            )
        })?;
    value
        .strip_prefix(scheme)
        .and_then(|value| value.strip_prefix(' '))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            problem(
                StatusCode::UNAUTHORIZED,
                "invalid_authorization",
                "Authorization scheme is invalid",
            )
        })
}

pub async fn shell_claim(
    State(state): State<AppState>,
    Path(runner_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if runner.status == "paused" {
        return Err(problem(
            StatusCode::CONFLICT,
            "runner_paused",
            "Runner is paused",
        ));
    }
    let Some((run_id, agent_id, command)) = state
        .runners
        .claim_shell(runner_id)
        .await
        .map_err(internal)?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    Ok(Json(shell_job((run_id, agent_id, command))).into_response())
}

fn shell_job((id, agent_id, command): (Uuid, Uuid, String)) -> loka_shared::shell::ShellJob {
    loka_shared::shell::ShellJob {
        id,
        command,
        workspace_key: format!("agents/{agent_id}"),
        timeout_secs: loka_shared::shell::shell_timeout(),
    }
}

pub async fn shell_output(
    State(state): State<AppState>,
    Path((runner_id, run_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<loka_shared::shell::ShellOutput>,
) -> Result<Json<Value>, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if request.chunk.len() > 64 * 1024 {
        return Err(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "chunk_too_large",
            "Send output in smaller chunks",
        ));
    }
    match state
        .runners
        .append_shell_output(runner.id, run_id, &request.chunk)
        .await
        .map_err(internal)?
    {
        Some(cancel) => Ok(Json(json!({ "cancel": cancel }))),
        None => Err(problem(
            StatusCode::CONFLICT,
            "shell_stopped",
            "Stop the shell command",
        )),
    }
}

pub async fn shell_finish(
    State(state): State<AppState>,
    Path((runner_id, run_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(mut finish): Json<loka_shared::shell::ShellFinish>,
) -> Result<StatusCode, ApiError> {
    let runner = authenticate_runner(&state, &headers, Some(runner_id)).await?;
    if !matches!(finish.status.as_str(), "done" | "failed" | "cancelled") {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid_status",
            "Shell finish status must be done, failed or cancelled",
        ));
    }
    if finish.output.chars().count() > loka_shared::shell::OUTPUT_CAP {
        let keep: String = finish
            .output
            .chars()
            .skip(finish.output.chars().count() - loka_shared::shell::OUTPUT_CAP)
            .collect();
        finish.output = keep;
        finish.truncated = true;
    }
    let Some(ended) = state
        .runners
        .finish_shell(runner.id, run_id, &finish)
        .await
        .map_err(internal)?
    else {
        return Err(problem(
            StatusCode::CONFLICT,
            "shell_stopped",
            "Shell run is not running anymore",
        ));
    };
    crate::routes::shell::announce_shell_end(&state, &ended).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_agent_owner(
    state: &AppState,
    user_id: Uuid,
    agent_id: Uuid,
) -> Result<(), ApiError> {
    let agent = state
        .world
        .get_agent(agent_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "agent_not_found", "Agent not found"))?;
    if agent.owner_user_id != Some(user_id) {
        return Err(problem(
            StatusCode::NOT_FOUND,
            "agent_not_found",
            "Agent not found",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod signing_notice_tests {
    use super::*;

    #[test]
    fn a_signing_notice_is_remembered_once_sent_and_retried_only_without_a_main_agent_or_on_errors() {
        let told: ToldSigning = std::sync::Mutex::new(None);
        assert!(claim_signing_notice(&told, "runner:loka"));
        assert!(!claim_signing_notice(&told, "runner:loka"), "a notice in flight is not sent twice");
        settle_signing_notice(&told, "runner:loka", &Ok(SigningNotice::NoMainAgent));
        assert!(claim_signing_notice(&told, "runner:loka"), "no main agent yet, so the next heartbeat tries again");
        settle_signing_notice(&told, "runner:loka", &Err(anyhow::anyhow!("database away")));
        assert!(claim_signing_notice(&told, "runner:loka"), "an error is retried on the next heartbeat");
        settle_signing_notice(&told, "runner:loka", &Ok(SigningNotice::Sent));
        assert!(!claim_signing_notice(&told, "runner:loka"), "a sent notice is remembered");
        assert!(claim_signing_notice(&told, "runner:unset"));
        settle_signing_notice(&told, "runner:unset", &Ok(SigningNotice::AlreadySent));
        assert!(!claim_signing_notice(&told, "runner:unset"), "a notice another replica or an earlier process sent is remembered");
        assert!(claim_signing_notice(&told, "other:unset"));
        settle_signing_notice(&told, "other:unset", &Ok(SigningNotice::NothingToSay));
        assert!(!claim_signing_notice(&told, "other:unset"), "nothing to say is not retried every heartbeat");
    }
}
