use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

mod access;
mod auth;
mod config;
mod migrate;
mod keepalive;
mod mcp;
mod metrics;
mod msgpack;
mod routes;
mod runner_wakes;
mod terminal;

pub use config::ApiConfig;

#[derive(Clone)]
pub struct AppState {
    pub db: loka_shared::db::Database,
    pub config: ApiConfig,
    pub world: loka_shared::world::WorldStore,
    pub runners: loka_shared::runners::RunnerStore,
    pub world_tx: tokio::sync::broadcast::Sender<loka_shared::world::EventWake>,
    pub runner_tx: runner_wakes::RunnerWakes,
    pub term: Arc<terminal::TermHub>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::args().nth(1).as_deref() == Some("login-link") {
        return print_login_link(std::env::args().nth(2));
    }
    loka_shared::environment::initialize();

    {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::from_default_env().add_directive("loka_api=info".parse()?))
            .with(tracing_subscriber::fmt::layer())
            .with(loka_shared::telemetry::log_shipper("api"))
            .init();
    }
    loka_shared::telemetry::start("api");
    let file_limit = loka_shared::limits::raise_open_files();
    loka_shared::limits::hide_process_memory();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8081".to_string())
        .parse()
        .expect("PORT must be a number");

    let db = loka_shared::db::Database::connect(&database_url).await?;
    if migrate::enabled() {
        migrate::run(db.pool()).await?;
    }
    metrics::sample_pool(db.pool().clone());
    if loka_shared::search_index::enabled() {
        let pool = db.pool().clone();
        tokio::spawn(async move {
            if let Err(error) = loka_shared::search_index::ensure(&pool).await {
                tracing::warn!(%error, "Search indexes were not checked");
            }
        });
    }
    let config = config::load()?;

    let world = loka_shared::world::WorldStore::new(db.pool().clone());
    if loka_shared::environment::rotating() {
        match world.rotate_environment_key().await {
            Ok(rotation) => tracing::info!(
                rotated = rotation.rotated,
                current = rotation.current,
                unreadable = rotation.unreadable,
                "Re-encrypted stored keys with the current WORLD_SECRETS_KEY"
            ),
            Err(error) => tracing::error!(%error, "Key rotation failed"),
        }
    }
    match world.ensure_feed_secrets().await {
        Ok(0) => {}
        Ok(made) => tracing::info!(made, "Gave webhook feeds without a secret a new one"),
        Err(error) => tracing::error!(%error, "Could not give webhook feeds their secrets"),
    }
    let runners = loka_shared::runners::RunnerStore::new(db.pool().clone());
    let (world_tx, _) = tokio::sync::broadcast::channel(256);
    tokio::spawn(relay_world_events(database_url.clone(), world_tx.clone()));
    let runner_tx = runner_wakes::RunnerWakes::default();
    let schedule_notify = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(relay_runner_events(
        database_url.clone(),
        db.pool().clone(),
        runner_tx.clone(),
        schedule_notify.clone(),
    ));
    let term = terminal::TermHub::new();
    let state = AppState {
        db,
        config,
        world,
        runners,
        world_tx,
        runner_tx,
        term,
    };
    tokio::spawn(sweep_runner_leases(state.clone()));
    tokio::spawn(watch_open_files(file_limit));
    loka_shared::pricing::spawn_refresh();
    tokio::spawn(sweep_terminal_sessions(state.clone()));
    tokio::spawn(loka_shared::feeds::poll_loop(state.world.clone(), schedule_notify.clone()));
    tokio::spawn(run_runner_schedules(state.clone(), schedule_notify));

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| origin.to_str().is_ok_and(|origin| auth::LocalPolicy::current().origin_ok(origin))))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderName::from_static(auth::APP_SESSION_HEADER),
        ]);

    let api = Router::new()
        .route(
            "/tasks",
            get(routes::tasks::list).post(routes::tasks::create),
        )
        .route(
            "/tasks/{id}",
            patch(routes::tasks::update).delete(routes::tasks::delete),
        )
        .route("/users", get(routes::users::list))
        .route("/config", get(routes::config::get))
        .route(
            "/world/agents",
            get(routes::world::list_agents).post(routes::world::create_agent),
        )
        .route(
            "/world/agents/{id}",
            get(routes::world::get_agent)
                .patch(routes::world::update_agent)
                .delete(routes::world::delete_agent),
        )
        .route("/world/agents/{id}/tidy", post(routes::world::tidy_agent))
        .route("/world/agents/{id}/reset-context", post(routes::world::reset_context))
        .route("/world/agents/{id}/context", get(routes::world::agent_context).patch(routes::world::update_context))
        .route("/world/agents/{id}/context/compact", post(routes::world::compact_context))
        .route("/world/agents/{id}/context/reset", post(routes::world::reset_agent_context))
        .route("/world/agents/{id}/fork", post(routes::world::fork_agent))
        .route(
            "/world/agents/{id}/messages",
            get(routes::world::list_messages).post(routes::world::post_message),
        )
        .route("/world/agents/{id}/messages/{message_id}", patch(routes::world::edit_message))
        .route("/world/agents/{id}/messages/{message_id}/rewind", post(routes::world::rewind_message))
        .route("/world/agents/{id}/messages/{message_id}/replies", get(routes::world::list_replies))
        .route("/world/agents/{id}/sessions", get(routes::world::list_sessions))
        .route("/world/agents/{id}/runs", get(routes::world::list_runs))
        .route("/world/agents/{id}/usage", get(routes::world::agent_usage))
        .route(
            "/world/agents/{id}/schedules",
            get(routes::world::list_schedules).post(routes::world::create_schedule),
        )
        .route(
            "/world/agents/{id}/schedules/{schedule_id}",
            patch(routes::world::toggle_schedule).delete(routes::world::delete_schedule),
        )
        .route(
            "/world/rules",
            get(routes::world::list_rules).post(routes::world::create_rule),
        )
        .route(
            "/world/rules/{id}",
            patch(routes::world::update_rule).delete(routes::world::delete_rule),
        )
        .route("/world/assistant", post(routes::world::ensure_assistant))
        .route("/world/onboarding", post(routes::onboarding::start))
        .route("/world/agents/{id}/asides", post(routes::world::ask_aside))
        .route("/world/agents/{id}/quick", post(routes::quick::start))
        .route("/world/quick", get(routes::quick::list))
        .route("/world/quick/{id}", get(routes::quick::read))
        .route("/world/quick/{id}/messages", post(routes::quick::send))
        .route("/world/quick/{id}/close", post(routes::quick::close))
        .route("/world/agents/{id}/direct", post(routes::world::start_direct))
        .route("/world/agents/{id}/voice", post(routes::voice::start))
        .route("/world/agents/{id}/voice/token", post(routes::voice::token))
        .route("/world/agents/{id}/voice/status", get(routes::voice::status))
        .route("/world/agents/{id}/voice/greeting", get(routes::voice::greeting))
        .route("/world/agents/{id}/voice/lines", post(routes::voice::lines))
        .route("/world/agents/{id}/feeds", get(routes::feeds::list).post(routes::feeds::create))
        .route(
            "/world/agents/{id}/feeds/{feed_id}",
            patch(routes::feeds::update).delete(routes::feeds::remove),
        )
        .route("/world/agents/{id}/feeds/{feed_id}/secret", post(routes::feeds::rotate))
        .route("/world/agents/{id}/feeds/{feed_id}/secret/reveal", post(routes::feeds::reveal))
        .route(
            "/world/agents/{id}/subscriptions",
            get(routes::world::get_subscriptions).put(routes::world::set_subscriptions),
        )
        .route(
            "/world/agents/{id}/watches",
            get(routes::github::watches).post(routes::github::add_watch),
        )
        .route("/world/agents/{id}/watches/{watch_id}", delete(routes::github::remove_watch))
        .route(
            "/world/mcp-servers",
            get(routes::world::list_mcp_servers).post(routes::world::install_mcp_server),
        )
        .route("/world/mcp-servers/{id}", delete(routes::world::delete_mcp_server))
        .route(
            "/world/skills",
            get(routes::world::list_skills).post(routes::world::install_skill),
        )
        .route("/world/skills/{id}", delete(routes::world::delete_skill))
        .route("/world/agents/{id}/kit", get(routes::world::agent_kit))
        .route(
            "/world/agents/{id}/omp-config",
            get(routes::omp_config::get)
                .put(routes::omp_config::save)
                .delete(routes::omp_config::delete),
        )
        .route("/world/agents/{id}/omp-config/equip", post(routes::omp_config::equip))
        .route(
            "/world/agents/{id}/mcp-servers/{server_id}",
            post(routes::world::attach_mcp).delete(routes::world::detach_mcp),
        )
        .route(
            "/world/agents/{id}/skills/{skill_id}",
            post(routes::world::attach_skill).delete(routes::world::detach_skill),
        )
        .route("/world/sessions", get(routes::world::list_session_briefs))
        .route("/world/sessions/{id}/runs", get(routes::world::session_runs))
        .route("/world/sessions/{id}/messages", post(routes::world::session_followup))
        .route("/world/sessions/{id}/review", post(routes::world::session_review))
        .route("/world/runs/{id}/text", get(routes::world::run_text))
        .route("/world/runs/{id}/events", get(routes::world::run_events))
        .route("/world/runs/{id}/retry", post(routes::world::retry_run))
        .route("/world/runs/{id}/stop", post(routes::world::stop_run))
        .route(
            "/world/uploads",
            post(routes::world::upload).layer(axum::extract::DefaultBodyLimit::max(routes::world::MAX_UPLOAD + 4096)),
        )
        .route("/world/uploads/{id}", get(routes::world::get_upload))
        .route("/world/notifications", get(routes::world::notifications))
        .route("/world/notifications/seen", post(routes::world::notifications_seen))
        .route("/world/notifications/clear", post(routes::world::notifications_clear))
        .route(
            "/world/runners",
            get(routes::runners::list).post(routes::runners::create),
        )
        .route(
            "/world/runners/{id}",
            patch(routes::runners::update).delete(routes::runners::revoke),
        )
        .route(
            "/world/agents/{id}/runner",
            get(routes::runners::get_agent_runner).patch(routes::runners::set_agent_runner),
        )
        .route(
            "/world/agents/{id}/handoff",
            post(routes::runners::handoff_agent),
        )
        .route(
            "/world/environment",
            get(routes::environment::list),
        )
        .route(
            "/world/environment/{name}",
            axum::routing::put(routes::environment::save)
                .delete(routes::environment::delete_entry),
        )
        .route(
            "/world/environment/providers/{id}/check",
            axum::routing::post(routes::environment::check_provider),
        )
        .route(
            "/world/environment/wanted/{agent_id}/{name}",
            axum::routing::delete(routes::environment::dismiss_wanted),
        )
        .route(
            "/world/environment/{name}/agents",
            axum::routing::put(routes::environment::set_agents),
        )
        .route(
            "/world/environment/{name}/variants/{environment}",
            axum::routing::put(routes::environment::save_variant).delete(routes::environment::delete_variant),
        )
        .route("/world/environment/choices", axum::routing::put(routes::environment::choose))
        .route(
            "/world/environment/import",
            axum::routing::post(routes::environment::import)
                .layer(axum::extract::DefaultBodyLimit::max(routes::environment::MAX_IMPORT_BYTES)),
        )
        .route(
            "/world/command-rules",
            get(routes::environment::list_command_rules).post(routes::environment::add_command_rule),
        )
        .route(
            "/world/command-rules/{id}",
            axum::routing::delete(routes::environment::delete_command_rule),
        )
        .route("/world/shared-keys", get(routes::environment::list_shared))
        .route(
            "/world/shared-keys/{name}",
            axum::routing::put(routes::environment::save_shared).delete(routes::environment::delete_shared),
        )
        .route("/world/shared-keys/{name}/agents", axum::routing::put(routes::environment::set_shared_agents))
        .route(
            "/world/channels",
            get(routes::world::list_channels).post(routes::world::create_channel),
        )
        .route("/world/starter-team", get(routes::starter_team::plan).post(routes::starter_team::build))
        .route("/world/setup-scan", post(routes::setup_scan::offer))
        .route("/world/teams", get(routes::teams::list).post(routes::teams::create))
        .route("/world/teams/{id}", patch(routes::teams::update).delete(routes::teams::delete))
        .route("/world/teams/{id}/members", post(routes::teams::add_members))
        .route("/world/teams/{id}/members/{kind}/{member_id}", delete(routes::teams::remove_member))
        .route("/world/dms", post(routes::world::open_dm))
        .route("/world/messages/{reference}", get(routes::world::resolve_message_link))
        .route("/world/search", get(routes::world::search))
        .route("/world/readiness", get(routes::readiness::get))
        .route("/world/embeds/slack", get(routes::world::slack_embed))
        .route("/world/channels/{id}", get(routes::world::get_channel))
        .route(
            "/world/channels/{id}/messages",
            get(routes::world::channel_messages).post(routes::world::post_channel),
        )
        .route("/world/channels/{id}/messages/{message_id}", patch(routes::world::edit_channel_message).delete(routes::world::delete_channel_message))
        .route("/world/channels/{id}/messages/{message_id}/replies", get(routes::world::channel_replies))
        .route("/world/channels/{id}/members", post(routes::world::add_channel_member))
        .route(
            "/world/channels/{id}/members/{kind}/{member_id}",
            delete(routes::world::remove_channel_member),
        )
        .route("/world/channels/{id}/read", post(routes::world::read_channel))
        .route("/world/me", get(routes::world::get_me).patch(routes::world::set_me))
        .route("/world/storage", get(routes::cleanup::report))
        .route("/world/storage/cleanup", post(routes::cleanup::cleanup))
        .route("/world/tokens", get(routes::tokens::list).post(routes::tokens::create))
        .route("/world/tokens/{id}", delete(routes::tokens::revoke))
        .route("/world/github", get(routes::github::get).patch(routes::github::update))
        .route("/world/quests", get(routes::world::list_quests))
        .route("/world/shell/{id}", get(routes::shell::get_run))
        .route("/world/terminal/sessions", post(routes::terminal::open_session))
        .route("/world/shell/{id}/cancel", post(routes::shell::cancel))
        .route("/world/shell/{id}/share", post(routes::shell::share))
        .route("/world/tasks/{id}/room", get(routes::world::quest_room))
        .route("/world/tasks/{id}/milestones", get(routes::world::list_milestones).post(routes::world::add_milestones))
        .route("/world/tasks/{id}/milestones/check", post(routes::world::check_milestones))
        .route("/world/milestones/{id}", patch(routes::world::update_milestone).delete(routes::world::delete_milestone))
        .route("/world/items", get(routes::world::list_items))
        .route("/world/items/{id}", patch(routes::world::update_item))
        .route("/world/regions", get(routes::loka::list_regions))
        .route("/world/regions/{id}", get(routes::loka::get_region))
        .route("/world/loka/presets", get(routes::loka::presets))
        .route("/world/npcs/{id}/talk", get(routes::loka::npc_history).post(routes::loka::talk_to_npc))
        .route("/world/challenges", get(routes::loka::list_challenges))
        .route("/world/challenges/{id}", get(routes::loka::get_challenge))
        .route("/world/challenges/{id}/attempts", post(routes::loka::submit_attempt))
        .route("/world/attempts/{id}", get(routes::loka::get_attempt))
        .route("/world/adventures", get(routes::loka::list_adventures))
        .route("/world/progress", get(routes::loka::progress))
        .route("/world/academy/offer", post(routes::loka::offer))
        .route("/world/academy", put(routes::loka::set_academy))
        .route("/world/reports", get(routes::reports::list).post(routes::reports::create))
        .route("/world/reports/find", get(routes::reports::find))
        .route("/world/reports/{id}", get(routes::reports::get).patch(routes::reports::update).delete(routes::reports::delete))
        .route("/world/reports/{id}/revisions", get(routes::reports::revisions))
        .route("/world/reports/{id}/revisions/{revision}", get(routes::reports::revision))
        .route("/world/reports/{id}/revisions/{revision}/restore", post(routes::reports::restore))
        .route("/world/todos", get(routes::todos::list).post(routes::todos::create))
        .route("/world/todos/order", post(routes::todos::reorder))
        .route("/world/todos/{id}", patch(routes::todos::update))
        .route("/world/todos/{id}/decide", post(routes::todos::decide))
        .route("/world/todos/{id}/proposals", delete(routes::todos::clear_proposals))
        .route("/world/events", get(routes::world::list_events))
        .route("/world/stream", get(routes::world::stream))
        .layer(middleware::from_fn_with_state(state.clone(), access::agent_guard))
        .layer(middleware::from_fn_with_state(state.clone(), access::role_middleware))
        .layer(middleware::from_fn_with_state(state.clone(), auth::auth_middleware));

    let runner_api = Router::new()
        .route("/{runner_id}/heartbeat", post(routes::runners::heartbeat))
        .route("/{runner_id}/claim", post(routes::runners::claim))
        .route("/{runner_id}/poll", post(routes::runners::poll))
        .route("/{runner_id}/shell/claim", post(routes::runners::shell_claim))
        .route("/{runner_id}/shell/{run_id}/output", post(routes::runners::shell_output))
        .route("/{runner_id}/shell/{run_id}/finish", post(routes::runners::shell_finish))
        .route("/{runner_id}/terminal/claim", post(routes::terminal::claim))
        .route("/{runner_id}/terminal/{session_id}/socket", get(routes::terminal::runner_socket))
        .route("/{runner_id}/slack", post(routes::runners::slack))
        .route("/{runner_id}/slack/watches", get(routes::runners::slack_watches))
        .route("/{runner_id}/github", post(routes::runners::github))
        .route("/{runner_id}/workspaces", post(routes::runners::workspaces))
        .route("/{runner_id}/stream", get(routes::runners::stream))
        .route(
            "/leases/{lease_id}/heartbeat",
            post(routes::runners::lease_heartbeat),
        )
        .route("/leases/{lease_id}/events", post(routes::runners::events))
        .route(
            "/leases/{lease_id}/complete",
            post(routes::runners::complete),
        )
        .route("/leases/{lease_id}/mcp", post(routes::runners::mcp))
        .route(
            "/leases/{lease_id}/media",
            post(routes::runners::media).layer(axum::extract::DefaultBodyLimit::max(loka_shared::media::MAX_VIDEO + 4096)),
        )
        .layer(tower_http::decompression::RequestDecompressionLayer::new());

    let mcp_api = Router::new()
        .route("/mcp", post(mcp::handle).get(mcp::no_stream).delete(mcp::no_stream))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(tower_http::decompression::RequestDecompressionLayer::new());

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(ready))
        .route("/webhooks/linear", post(routes::webhooks::linear_webhook))
        .route("/webhooks/clickup", post(routes::webhooks::clickup_webhook))
        .route(
            "/webhooks/feeds/{token}",
            post(routes::feeds::webhook).layer(axum::extract::DefaultBodyLimit::max(
                loka_shared::feeds::WEBHOOK_BODY_LIMIT,
            )),
        )
        .merge(mcp_api)
        .route("/api/terminal/socket", get(routes::terminal::browser_socket))
        .route("/api/app-session", get(auth::app_session_handoff))
        .nest("/api/runner", runner_api)
        .nest("/api", api.layer(tower_http::compression::CompressionLayer::new()))
        .route_layer(middleware::from_fn(metrics::track))
        .layer(cors)
        .with_state(state);

    let bind_host = std::env::var("API_BIND_HOST").ok().filter(|host| !host.trim().is_empty()).unwrap_or_else(|| auth::default_bind_host().into());
    let addr = SocketAddr::new(bind_host.parse()?, port);
    info!("API server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    keepalive::spawn()?;
    if auth::auth_disabled() {
        auth::app_session_secret();
        tracing::warn!(%addr, "AUTH_DISABLED=true: local pages that hold the app session act as the one local user (the desktop app hands its window the session; in development run loka-api login-link). Requests without it get 401. Never expose this API to other machines");
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;

    Ok(())
}

async fn watch_open_files(limit: Option<u64>) {
    loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        let Some(open) = loka_shared::limits::open_files() else { return };
        match limit {
            Some(limit) if open as u64 * 5 >= limit * 4 => {
                tracing::warn!(open, limit, "Open files close to the limit")
            }
            _ => info!(open, limit, "Open files"),
        }
    }
}

