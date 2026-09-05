use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use loka_shared::world::AgentSchedule;
use tracing::{error, info, warn};

use super::{Bus, Ctx};

const MAX_SLEEP: Duration = Duration::from_secs(3600);

pub(super) async fn run(ctx: Arc<Ctx>, bus: Arc<Bus>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        let notified = bus.schedules.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Err(e) = tick(&ctx).await {
            error!(error = %e, "Schedule pass failed");
        }
        let next = match (ctx.store.next_schedule_at().await, ctx.store.next_timer_at().await) {
            (Ok(a), Ok(b)) => Ok(match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        let wait = match next {
            Ok(Some(at)) => (at - Utc::now()).to_std().unwrap_or(Duration::ZERO).min(MAX_SLEEP),
            Ok(None) => MAX_SLEEP,
            Err(e) => {
                error!(error = %e, "Could not read next schedule time");
                Duration::from_secs(30)
            }
        };
        let _ = tokio::time::timeout(wait.max(Duration::from_millis(250)), notified).await;
    }
}

async fn tick(ctx: &Ctx) -> anyhow::Result<()> {
    for t in ctx.store.fire_due_timers().await? {
        info!(timer = %t.name, "Timer fired");
    }
    for s in ctx.store.schedules_missing_next_run().await? {
        let (next, disable) = next_fire(&s, Utc::now());
        ctx.store.set_schedule_next_run(s.id, next, disable && next.is_none()).await?;
    }
    for s in ctx.store.claim_due_schedules().await? {
        if ctx.store.fire_schedule(&s).await? {
            info!(schedule = %s.name, agent = %s.agent_id, "Schedule fired");
        } else {
            info!(schedule = %s.name, agent = %s.agent_id, "Schedule skipped, its last tick is still waiting");
        }
        let after = Utc::now() + chrono::Duration::seconds(1);
        let (next, disable) = if s.cron_expression.is_some() {
            next_fire(&s, after)
        } else {
            (None, true)
        };
        ctx.store.set_schedule_next_run(s.id, next, disable).await?;
    }
    Ok(())
}

fn next_fire(s: &AgentSchedule, after: DateTime<Utc>) -> (Option<DateTime<Utc>>, bool) {
    if let Some(expr) = &s.cron_expression {
        match CronSchedule::from_str(&loka_shared::cron_util::normalize(expr)) {
            Ok(c) => return (c.after(&after).next(), false),
            Err(e) => {
                warn!(schedule = %s.name, error = %e, "Invalid cron expression");
                return (None, true);
            }
        }
    }
    match s.run_at {
        Some(at) => (Some(at), false),
        None => (None, true),
    }
}
