use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::runners::RunnerStore;
use crate::world::WorldStore;

pub const DEFAULT_IDLE_SECS: u64 = 900;
pub const DEFAULT_MAX_PER_DEVICE: i64 = 5;
pub const BROWSER_FRAME_MAX: usize = 1024 * 1024;
pub const RUNNER_FRAME_MAX: usize = 256 * 1024;
pub const OUT_FRAME_MAX: usize = 96 * 1024;
pub const IN_FRAME_MAX: usize = 32 * 1024;
pub const PENDING_STALE_SECS: i64 = 600;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct TerminalSession {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub runner_id: Uuid,
    pub cols: i32,
    pub rows: i32,
    pub status: String,
    pub close_reason: Option<String>,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalJob {
    pub id: Uuid,
    pub cols: i32,
    pub rows: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TermFrame {
    In { data: String },
    Resize { cols: u16, rows: u16 },
    Close { reason: Option<String> },
    Hello { cols: u16, rows: u16, cwd: String },
    Out { data: String },
    Error { message: String },
    Bye { reason: String },
    Status { state: String, device: Option<String> },
    Closed { reason: String },
    Ping,
}

pub fn idle_secs() -> u64 {
    std::env::var("WORLD_TERMINAL_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_SECS)
        .clamp(3, 24 * 3600)
}

pub fn max_per_device() -> i64 {
    std::env::var("WORLD_TERMINAL_MAX_PER_DEVICE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_PER_DEVICE)
        .clamp(1, 20)
}

pub enum NewTerminal {
    Created(TerminalSession),
    Busy { live: i64 },
}

impl WorldStore {
    pub async fn create_terminal(&self, owner: Uuid, runner: Uuid, cols: i32, rows: i32, cap: i64) -> Result<NewTerminal> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text))")
            .bind(runner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE terminal_sessions SET status = 'closed', close_reason = 'never_connected', closed_at = NOW() WHERE status = 'pending' AND closed_at IS NULL AND created_at < NOW() - make_interval(secs => $1)")
            .bind(PENDING_STALE_SECS)
            .execute(&mut *tx)
            .await?;
        let live = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM terminal_sessions WHERE runner_id = $1 AND closed_at IS NULL")
            .bind(runner)
            .fetch_one(&mut *tx)
            .await?;
        if live >= cap {
            tx.rollback().await?;
            return Ok(NewTerminal::Busy { live });
        }
        let session = sqlx::query_as::<_, TerminalSession>(
            "INSERT INTO terminal_sessions (owner_user_id, runner_id, cols, rows) VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(owner)
        .bind(runner)
        .bind(cols)
        .bind(rows)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(NewTerminal::Created(session))
    }

    pub async fn terminal(&self, id: Uuid) -> Result<Option<TerminalSession>> {
        Ok(sqlx::query_as::<_, TerminalSession>("SELECT * FROM terminal_sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn touch_terminal(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE terminal_sessions SET last_activity_at = NOW() WHERE id = $1 AND closed_at IS NULL")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn stale_open_terminals(&self, stale_secs: i64) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM terminal_sessions WHERE status = 'open' AND closed_at IS NULL AND last_activity_at < NOW() - make_interval(secs => $1)",
        )
        .bind(stale_secs)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn close_terminal(&self, id: Uuid, reason: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "WITH done AS (
               UPDATE terminal_sessions SET status = 'closed', close_reason = $2, closed_at = NOW()
               WHERE id = $1 AND closed_at IS NULL RETURNING id
             ) SELECT COUNT(*) FROM done",
        )
        .bind(id)
        .bind(reason)
        .fetch_one(self.pool())
        .await?
            > 0)
    }
}

impl RunnerStore {
    pub async fn claim_terminal(&self, runner_id: Uuid) -> Result<Option<(Uuid, i32, i32)>> {
        Ok(sqlx::query_as::<_, (Uuid, i32, i32)>(
            "WITH next AS (
               SELECT t.id FROM terminal_sessions t
               JOIN world_runners w ON w.id = t.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE t.status = 'pending' AND t.closed_at IS NULL AND t.runner_id = $1
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin')
               ORDER BY t.created_at LIMIT 1 FOR UPDATE OF t SKIP LOCKED
             )
             UPDATE terminal_sessions t SET status = 'open', last_activity_at = NOW()
             FROM next WHERE t.id = next.id
             RETURNING t.id, t.cols, t.rows",
        )
        .bind(runner_id)
        .fetch_optional(self.pool())
        .await?)
    }
}

pub fn login_shell() -> String {
    let from_env = std::env::var("SHELL").ok();
    let candidates = if cfg!(target_os = "macos") { [account_shell(), from_env] } else { [from_env, account_shell()] };
    candidates
        .into_iter()
        .flatten()
        .find(|shell| usable_shell(shell))
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".to_string() } else { "/bin/bash".to_string() })
}

fn usable_shell(shell: &str) -> bool {
    let path = std::path::Path::new(shell);
    path.is_absolute()
        && path.is_file()
        && !matches!(path.file_name().and_then(|name| name.to_str()), Some("nologin" | "false" | "true" | "sh"))
}

#[cfg(unix)]
fn account_shell() -> Option<String> {
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let code = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
    if code != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) }.to_str().ok().map(str::to_string)
}

#[cfg(not(unix))]
fn account_shell() -> Option<String> {
    None
}
