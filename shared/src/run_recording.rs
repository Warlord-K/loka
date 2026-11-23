use std::path::Path;

use anyhow::Result;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::cli_errors::{classify, FailureKind};
use crate::runners::{RunnerUsage, WORK_RUN_KINDS};
use crate::world::{emit_in, NewEvent, SESSION_STARTED_LIKE};

pub const EVENT_MAX_CHARS: usize = 4_000;
pub const SPACE_RETRY_SECONDS: i64 = 60;
pub const LOW_DISK_BYTES: f64 = 5.0 * 1024.0 * 1024.0 * 1024.0;

pub fn disk_space(path: &Path) -> Option<(f64, f64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as f64;
    Some((stats.f_bavail as f64 * block, stats.f_blocks as f64 * block))
}

pub fn free_bytes(root: &Path) -> f64 {
    disk_space(root).map(|(free, _)| free).unwrap_or(f64::MAX)
}

pub fn floor_for(total: Option<f64>) -> f64 {
    total.map_or(LOW_DISK_BYTES, |total| LOW_DISK_BYTES.min(total * 0.1))
}

pub fn run_floor(root: &Path) -> f64 {
    floor_for(disk_space(root).map(|(_, total)| total))
}

pub fn has_room(root: &Path) -> bool {
    free_bytes(root) >= run_floor(root)
}

pub fn no_room_message(free: f64) -> String {
    format!(
        "No space left on device: this device has only {:.1} GB free, so Loka did not start this run. It waits and starts by itself once there is room.",
        free / 1e9
    )
}

pub fn cleared_note(freed: u64) -> String {
    if freed == 0 {
        return String::new();
    }
    format!(" Loka already cleared {:.1} GB of build output and test data from quests that were not running, which was not enough.", freed as f64 / 1e9)
}

pub fn no_room_to_start_message(free: f64, freed: u64) -> String {
    format!(
        "No space left on device: this device has only {:.1} GB free, so Loka did not create the quest's workspace.{} Start the quest again once there is room.",
        free / 1e9,
        cleared_note(freed)
    )
}

pub fn shows_activity(kind: &str) -> bool {
    !matches!(kind, "aside" | "publish" | "action")
}

pub fn accounts_spend(kind: &str) -> bool {
    !matches!(kind, "publish" | "action")
}

pub fn waits_for_space(kind: &str, error: &str) -> bool {
    WORK_RUN_KINDS.contains(&kind) && classify(error).is_some_and(|failure| failure.kind == FailureKind::DiskFull)
}

pub async fn requeue_for_space(tx: &mut Transaction<'_, Postgres>, run_id: Uuid, lease_id: Option<Uuid>) -> Result<bool> {
    let steps = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM session_run_events WHERE run_id = $1 AND kind IN ('text', 'tool') AND body NOT LIKE $2",
    )
    .bind(run_id)
    .bind(SESSION_STARTED_LIKE)
    .fetch_one(&mut **tx)
    .await?;
    let open_quest = sqlx::query_scalar::<_, bool>(
        "SELECT NOT EXISTS (SELECT 1 FROM session_runs r JOIN agent_tasks t ON t.id = r.task_id
                            WHERE r.id = $1 AND t.status IN ('cancelled', 'done'))",
    )
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?;
    if steps > 0 || !open_quest {
        return Ok(false);
    }
    let already_waiting = sqlx::query_scalar::<_, bool>("SELECT waiting_for IS NOT NULL FROM session_runs WHERE id = $1 FOR UPDATE")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or(false);
    let requeued = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, String)>(
        "UPDATE session_runs SET status = 'queued', waiting_for = 'disk', claimed_by = NULL, started_at = NULL,
           heartbeat_at = NULL, lease_expires_at = NULL, lease_id = NULL, error = NULL,
           not_before = NOW() + make_interval(secs => $3::double precision)
         WHERE id = $1 AND status = 'running' AND lease_id IS NOT DISTINCT FROM $2
         RETURNING session_id, agent_id, task_id, kind",
    )
    .bind(run_id)
    .bind(lease_id)
    .bind(SPACE_RETRY_SECONDS as f64)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((session_id, agent_id, task_id, kind)) = requeued else {
        return Ok(false);
    };
    if !already_waiting {
        emit_waiting(tx, run_id, session_id, agent_id, task_id, &kind, true).await?;
    }
    Ok(true)
}

pub async fn emit_waiting(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    session_id: Uuid,
    agent_id: Uuid,
    task_id: Option<Uuid>,
    kind: &str,
    waiting: bool,
) -> Result<()> {
    emit_in(
        tx,
        &NewEvent::new("run.waiting").agent(agent_id).session(session_id).run(run_id).task(task_id).payload(serde_json::json!({
            "waiting": waiting,
            "reason": "disk",
            "run_kind": kind,
            "task_id": task_id,
            "quiet": true,
        })),
        None,
    )
    .await?;
    Ok(())
}