async fn sweep_terminal_sessions(state: AppState) {
    let cadence = std::env::var("WORLD_TERMINAL_SWEEP_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(5, 3600);
    loop {
        tokio::time::sleep(Duration::from_secs(cadence)).await;
        let stale = match state.world.stale_open_terminals(150).await {
            Ok(stale) => stale,
            Err(error) => {
                tracing::error!(%error, "terminal session sweep failed");
                continue;
            }
        };
        for session in stale {
            if state.term.is_live(session) {
                continue;
            }
            tracing::warn!(%session, "closing dead terminal session");
            let _ = state.world.close_terminal(session, "device_disconnected").await;
        }
    }
}

async fn ready(axum::extract::State(state): axum::extract::State<AppState>) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let check = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(state.world.pool());
    let database = matches!(tokio::time::timeout(Duration::from_secs(3), check).await, Ok(Ok(1)));
    let status = if database { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };
    (status, axum::Json(serde_json::json!({ "ok": database, "database": database })))
}

async fn sweep_runner_leases(state: AppState) {
    let mut last_tidy = std::time::Instant::now() - Duration::from_secs(3600);
    let mut last_stall_check = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if last_stall_check.elapsed() >= Duration::from_secs(60) {
            last_stall_check = std::time::Instant::now();
            match state.world.release_budget_pauses(None).await {
                Ok(released) if !released.is_empty() => {
                    tracing::info!(count = released.len(), "resumed agents after their daily budget reset");
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "daily budget release failed"),
            }
            match loka_shared::withdraw::stop_budget_paused_work(state.world.pool(), None).await {
                Ok(stopped) if !stopped.is_empty() => {
                    tracing::info!(runs = stopped.runs.len(), "stopped running work of agents over their budget");
                    routes::runners::finish_stopped(&state, &stopped).await;
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "stopping work over budget failed"),
            }
            match state.world.archived_due_for_deletion().await {
                Ok(due) => {
                    for (agent_id, name, owner) in due {
                        if let Err(error) = state.world.delete_agent(agent_id).await {
                            tracing::error!(%error, agent_id = %agent_id, "auto delete of an archived agent failed");
                            continue;
                        }
                        tracing::info!(agent_id = %agent_id, "deleted an archived agent on its auto delete date");
                        let event = loka_shared::world::NewEvent::new("agent.removed")
                            .payload(serde_json::json!({ "agent_id": agent_id, "name": name, "owner_user_id": owner, "auto_deleted": true }));
                        if let Err(error) = state.world.emit(event, None).await {
                            tracing::error!(%error, "failed to record an auto deleted agent");
                        }
                    }
                }
                Err(error) => tracing::error!(%error, "archived agent auto delete check failed"),
            }
            match state.runners.stalled_on_offline_device().await {
                Ok(stalled) => {
                    for (agent_id, device) in stalled {
                        let detail = format!(
                            "Work is waiting, but {device} has been offline for over 5 minutes. Open Loka on that device, or move this agent to another one."
                        );
                        tracing::info!(agent_id = %agent_id, device = %device, "agent waiting on an offline device");
                        if let Err(error) = state.world.set_agent_status(agent_id, "error", Some(&detail)).await {
                            tracing::error!(%error, agent_id = %agent_id, "failed to flag agent waiting on an offline device");
                        }
                    }
                }
                Err(error) => tracing::error!(%error, "offline device check failed"),
            }
        }
        if last_tidy.elapsed() >= Duration::from_secs(3600) {
            last_tidy = std::time::Instant::now();
            match state.world.prune_done_timers(7).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "removed finished timers older than a week"),
                Err(error) => tracing::error!(%error, "failed to prune finished timers"),
            }
            match state.world.tidy_failed_turns(None, 24).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "removed empty failed main turns"),
                Err(error) => tracing::error!(%error, "failed to tidy failed main turns"),
            }
        }
        let coordinator = loka_agent::runner_server::RunnerCoordinator::new(
            state.db.clone(),
            state.world.clone(),
        );
        match state.runners.pending_completions(20).await {
            Ok(pending) => {
                for (lease, completion) in pending {
                    match coordinator.complete(&lease, &completion).await {
                        Ok(()) => {
                            if let Err(error) = state
                                .runners
                                .finish_completion(lease.runner_id, lease.lease_id)
                                .await
                            {
                                tracing::error!(%error, lease_id = %lease.lease_id, "failed to close recovered completion");
                            }
                        }
                        Err(error) => {
                            tracing::error!(%error, lease_id = %lease.lease_id, "failed to recover runner completion");
                        }
                    }
                }
            }
            Err(error) => tracing::error!(%error, "failed to load pending runner completions"),
        }
        match state.runners.expire_stale().await {
            Ok(expired) if !expired.is_empty() => {
                tracing::warn!(count = expired.len(), "expired stale runner leases");
                if let Err(error) = coordinator.handle_expired(&expired).await {
                    tracing::error!(%error, "failed to finalize expired runner leases");
                }
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "runner lease sweep failed"),
        }
        if let Err(error) = state.runners.apply_pending_moves().await {
            tracing::error!(%error, "pending device moves failed");
        }
        match state.runners.expire_stale_shell_runs().await {
            Ok(expired) => {
                for ended in expired {
                    tracing::warn!(run_id = %ended.id, "shell run expired");
                    if let Err(error) = routes::shell::announce_shell_end(&state, &ended).await {
                        tracing::error!(%error, "failed to announce expired shell run");
                    }
                }
            }
            Err(error) => tracing::error!(%error, "shell run sweep failed"),
        }
    }
}

