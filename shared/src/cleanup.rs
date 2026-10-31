use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::world::WorldStore;

pub const MIN_DAYS: i32 = 7;
pub const MAX_DAYS: i32 = 3650;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupKind {
    RunLogs,
    Chats,
    Quests,
    Activity,
}

impl CleanupKind {
    pub const ALL: [CleanupKind; 4] = [CleanupKind::RunLogs, CleanupKind::Chats, CleanupKind::Quests, CleanupKind::Activity];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "run_logs" | "logs" => Some(Self::RunLogs),
            "chats" | "messages" => Some(Self::Chats),
            "quests" => Some(Self::Quests),
            "activity" | "events" => Some(Self::Activity),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::RunLogs => "run_logs",
            Self::Chats => "chats",
            Self::Quests => "quests",
            Self::Activity => "activity",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::RunLogs => "step logs, prompts and raw output of finished runs (cost and run history stay)",
            Self::Chats => "chat messages with your agents",
            Self::Quests => "finished, failed and cancelled quests with their sessions, runs and logs (lifetime cost stays)",
            Self::Activity => "activity feed events, including cleared bell items",
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AgentStorage {
    pub agent_id: Uuid,
    pub name: String,
    pub slug: String,
    pub messages: i64,
    pub message_bytes: i64,
    pub runs: i64,
    pub run_bytes: i64,
    pub steps: i64,
    pub step_bytes: i64,
    pub events: i64,
    pub event_bytes: i64,
    pub total_bytes: i64,
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PastCleanup {
    pub kind: String,
    pub agent_id: Option<Uuid>,
    pub by_agent_id: Option<Uuid>,
    pub older_than: i32,
    pub rows_removed: i64,
    pub bytes_freed: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    pub agents: Vec<AgentStorage>,
    pub total_bytes: i64,
    pub cleanups: Vec<PastCleanup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupResult {
    pub kind: CleanupKind,
    pub older_than_days: i32,
    pub agent_id: Option<Uuid>,
    pub rows: i64,
    pub bytes: i64,
    pub applied: bool,
    pub what: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct CleanupRequest {
    pub owner: Uuid,
    pub org: Option<Uuid>,
    pub agent: Option<Uuid>,
    pub kind: CleanupKind,
    pub older_than_days: i32,
    pub by_agent: Option<Uuid>,
}

const MINE: &str = "SELECT id FROM agent_profiles WHERE owner_user_id = $1 AND org_id IS NOT DISTINCT FROM $2 AND ($3::uuid IS NULL OR id = $3)";

pub fn human_bytes(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{} B", bytes.max(0))
    }
}

impl WorldStore {
    pub async fn storage_report(&self, owner: Uuid, org: Option<Uuid>) -> Result<StorageReport> {
        let agents = sqlx::query_as::<_, AgentStorage>(
            "SELECT a.id AS agent_id, a.name, a.slug,
                    m.n AS messages, m.b AS message_bytes, r.n AS runs, r.b AS run_bytes,
                    s.n AS steps, s.b AS step_bytes, e.n AS events, e.b AS event_bytes,
                    (m.b + r.b + s.b + e.b) AS total_bytes,
                    GREATEST(m.last, r.last) AS last_active
               FROM agent_profiles a
               CROSS JOIN LATERAL (SELECT COUNT(*) AS n, COALESCE(SUM(pg_column_size(x.*)), 0)::BIGINT AS b, MAX(x.created_at) AS last
                                     FROM agent_messages x WHERE x.agent_id = a.id) m
               CROSS JOIN LATERAL (SELECT COUNT(*) AS n, COALESCE(SUM(pg_column_size(x.*)), 0)::BIGINT AS b, MAX(x.created_at) AS last
                                     FROM session_runs x WHERE x.agent_id = a.id) r
               CROSS JOIN LATERAL (SELECT COUNT(*) AS n, COALESCE(SUM(pg_column_size(x.*)), 0)::BIGINT AS b
                                     FROM session_run_events x JOIN session_runs y ON y.id = x.run_id WHERE y.agent_id = a.id) s
               CROSS JOIN LATERAL (SELECT COUNT(*) AS n, COALESCE(SUM(pg_column_size(x.*)), 0)::BIGINT AS b
                                     FROM world_events x WHERE x.agent_id = a.id) e
              WHERE a.owner_user_id = $1 AND a.org_id IS NOT DISTINCT FROM $2
              ORDER BY total_bytes DESC",
        )
        .bind(owner)
        .bind(org)
        .fetch_all(self.pool())
        .await?;
        let cleanups = sqlx::query_as::<_, PastCleanup>(
            "SELECT kind, agent_id, by_agent_id, older_than, rows_removed, bytes_freed, created_at FROM world_cleanups
              WHERE user_id = $1 AND org_id IS NOT DISTINCT FROM $2 ORDER BY created_at DESC LIMIT 20",
        )
        .bind(owner)
        .bind(org)
        .fetch_all(self.pool())
        .await?;
        let total_bytes = agents.iter().map(|a| a.total_bytes).sum();
        Ok(StorageReport { agents, total_bytes, cleanups })
    }

    pub async fn clean_up(&self, request: CleanupRequest, apply: bool) -> Result<CleanupResult> {
        if !(MIN_DAYS..=MAX_DAYS).contains(&request.older_than_days) {
            bail!("older_than_days must be between {MIN_DAYS} and {MAX_DAYS}");
        }
        if let Some(agent) = request.agent {
            let mine: bool = sqlx::query_scalar(&format!("SELECT EXISTS ({MINE})"))
                .bind(request.owner)
                .bind(request.org)
                .bind(agent)
                .fetch_one(self.pool())
                .await?;
            if !mine {
                bail!("that agent is not yours");
            }
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET LOCAL loka.cleanup = 'on'").execute(&mut *tx).await?;
        let (rows, bytes) = match request.kind {
            CleanupKind::RunLogs => run_logs(&mut tx, &request, apply).await?,
            CleanupKind::Chats => chats(&mut tx, &request, apply).await?,
            CleanupKind::Quests => quests(&mut tx, &request, apply).await?,
            CleanupKind::Activity => activity(&mut tx, &request, apply).await?,
        };
        if apply && rows > 0 {
            sqlx::query(
                "INSERT INTO world_cleanups (user_id, org_id, agent_id, by_agent_id, kind, older_than, rows_removed, bytes_freed)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(request.owner)
            .bind(request.org)
            .bind(request.agent)
            .bind(request.by_agent)
            .bind(request.kind.key())
            .bind(request.older_than_days)
            .bind(rows)
            .bind(bytes)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(CleanupResult {
            kind: request.kind,
            older_than_days: request.older_than_days,
            agent_id: request.agent,
            rows,
            bytes,
            applied: apply && rows > 0,
            what: request.kind.describe(),
        })
    }
}

async fn targets(tx: &mut Transaction<'_, Postgres>, table: &str, sql: &str, r: &CleanupRequest) -> Result<()> {
    sqlx::query(table).execute(&mut **tx).await?;
    sqlx::query(sql)
        .bind(r.owner)
        .bind(r.org)
        .bind(r.agent)
        .bind(r.older_than_days)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn measure(tx: &mut Transaction<'_, Postgres>, sql: &str) -> Result<(i64, i64)> {
    Ok(sqlx::query_as::<_, (i64, i64)>(sql).fetch_one(&mut **tx).await?)
}

async fn run_logs(tx: &mut Transaction<'_, Postgres>, r: &CleanupRequest, apply: bool) -> Result<(i64, i64)> {
    targets(
        tx,
        "CREATE TEMP TABLE cleanup_runs (id UUID PRIMARY KEY) ON COMMIT DROP",
        &format!(
            "INSERT INTO cleanup_runs SELECT x.id FROM session_runs x LEFT JOIN agent_tasks t ON t.id = x.task_id
              WHERE x.agent_id IN ({MINE}) AND x.status NOT IN ('queued', 'running')
                AND COALESCE(x.finished_at, x.created_at) < NOW() - make_interval(days => $4)
                AND (t.id IS NULL OR t.status IN ('done', 'failed', 'cancelled'))"
        ),
        r,
    )
    .await?;
    let (steps, step_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(e.*)), 0)::BIGINT FROM session_run_events e WHERE e.run_id IN (SELECT id FROM cleanup_runs)",
    )
    .await?;
    let (texts, text_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(octet_length(x.prompt) + COALESCE(octet_length(x.output), 0)), 0)::BIGINT
           FROM session_runs x WHERE x.id IN (SELECT id FROM cleanup_runs) AND (x.prompt <> '' OR x.output IS NOT NULL)",
    )
    .await?;
    if apply {
        sqlx::query("DELETE FROM session_run_events WHERE run_id IN (SELECT id FROM cleanup_runs)").execute(&mut **tx).await?;
        sqlx::query(
            "UPDATE session_runs SET prompt = '', output = NULL WHERE id IN (SELECT id FROM cleanup_runs) AND (prompt <> '' OR output IS NOT NULL)",
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok((steps + texts, step_bytes + text_bytes))
}

async fn chats(tx: &mut Transaction<'_, Postgres>, r: &CleanupRequest, apply: bool) -> Result<(i64, i64)> {
    targets(
        tx,
        "CREATE TEMP TABLE cleanup_messages (id UUID PRIMARY KEY) ON COMMIT DROP",
        &format!(
            "INSERT INTO cleanup_messages SELECT m.id FROM agent_messages m
              WHERE m.agent_id IN ({MINE}) AND m.created_at < NOW() - make_interval(days => $4)
                AND NOT (COALESCE(m.reply_count, 0) > 0 AND m.last_reply_at >= NOW() - make_interval(days => $4))"
        ),
        r,
    )
    .await?;
    let measured = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(m.*)), 0)::BIGINT FROM agent_messages m WHERE m.id IN (SELECT id FROM cleanup_messages)",
    )
    .await?;
    if apply {
        sqlx::query("DELETE FROM agent_messages WHERE id IN (SELECT id FROM cleanup_messages)").execute(&mut **tx).await?;
    }
    Ok(measured)
}

async fn quests(tx: &mut Transaction<'_, Postgres>, r: &CleanupRequest, apply: bool) -> Result<(i64, i64)> {
    targets(
        tx,
        "CREATE TEMP TABLE cleanup_tasks (id UUID PRIMARY KEY) ON COMMIT DROP",
        &format!(
            "INSERT INTO cleanup_tasks SELECT t.id FROM agent_tasks t
              WHERE (t.assigned_agent_id IN ({MINE}) OR ($3::uuid IS NULL AND t.assigned_agent_id IS NULL AND t.user_id = $1))
                AND t.org_id IS NOT DISTINCT FROM $2
                AND t.status IN ('done', 'failed', 'cancelled')
                AND COALESCE(t.completed_at, t.updated_at, t.created_at) < NOW() - make_interval(days => $4)
                AND NOT EXISTS (SELECT 1 FROM agent_tasks c WHERE c.parent_task_id = t.id
                                  AND (c.status NOT IN ('done', 'failed', 'cancelled')
                                       OR COALESCE(c.completed_at, c.updated_at, c.created_at) >= NOW() - make_interval(days => $4)))
                AND NOT EXISTS (SELECT 1 FROM session_runs x WHERE x.task_id = t.id AND x.status IN ('queued', 'running'))"
        ),
        r,
    )
    .await?;
    sqlx::query(
        "CREATE TEMP TABLE cleanup_sessions ON COMMIT DROP AS
         SELECT s.id FROM agent_sessions s WHERE s.task_id IN (SELECT id FROM cleanup_tasks) AND s.role <> 'main'",
    )
    .execute(&mut **tx)
    .await?;
    let (tasks, task_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(t.*)), 0)::BIGINT FROM agent_tasks t WHERE t.id IN (SELECT id FROM cleanup_tasks)",
    )
    .await?;
    let (sessions, session_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(s.*)), 0)::BIGINT FROM agent_sessions s WHERE s.id IN (SELECT id FROM cleanup_sessions)",
    )
    .await?;
    let (runs, run_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(x.*)), 0)::BIGINT FROM session_runs x WHERE x.session_id IN (SELECT id FROM cleanup_sessions)",
    )
    .await?;
    let (steps, step_bytes) = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(e.*)), 0)::BIGINT FROM session_run_events e
          JOIN session_runs x ON x.id = e.run_id WHERE x.session_id IN (SELECT id FROM cleanup_sessions)",
    )
    .await?;
    if apply {
        sqlx::query("DELETE FROM agent_sessions WHERE id IN (SELECT id FROM cleanup_sessions)").execute(&mut **tx).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id IN (SELECT id FROM cleanup_tasks)").execute(&mut **tx).await?;
    }
    Ok((tasks + sessions + runs + steps, task_bytes + session_bytes + run_bytes + step_bytes))
}

