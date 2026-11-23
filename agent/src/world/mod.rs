mod feeds_tools;
mod items_tools;
pub(crate) mod kit;
pub mod mcp;
pub(crate) mod prompts;
mod loka_tools;
mod music_tools;
mod quests;
mod reports_tools;
mod canvas_tools;
mod practice_tools;
mod runs;
mod schedule;
mod teams_tools;
pub mod tool_catalog;
mod turn;
pub mod warm;
mod watch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use loka_shared::world::{
    WorldStore, CHANNEL_INBOX, CHANNEL_RUNS, CHANNEL_SCHEDULES,
};
use sqlx::postgres::PgListener;
use tokio::sync::{Notify, Semaphore};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;

const SWEEP_EVERY: Duration = Duration::from_secs(60);
const AGENT_CLAIM_STALE_SECS: i64 = 600;
const RUN_HEARTBEAT_STALE_SECS: i64 = 300;

pub struct Ctx {
    pub config: Config,
    pub db: Database,
    pub store: WorldStore,
    pub worker: String,
    pub owner: String,
    pub max_review_rounds: i32,
    pub reviews_enabled: bool,
    pub warm: Arc<warm::Pool>,
    pub(crate) bus: Arc<Bus>,
}

#[derive(Default)]
pub(crate) struct Bus {
    inbox: Notify,
    pub(crate) runs: Notify,
    schedules: Notify,
    feeds: Arc<Notify>,
}

impl Bus {
    fn wake_all(&self) {
        self.inbox.notify_waiters();
        self.runs.notify_waiters();
        self.schedules.notify_waiters();
        self.feeds.notify_waiters();
    }
}