pub async fn write_session_after_run(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    cli_session_id: Option<&str>,
    context_tokens: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_sessions
         SET cli_session_id = COALESCE($2, cli_session_id),
             summary = CASE WHEN role = 'main' AND $2 IS NOT NULL THEN NULL ELSE summary END,
             context_tokens = COALESCE($3, context_tokens), updated_at = NOW()
         WHERE id = $1",
    )
    .bind(session_id)
    .bind(cli_session_id)
    .bind(context_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct Reported {
    pub cost_usd: f64,
    pub usage: RunnerUsage,
}

#[derive(Debug, Clone)]
pub struct Accounted {
    pub baseline: Value,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

pub fn account(backend: &str, model: &str, held: &Value, reported: &Reported) -> Accounted {
    let previous_f64 = |key: &str| held.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let mut baseline = if held.is_object() { held.clone() } else { serde_json::json!({}) };
    let usage = if backend == "codex" {
        reported.usage.record(&mut baseline);
        reported.usage.since(held)
    } else {
        reported.usage
    };
    let RunnerUsage { input_tokens, cached_input_tokens, output_tokens, .. } = usage;
    let cost_usd = if crate::backends::cost_per_run(backend) && reported.cost_usd > 0.0 {
        reported.cost_usd
    } else if reported.cost_usd > 0.0 {
        let previous = previous_f64("cost_usd");
        if let Some(object) = baseline.as_object_mut() {
            object.insert("cost_usd".into(), serde_json::json!(reported.cost_usd));
        }
        crate::pricing::session_cost_delta(reported.cost_usd, previous, model, input_tokens, cached_input_tokens, output_tokens)
    } else {
        crate::pricing::cost(model, input_tokens, cached_input_tokens, output_tokens)
    };
    Accounted {
        baseline,
        cost_usd,
        input_tokens: input_tokens as i64,
        cached_input_tokens: cached_input_tokens as i64,
        output_tokens: output_tokens as i64,
    }
}

pub fn raise_baseline(current: &Value, reported: &Value) -> Value {
    let mut raised = if current.is_object() { current.clone() } else { serde_json::json!({}) };
    if let (Some(target), Some(reported)) = (raised.as_object_mut(), reported.as_object()) {
        for (key, value) in reported.iter().filter(|(_, value)| value.is_number()) {
            let higher = target.get(key).and_then(Value::as_f64).is_none_or(|held| value.as_f64().is_some_and(|new| new > held));
            if higher {
                target.insert(key.clone(), value.clone());
            }
        }
    }
    raised
}

async fn store_raised_baseline(tx: &mut Transaction<'_, Postgres>, session_id: Uuid, held: &Value, reported_baseline: &Value) -> Result<()> {
    let raised = raise_baseline(held, reported_baseline);
    if &raised != held {
        sqlx::query("UPDATE agent_sessions SET usage_baseline = $2 WHERE id = $1").bind(session_id).bind(raised).execute(&mut **tx).await?;
    }
    Ok(())
}

pub async fn bill_session(tx: &mut Transaction<'_, Postgres>, session_id: Uuid, reported: &Reported) -> Result<Option<Accounted>> {
    let held = sqlx::query_as::<_, (String, String, Value)>("SELECT backend, model, usage_baseline FROM agent_sessions WHERE id = $1 FOR UPDATE")
        .bind(session_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some((backend, model, baseline)) = held else {
        return Ok(None);
    };
    let accounted = account(&backend, &model, &baseline, reported);
    store_raised_baseline(tx, session_id, &baseline, &accounted.baseline).await?;
    Ok(Some(accounted))
}

pub async fn raise_session_baseline(tx: &mut Transaction<'_, Postgres>, session_id: Uuid, reported_baseline: &Value) -> Result<()> {
    let held = sqlx::query_scalar::<_, Value>("SELECT usage_baseline FROM agent_sessions WHERE id = $1 FOR UPDATE")
        .bind(session_id)
        .fetch_optional(&mut **tx)
        .await?;
    match held {
        Some(held) => store_raised_baseline(tx, session_id, &held, reported_baseline).await,
        None => Ok(()),
    }
}

pub async fn keep_spend_of_closed_run(tx: &mut Transaction<'_, Postgres>, run_id: Uuid, session_id: Uuid, reported: &Reported) -> Result<bool> {
    let closed = sqlx::query_scalar::<_, bool>("SELECT status NOT IN ('queued', 'running') FROM session_runs WHERE id = $1 FOR UPDATE")
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or(false);
    if !closed {
        return Ok(false);
    }
    let Some(spend) = bill_session(tx, session_id, reported).await? else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE session_runs SET cost_usd = GREATEST(cost_usd, $2), input_tokens = GREATEST(input_tokens, $3),
           cached_input_tokens = GREATEST(cached_input_tokens, $4), output_tokens = GREATEST(output_tokens, $5)
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(spend.cost_usd)
    .bind(spend.input_tokens)
    .bind(spend.cached_input_tokens)
    .bind(spend.output_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_disk_uses_a_tenth_of_itself_as_the_floor() {
        assert_eq!(floor_for(Some(500.0e9)), LOW_DISK_BYTES);
        assert_eq!(floor_for(None), LOW_DISK_BYTES);
        assert_eq!(floor_for(Some(20.0e9)), 2.0e9);
    }

    #[test]
    fn a_late_baseline_only_ever_raises_what_the_session_holds() {
        let held = serde_json::json!({"input_tokens": 900, "output_tokens": 50, "cost_usd": 1.5});
        let late = serde_json::json!({"input_tokens": 400, "cached_input_tokens": 70, "output_tokens": 80, "cost_usd": 0.4});
        assert_eq!(raise_baseline(&held, &late), serde_json::json!({"input_tokens": 900, "cached_input_tokens": 70, "output_tokens": 80, "cost_usd": 1.5}));
        assert_eq!(raise_baseline(&serde_json::json!({}), &late), late);
        assert_eq!(raise_baseline(&held, &serde_json::json!({})), held);
    }

    #[test]
    fn the_message_of_a_run_that_did_not_start_is_the_class_that_waits() {
        let message = no_room_message(1.0e9);
        assert!(waits_for_space("task", &message), "{message}");
        assert!(!waits_for_space("direct", &message));
    }
}