async fn activity(tx: &mut Transaction<'_, Postgres>, r: &CleanupRequest, apply: bool) -> Result<(i64, i64)> {
    targets(
        tx,
        "CREATE TEMP TABLE cleanup_events (seq BIGINT PRIMARY KEY) ON COMMIT DROP",
        &format!(
            "INSERT INTO cleanup_events SELECT e.seq FROM world_events e
              WHERE e.org_id IS NOT DISTINCT FROM $2
                AND e.created_at < NOW() - make_interval(days => $4)
                AND (e.agent_id IN ({MINE}) OR ($3::uuid IS NULL AND e.agent_id IS NULL AND e.actor_user_id = $1))
                AND NOT EXISTS (SELECT 1 FROM world_notifications n WHERE n.event_seq = e.seq AND n.cleared_at IS NULL)"
        ),
        r,
    )
    .await?;
    let measured = measure(
        tx,
        "SELECT COUNT(*), COALESCE(SUM(pg_column_size(e.*)), 0)::BIGINT FROM world_events e WHERE e.seq IN (SELECT seq FROM cleanup_events)",
    )
    .await?;
    if apply {
        sqlx::query("DELETE FROM world_events WHERE seq IN (SELECT seq FROM cleanup_events)").execute(&mut **tx).await?;
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleanups_preview_then_remove_only_old_data_and_keep_totals() -> Result<()> {
        let Ok(url) = std::env::var("CLEANUP_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("clean-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("other-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let agent = |slug: String, user: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Keeper', 'claude', 'haiku', $2) RETURNING id",
                )
                .bind(slug)
                .bind(user)
                .fetch_one(&pool)
                .await
            }
        };
        let mine = agent(format!("keep-{tag}"), owner).await?;
        let theirs = agent(format!("their-{tag}"), other).await?;
        let org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(mine).fetch_one(&pool).await?;

        for (a, age) in [(mine, 100), (mine, 1), (theirs, 100)] {
            sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, body, created_at) VALUES ($1, 'agent', 'hello', NOW() - make_interval(days => $2))")
                .bind(a)
                .bind(age)
                .execute(&pool)
                .await?;
            sqlx::query("INSERT INTO world_events (type, agent_id, payload, created_at) VALUES ('agent.progress', $1, '{\"quiet\":true}', NOW() - make_interval(days => $2))")
                .bind(a)
                .bind(age)
                .execute(&pool)
                .await?;
        }
        let quest: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, assigned_agent_id, status) VALUES ($1, 'Old quest', $2, 'done') RETURNING id",
        )
        .bind(owner)
        .bind(mine)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_tasks SET completed_at = NOW() - INTERVAL '100 days', updated_at = NOW() - INTERVAL '100 days' WHERE id = $1")
            .bind(quest)
            .execute(&pool)
            .await?;
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'haiku') RETURNING id")
            .bind(mine)
            .fetch_one(&pool)
            .await?;
        let work: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'claude', 'haiku', $2) RETURNING id")
            .bind(mine)
            .bind(quest)
            .fetch_one(&pool)
            .await?;
        let mut runs = Vec::new();
        for (session, task) in [(main, None), (work, Some(quest))] {
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, cost_usd, created_at, finished_at)
                 VALUES ($1, $2, $3, 'turn', 'completed', 'long prompt', 'long output', 2.5, NOW() - INTERVAL '100 days', NOW() - INTERVAL '100 days')
                 RETURNING id",
            )
            .bind(session)
            .bind(mine)
            .bind(task)
            .fetch_one(&pool)
            .await?;
            sqlx::query("INSERT INTO session_run_events (run_id, kind, body) VALUES ($1, 'text', 'step one'), ($1, 'tool', 'step two')")
                .bind(run)
                .execute(&pool)
                .await?;
            runs.push(run);
        }
        let total = || async {
            sqlx::query_scalar::<_, f64>("SELECT cost_usd::float8 FROM agent_usage_totals WHERE agent_id = $1").bind(mine).fetch_one(&pool).await
        };
        let before = total().await?;
        let request = |kind| CleanupRequest { owner, org, agent: None, kind, older_than_days: 30, by_agent: None };

        assert!(store.clean_up(CleanupRequest { older_than_days: 3, ..request(CleanupKind::Chats) }, false).await.is_err());
        assert!(store.clean_up(CleanupRequest { agent: Some(theirs), ..request(CleanupKind::Chats) }, true).await.is_err());

        let preview = store.clean_up(request(CleanupKind::Chats), false).await?;
        assert_eq!((preview.rows, preview.applied), (1, false));
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1").bind(mine).fetch_one(&pool).await?;
        assert_eq!(left, 2, "a preview deletes nothing");
        assert!(store.clean_up(request(CleanupKind::Chats), true).await?.applied);
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1), (SELECT COUNT(*) FROM agent_messages WHERE agent_id = $2)",
        )
        .bind(mine)
        .bind(theirs)
        .fetch_one(&pool)
        .await?;
        assert_eq!(counts, (1, 1), "only my old message goes");

        let logs = store.clean_up(request(CleanupKind::RunLogs), true).await?;
        assert_eq!(logs.rows, 6);
        let (steps, prompt): (i64, String) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM session_run_events WHERE run_id = $1), (SELECT prompt FROM session_runs WHERE id = $1)",
        )
        .bind(runs[0])
        .fetch_one(&pool)
        .await?;
        assert_eq!((steps, prompt.as_str()), (0, ""));

        assert!(store.clean_up(request(CleanupKind::Quests), true).await?.applied);
        let gone: (bool, bool, bool, bool) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM agent_tasks WHERE id = $1), EXISTS (SELECT 1 FROM agent_sessions WHERE id = $2),
                    EXISTS (SELECT 1 FROM session_runs WHERE id = $3), EXISTS (SELECT 1 FROM agent_sessions WHERE id = $4)",
        )
        .bind(quest)
        .bind(work)
        .bind(runs[1])
        .bind(main)
        .fetch_one(&pool)
        .await?;
        assert_eq!(gone, (false, false, false, true), "the quest goes with its session and runs, the main session stays");
        assert_eq!(total().await?, before, "lifetime cost is unchanged");

        assert_eq!(store.clean_up(request(CleanupKind::Activity), true).await?.rows, 1);
        let report = store.storage_report(owner, org).await?;
        assert_eq!(report.agents.len(), 1);
        assert_eq!(report.agents[0].messages, 1);
        assert_eq!(report.cleanups.len(), 4);

        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![mine, theirs]).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, other]).execute(&pool).await?;
        Ok(())
    }
}