pub async fn run(
    config: Config,
    db: Database,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
) -> Result<()> {
    let store = WorldStore::new(db.pool().clone());
    let worker = format!(
        "{}-{}-{}",
        hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or_default(),
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let budget = crate::resources::Budget::detect();
    let inbox_slots = budget.chat_slots;
    let run_total = budget.work_slots;
    let bus = Arc::new(Bus::default());
    let ctx = Arc::new(Ctx {
        max_review_rounds: env_num("WORLD_MAX_REVIEW_ROUNDS", 2) as i32,
        reviews_enabled: std::env::var("WORLD_REVIEWS").map(|v| v != "off").unwrap_or(true),
        owner: std::env::var("WORLD_OWNER_NAME").unwrap_or_else(|_| "the team".to_string()),
        warm: warm::Pool::new(budget.warm_sessions),
        bus: bus.clone(),
        config,
        db,
        store,
        worker,
    });
    info!(worker = %ctx.worker, "Loka runtime starting");
    loka_shared::pricing::spawn_refresh();
    if crate::mcp_hub::enabled() {
        let mut socket = ctx.config.workspaces_dir.join(".mcp-hub.sock");
        if socket.as_os_str().len() > 100 {
            socket = std::env::temp_dir().join(format!("world-mcp-hub-{}.sock", std::process::id()));
        }
        crate::mcp_hub::start(socket).await?;
    }
    if crate::key_proxy::enabled() {
        crate::key_proxy::start().await?;
    }
    if crate::cmd_hub::enabled() {
        let mut socket = ctx.config.workspaces_dir.join(".cmd-hub.sock");
        if socket.as_os_str().len() > 100 {
            socket = std::env::temp_dir().join(format!("world-cmd-hub-{}.sock", std::process::id()));
        }
        crate::cmd_hub::start(socket, ctx.config.workspaces_dir.join(".cmd-hub")).await?;
    }

    tokio::spawn(listen(ctx.config.database_url.clone(), bus.clone()));

    let run_slots = Arc::new(Semaphore::new(run_total));

    let mut handles = Vec::new();
    for _ in 0..inbox_slots {
        handles.push(tokio::spawn(inbox_loop(ctx.clone(), bus.clone(), shutdown.clone())));
    }
    handles.push(tokio::spawn(run_loop(ctx.clone(), bus.clone(), run_slots.clone(), shutdown.clone())));
    handles.push(tokio::spawn(schedule::run(ctx.clone(), bus.clone(), shutdown.clone())));
    handles.push(tokio::spawn(loka_shared::feeds::poll_loop(ctx.store.clone(), bus.feeds.clone())));
    handles.push(tokio::spawn(ctx.warm.clone().reap_loop()));
    handles.push(tokio::spawn(sweep_loop(ctx.clone(), bus.clone(), shutdown.clone())));

    while !shutdown.load(Ordering::SeqCst) {
        shutdown_notify.notified().await;
    }
    info!("Loka runtime draining");
    bus.wake_all();
    let total = run_total as u32;
    let _ = tokio::time::timeout(Duration::from_secs(600), run_slots.acquire_many(total)).await;
    for h in handles {
        h.abort();
    }
    ctx.warm.clear();
    Ok(())
}

async fn listen(database_url: String, bus: Arc<Bus>) {
    let options = match database_url.parse::<sqlx::postgres::PgConnectOptions>() {
        Ok(options) => options.application_name(loka_shared::runners::WORLD_WORKER_APP),
        Err(e) => {
            error!(error = %e, "Invalid DATABASE_URL for the notification listener");
            return;
        }
    };
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_lazy_with(options);
    loop {
        match PgListener::connect_with(&pool).await {
            Ok(mut listener) => {
                if let Err(e) = listener
                    .listen_all([CHANNEL_INBOX, CHANNEL_RUNS, CHANNEL_SCHEDULES])
                    .await
                {
                    error!(error = %e, "LISTEN failed");
                } else {
                    info!("Listening for world notifications");
                    bus.wake_all();
                    loop {
                        match listener.recv().await {
                            Ok(n) => match n.channel() {
                                CHANNEL_INBOX => bus.inbox.notify_waiters(),
                                CHANNEL_RUNS => bus.runs.notify_waiters(),
                                CHANNEL_SCHEDULES => {
                                    bus.schedules.notify_waiters();
                                    bus.feeds.notify_waiters();
                                }
                                _ => {}
                            },
                            Err(e) => {
                                warn!(error = %e, "Notification stream dropped, reconnecting");
                                bus.wake_all();
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => error!(error = %e, "Failed to connect notification listener"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn inbox_loop(ctx: Arc<Ctx>, bus: Arc<Bus>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        let notified = bus.inbox.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        match ctx.store.claim_agent_inbox(&ctx.worker).await {
            Ok(Some(agent_id)) => {
                let span = tracing::info_span!("turn", agent = %agent_id);
                if let Err(e) = tracing::Instrument::instrument(turn::process(&ctx, agent_id), span).await {
                    error!(agent = %agent_id, error = %e, "Agent turn failed");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                if let Err(e) = ctx.store.release_agent_inbox(agent_id, &ctx.worker).await {
                    error!(agent = %agent_id, error = %e, "Failed to release agent claim");
                }
                if let Err(e) = loka_shared::runners::RunnerStore::new(ctx.store.pool().clone())
                    .apply_pending_move(agent_id)
                    .await
                {
                    error!(agent = %agent_id, error = %e, "Failed to apply pending device move");
                }
            }
            Ok(None) => {
                let wait = match ctx.store.inbox_settle_secs(None, true).await {
                    Ok(Some(secs)) => SWEEP_EVERY.min(Duration::from_secs_f64(secs + 0.25)),
                    _ => SWEEP_EVERY,
                };
                let _ = tokio::time::timeout(wait, notified).await;
            }
            Err(e) => {
                error!(error = %e, "Inbox claim failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_loop(ctx: Arc<Ctx>, bus: Arc<Bus>, slots: Arc<Semaphore>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            return;
        };
        let notified = bus.runs.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        match ctx.store.claim_queued_run(&ctx.worker).await {
            Ok(Some(run)) => {
                let ctx = ctx.clone();
                let span = tracing::info_span!("run", run = %run.id, agent = %run.agent_id, kind = %run.kind);
                tokio::spawn(tracing::Instrument::instrument(async move {
                    let run_id = run.id;
                    if let Err(e) = runs::execute(&ctx, run).await {
                        error!(run = %run_id, error = %e, "Run handling failed");
                    }
                    drop(permit);
                }, span));
            }
            Ok(None) => {
                drop(permit);
                let _ = tokio::time::timeout(SWEEP_EVERY, notified).await;
            }
            Err(e) => {
                drop(permit);
                error!(error = %e, "Run claim failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn sweep_loop(ctx: Arc<Ctx>, bus: Arc<Bus>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(SWEEP_EVERY).await;
        match ctx
            .store
            .sweep_stale(AGENT_CLAIM_STALE_SECS, RUN_HEARTBEAT_STALE_SECS)
            .await
        {
            Ok(lost) => {
                for run in lost {
                    warn!(run = %run.id, "Run lost its worker");
                    if let Err(e) = runs::fail_stale(
                        &ctx,
                        &run,
                        "the worker running this session stopped responding",
                    )
                    .await
                    {
                        error!(error = %e, "Failed to handle lost run");
                    }
                }
            }
            Err(e) => error!(error = %e, "Sweep failed"),
        }
        if let Err(e) = loka_shared::runners::RunnerStore::new(ctx.store.pool().clone())
            .apply_pending_moves()
            .await
        {
            error!(error = %e, "Pending device moves failed");
        }
        if !ctx.config.github_token.is_empty() {
            watch::check_due(&ctx).await;
        }
        bus.inbox.notify_waiters();
        bus.runs.notify_waiters();
    }
}

pub fn agent_home(ctx: &Ctx, slug: &str) -> std::path::PathBuf {
    ctx.config.workspaces_dir.join("agents").join(slug)
}

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &usize| *v > 0)
        .unwrap_or(default)
}