async fn run_runner_schedules(state: AppState, notify: Arc<tokio::sync::Notify>) {
    const MAX_SLEEP: Duration = Duration::from_secs(3600);
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let coordinator = loka_agent::runner_server::RunnerCoordinator::new(
            state.db.clone(),
            state.world.clone(),
        );
        if let Err(error) = coordinator.tick_schedules().await {
            tracing::error!(%error, "runner schedule pass failed");
        }
        let next = match (
            state.world.next_runner_schedule_at().await,
            state.world.next_timer_at().await,
        ) {
            (Ok(a), Ok(b)) => Some(match (a, b) {
                (Some(a), Some(b)) => a.min(b),
                (Some(at), None) | (None, Some(at)) => at,
                (None, None) => {
                    chrono::Utc::now() + chrono::Duration::seconds(MAX_SLEEP.as_secs() as i64)
                }
            }),
            (Err(error), _) | (_, Err(error)) => {
                tracing::error!(%error, "could not read next runner schedule time");
                None
            }
        };
        let wait = next
            .and_then(|at| (at - chrono::Utc::now()).to_std().ok())
            .unwrap_or(Duration::from_secs(30))
            .min(MAX_SLEEP)
            .max(Duration::from_millis(250));
        let _ = tokio::time::timeout(wait, notified).await;
    }
}

