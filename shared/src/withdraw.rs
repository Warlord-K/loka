use anyhow::Result;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub const STOPPED_BY: &str = "Loka";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoppedWork {
    pub runs: Vec<Uuid>,
    pub on_runners: Vec<Uuid>,
    pub runners: Vec<Uuid>,
}

impl StoppedWork {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

pub const BUDGET_PAUSES: [&str; 2] = ["Daily budget of", "Organization daily budget of"];

pub fn is_budget_pause(reason: &str) -> bool {
    BUDGET_PAUSES.iter().any(|prefix| reason.starts_with(prefix))
}

pub fn budget_stop_error(reason: &str) -> String {
    format!("Stopped by {STOPPED_BY}. {reason} Resume this work on the same quest once the budget resets or is raised.")
}

pub async fn stop_budget_paused_work(pool: &PgPool, near: Option<Uuid>) -> Result<StoppedWork> {
    let paused: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT a.id, a.paused_reason FROM agent_profiles a
         JOIN session_runs r ON r.agent_id = a.id AND r.status = 'running' AND r.stop_requested_by IS NULL
         WHERE (starts_with(COALESCE(a.paused_reason, ''), $2) OR starts_with(COALESCE(a.paused_reason, ''), $3))
           AND ($1::uuid IS NULL OR a.id = $1 OR a.org_id = (SELECT org_id FROM agent_profiles WHERE id = $1))",
    )
    .bind(near)
    .bind(BUDGET_PAUSES[0])
    .bind(BUDGET_PAUSES[1])
    .fetch_all(pool)
    .await?;
    if paused.is_empty() {
        return Ok(StoppedWork::default());
    }
    let mut tx = pool.begin().await?;
    let mut all = StoppedWork::default();
    for (agent, reason) in paused {
        let stopped = stop_running(&mut tx, &[agent], &budget_stop_error(&reason)).await?;
        all.runs.extend(stopped.runs);
        all.on_runners.extend(stopped.on_runners);
        all.runners.extend(stopped.runners);
    }
    tx.commit().await?;
    all.runners.sort();
    all.runners.dedup();
    announce_stopped(pool, &all).await;
    Ok(all)
}

async fn stop_running(tx: &mut Transaction<'_, Postgres>, agents: &[Uuid], error: &str) -> Result<StoppedWork> {
    let rows: Vec<(Uuid, bool, Option<Uuid>)> = sqlx::query_as(
        "UPDATE session_runs SET stop_requested_by = $2
         WHERE agent_id = ANY($1) AND status = 'running' AND stop_requested_by IS NULL
         RETURNING id, lease_id IS NOT NULL,
           COALESCE((SELECT l.runner_id FROM runner_leases l WHERE l.id = session_runs.lease_id), session_runs.runner_id)",
    )
    .bind(agents)
    .bind(STOPPED_BY)
    .fetch_all(&mut **tx)
    .await?;
    let on_runners: Vec<Uuid> = rows.iter().filter(|(_, leased, _)| *leased).map(|(id, _, _)| *id).collect();
    let mut runners: Vec<Uuid> = rows.iter().filter_map(|(_, _, runner)| *runner).collect();
    runners.sort();
    runners.dedup();
    if !on_runners.is_empty() {
        sqlx::query("UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW() WHERE id = ANY($1) AND status = 'running'")
            .bind(&on_runners)
            .bind(error)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE runner_leases SET status = 'cancelled', finished_at = NOW() WHERE run_id = ANY($1) AND status = 'active'")
            .bind(&on_runners)
            .execute(&mut **tx)
            .await?;
    }
    Ok(StoppedWork { runs: rows.into_iter().map(|(id, _, _)| id).collect(), on_runners, runners })
}

pub(crate) async fn announce_stopped(pool: &PgPool, stopped: &StoppedWork) {
    for id in &stopped.runs {
        let _ = sqlx::query("SELECT pg_notify('session_runs', $1)").bind(id.to_string()).execute(pool).await;
    }
    for runner in &stopped.runners {
        let _ = sqlx::query("SELECT pg_notify('runner_jobs', $1)").bind(runner.to_string()).execute(pool).await;
    }
    let _ = sqlx::query("SELECT pg_notify('session_runs', ''), pg_notify('agent_inbox', '')").execute(pool).await;
}