async fn relay_world_events(database_url: String, tx: tokio::sync::broadcast::Sender<loka_shared::world::EventWake>) {
    loop {
        match sqlx::postgres::PgListener::connect(&database_url).await {
            Ok(mut listener) => {
                if listener.listen(loka_shared::world::CHANNEL_EVENTS).await.is_ok() {
                    let _ = tx.send(loka_shared::world::EventWake::default());
                    while let Ok(n) = listener.recv().await {
                        let _ = tx.send(loka_shared::world::EventWake::from_payload(n.payload()));
                    }
                }
                tracing::warn!("world event listener dropped, reconnecting");
            }
            Err(e) => tracing::error!(error = %e, "world event listener failed to connect"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn relay_runner_events(
    database_url: String,
    pool: sqlx::PgPool,
    tx: runner_wakes::RunnerWakes,
    schedules: Arc<tokio::sync::Notify>,
) {
    loop {
        match sqlx::postgres::PgListener::connect(&database_url).await {
            Ok(mut listener) => {
                let listened = listener
                    .listen_all([
                        loka_shared::runners::CHANNEL_RUNNER_JOBS,
                        loka_shared::world::CHANNEL_INBOX,
                        loka_shared::world::CHANNEL_SCHEDULES,
                    ])
                    .await;
                if listened.is_ok() {
                    schedules.notify_waiters();
                    if let Ok(runner_ids) = sqlx::query_scalar::<_, uuid::Uuid>(
                        "SELECT DISTINCT runner_id FROM (
                           SELECT a.runner_id FROM agent_profiles a
                           JOIN agent_inbox i ON i.agent_id = a.id
                           WHERE a.runner_id IS NOT NULL AND i.status = 'pending'
                           UNION
                           SELECT r.runner_id FROM session_runs r
                           WHERE r.runner_id IS NOT NULL AND r.status = 'queued'
                         ) pending",
                    )
                    .fetch_all(&pool)
                    .await
                    {
                        for runner_id in runner_ids {
                            tx.wake(runner_id);
                        }
                    }
                    while let Ok(notification) = listener.recv().await {
                        match notification.channel() {
                            loka_shared::runners::CHANNEL_RUNNER_JOBS => {
                                if let Ok(runner_id) = notification.payload().parse() {
                                    tx.wake(runner_id);
                                }
                            }
                            loka_shared::world::CHANNEL_INBOX => {
                                if let Ok(agent_id) = notification.payload().parse::<uuid::Uuid>() {
                                    let runners = loka_shared::runners::RunnerStore::new(pool.clone());
                                    if let Ok(Some(runner_id)) = runners.runner_for_stranded(agent_id).await {
                                        match runners.adopt_stranded(runner_id, Some(agent_id)).await {
                                            Ok(adopted) if !adopted.is_empty() => tracing::info!(%agent_id, %runner_id, "Put an agent with no device on its owner's online runner"),
                                            Ok(_) => {}
                                            Err(error) => tracing::warn!(%error, "could not place an agent on a runner"),
                                        }
                                    }
                                    if let Ok(Some(runner_id)) =
                                        sqlx::query_scalar::<_, uuid::Uuid>(
                                            "SELECT runner_id FROM agent_profiles WHERE id = $1",
                                        )
                                        .bind(agent_id)
                                        .fetch_optional(&pool)
                                        .await
                                    {
                                        tx.wake(runner_id);
                                        if let Ok(Some(secs)) = loka_shared::world::WorldStore::new(pool.clone())
                                            .inbox_settle_secs(Some(agent_id), false)
                                            .await
                                        {
                                            let tx = tx.clone();
                                            tokio::spawn(async move {
                                                tokio::time::sleep(Duration::from_secs_f64(secs + 0.25)).await;
                                                tx.wake(runner_id);
                                            });
                                        }
                                    }
                                }
                            }
                            loka_shared::world::CHANNEL_SCHEDULES => schedules.notify_waiters(),
                            _ => {}
                        }
                    }
                }
                tracing::warn!("runner event listener dropped, reconnecting");
            }
            Err(error) => tracing::error!(%error, "runner event listener failed to connect"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn print_login_link(base: Option<String>) -> anyhow::Result<()> {
    let Some(secret) = auth::saved_app_session() else {
        anyhow::bail!("No app session found. Start the API with AUTH_DISABLED=true and LOKA_APP_SESSION_FILE set first.");
    };
    let base = base
        .or_else(|| std::env::var("WORLD_APP_URL").ok())
        .map(|base| base.trim().to_string())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| format!("http://localhost:{}", std::env::var("FRONTEND_PORT").ok().filter(|port| port.parse::<u16>().is_ok()).unwrap_or_else(|| "3000".into())));
    println!("{}", auth::login_link(&base, &secret));
    Ok(())
}
