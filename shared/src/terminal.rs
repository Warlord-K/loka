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
pub const RUNNER_FRAME_MAX: usize = 640 * 1024;
pub const VIEW_FRAME_MAX: usize = 512 * 1024;
pub const SHELL: &str = "shell";
pub const BROWSER: &str = "browser";
pub const OUT_FRAME_MAX: usize = 96 * 1024;
pub const IN_FRAME_MAX: usize = 32 * 1024;
pub const PENDING_STALE_SECS: i64 = 600;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct TerminalSession {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub runner_id: Uuid,
    pub kind: String,
    pub agent_id: Option<Uuid>,
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
    #[serde(default = "shell_kind")]
    pub kind: String,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watched {
    pub agent: Uuid,
    pub quest: Option<Uuid>,
}

fn shell_kind() -> String {
    SHELL.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    Frame { seq: u64, data: String, width: u32, height: u32 },
    Page { url: String },
    Idle { reason: String },
    Mouse {
        event: String,
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clicks: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dx: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dy: Option<f64>,
        #[serde(default)]
        modifiers: u8,
    },
    Key {
        event: String,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_code: Option<u32>,
        #[serde(default)]
        modifiers: u8,
    },
    Paste { text: String },
    Open { url: String },
}

impl TermFrame {
    pub fn viewer_input_fits(&self) -> bool {
        match self {
            TermFrame::In { data } => data.len() <= IN_FRAME_MAX,
            TermFrame::Resize { cols, rows } => (1..=500).contains(cols) && (1..=500).contains(rows),
            TermFrame::Mouse { event, x, y, button, dx, dy, .. } => {
                event.len() <= 32
                    && [Some(*x), Some(*y), *dx, *dy].into_iter().flatten().all(|v| v.is_finite() && v.abs() <= 100_000.0)
                    && button.as_ref().is_none_or(|b| b.len() <= 16)
            }
            TermFrame::Key { event, key, code, text, .. } => {
                event.len() <= 16 && key.len() <= 64 && code.as_ref().is_none_or(|c| c.len() <= 64) && text.as_ref().is_none_or(|t| t.len() <= 16)
            }
            TermFrame::Paste { text } => text.len() <= IN_FRAME_MAX,
            TermFrame::Open { url } => url.len() <= 2048,
            _ => false,
        }
    }

    pub fn runner_output_fits(&self) -> bool {
        match self {
            TermFrame::Hello { .. } | TermFrame::Error { .. } => true,
            TermFrame::Out { data } => data.len() <= OUT_FRAME_MAX,
            TermFrame::Frame { data, width, height, .. } => data.len() <= VIEW_FRAME_MAX && *width <= 8192 && *height <= 8192,
            TermFrame::Page { url } => url.len() <= 4096,
            TermFrame::Idle { reason } => reason.len() <= 64,
            _ => false,
        }
    }
}

pub fn idle_secs() -> u64 {
    std::env::var("WORLD_TERMINAL_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_SECS)
        .clamp(3, 24 * 3600)
}

pub fn max_views_per_device() -> i64 {
    std::env::var("WORLD_BROWSER_VIEWS_PER_DEVICE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_PER_DEVICE * 2)
        .clamp(1, 40)
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
    pub async fn create_terminal(&self, owner: Uuid, runner: Uuid, watched: Option<Watched>, cols: i32, rows: i32, cap: i64) -> Result<NewTerminal> {
        let kind = if watched.is_some() { BROWSER } else { SHELL };
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text))")
            .bind(runner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE terminal_sessions SET status = 'closed', close_reason = 'never_connected', closed_at = NOW() WHERE status = 'pending' AND closed_at IS NULL AND created_at < NOW() - make_interval(secs => $1)")
            .bind(PENDING_STALE_SECS)
            .execute(&mut *tx)
            .await?;
        let live = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM terminal_sessions WHERE runner_id = $1 AND kind = $2 AND closed_at IS NULL")
            .bind(runner)
            .bind(kind)
            .fetch_one(&mut *tx)
            .await?;
        if live >= cap {
            tx.rollback().await?;
            return Ok(NewTerminal::Busy { live });
        }
        let session = sqlx::query_as::<_, TerminalSession>(
            "INSERT INTO terminal_sessions (owner_user_id, runner_id, cols, rows, kind, agent_id, task_id) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(owner)
        .bind(runner)
        .bind(cols)
        .bind(rows)
        .bind(kind)
        .bind(watched.map(|w| w.agent))
        .bind(watched.and_then(|w| w.quest))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(NewTerminal::Created(session))
    }

    pub async fn agent_works_on(&self, agent: Uuid, quest: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_tasks WHERE id = $1 AND assigned_agent_id = $2)")
            .bind(quest)
            .bind(agent)
            .fetch_one(self.pool())
            .await?)
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

pub const SERVES_KIND: &str = "(t.kind = 'shell' OR (w.capabilities->>'browser_view') = 'true')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRoom {
    pub shells: bool,
    pub views: bool,
}

impl TerminalRoom {
    pub const SHELLS: TerminalRoom = TerminalRoom { shells: true, views: false };

    pub fn any(self) -> bool {
        self.shells || self.views
    }
}

pub fn runner_shows_browsers(capabilities: &serde_json::Value) -> bool {
    capabilities.get("browser_view").and_then(serde_json::Value::as_bool) == Some(true)
}

impl RunnerStore {
    pub async fn claim_terminal(&self, runner_id: Uuid, room: TerminalRoom) -> Result<Option<TerminalJob>> {
        let claimed = sqlx::query_as::<_, (Uuid, i32, i32, String, Option<Uuid>, Option<Uuid>)>(&format!(
            "WITH next AS (
               SELECT t.id FROM terminal_sessions t
               JOIN world_runners w ON w.id = t.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE t.status = 'pending' AND t.closed_at IS NULL AND t.runner_id = $1
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin') AND {SERVES_KIND}
                 AND CASE WHEN t.kind = 'browser' THEN $2 ELSE $3 END
               ORDER BY t.created_at LIMIT 1 FOR UPDATE OF t SKIP LOCKED
             )
             UPDATE terminal_sessions t SET status = 'open', last_activity_at = NOW()
             FROM next WHERE t.id = next.id
             RETURNING t.id, t.cols, t.rows, t.kind, t.agent_id, t.task_id",
        ))
        .bind(runner_id)
        .bind(room.views)
        .bind(room.shells)
        .fetch_optional(self.pool())
        .await?;
        Ok(claimed.map(|(id, cols, rows, kind, agent_id, task_id)| TerminalJob { id, cols, rows, kind, agent_id, task_id }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn connect() -> Result<Option<(PgPool, WorldStore, RunnerStore, String)>> {
        let Ok(url) = std::env::var("TERMINAL_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        Ok(Some((pool.clone(), WorldStore::new(pool.clone()), RunnerStore::new(pool), Uuid::new_v4().simple().to_string()[..8].to_string())))
    }

    #[tokio::test]
    async fn browser_views_reach_only_runners_that_can_show_them() -> Result<()> {
        let Some((pool, world, runners, tag)) = connect().await? else { return Ok(()) };
        let ann: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, $1) RETURNING id").bind(format!("ann-{tag}@view.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, ann, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let mac: Uuid = sqlx::query_scalar("INSERT INTO world_runners (owner_user_id, name, platform, token_hash, status, last_seen_at) VALUES ($1, 'mac', 'macos', $2, 'online', NOW()) RETURNING id")
            .bind(ann)
            .bind([*Uuid::new_v4().as_bytes(), *Uuid::new_v4().as_bytes()].concat())
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, $1, 'claude', 'sonnet', $2, $3) RETURNING id")
            .bind(format!("viewer-{tag}"))
            .bind(ann)
            .bind(org)
            .fetch_one(&pool)
            .await?;

        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, repo, assigned_agent_id) VALUES ($1, 'Check the dashboard', 'acme/api', $2) RETURNING id")
            .bind(ann)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let NewTerminal::Created(view) = world.create_terminal(ann, mac, Some(Watched { agent, quest: Some(quest) }), 80, 24, 1).await? else { panic!("view refused") };
        assert_eq!((view.kind.as_str(), view.agent_id), (BROWSER, Some(agent)));
        assert!(world.agent_works_on(agent, quest).await? && !world.agent_works_on(agent, Uuid::new_v4()).await?);
        assert!(matches!(world.create_terminal(ann, mac, Some(Watched { agent, quest: None }), 80, 24, 1).await?, NewTerminal::Busy { live: 1 }), "views count against their own cap");
        let NewTerminal::Created(shell) = world.create_terminal(ann, mac, None, 80, 24, 1).await? else { panic!("a shell is not held back by views") };
        assert_eq!((shell.kind.as_str(), shell.agent_id), (SHELL, None));

        let both = TerminalRoom { shells: true, views: true };
        let views_only = TerminalRoom { shells: false, views: true };
        assert!(runners.claim_terminal(mac, views_only).await?.is_none(), "a runner with room only for views never gets a shell");
        let first = runners.claim_terminal(mac, both).await?.expect("the shell is claimed");
        assert_eq!((first.id, first.kind.as_str()), (shell.id, SHELL));
        assert!(runners.claim_terminal(mac, both).await?.is_none(), "an older runner never gets a browser view, it would open a shell");
        assert!(!runners.has_work(mac).await?, "has_work skips what the claim skips");

        sqlx::query("UPDATE world_runners SET capabilities = '{\"browser_view\": true}' WHERE id = $1").bind(mac).execute(&pool).await?;
        assert!(runners.has_work(mac).await?);
        assert!(runners.claim_terminal(mac, TerminalRoom::SHELLS).await?.is_none(), "a runner whose view slots are full never gets another view");
        let job = runners.claim_terminal(mac, views_only).await?.expect("the view is claimed");
        assert_eq!((job.id, job.kind.as_str(), job.agent_id, job.task_id), (view.id, BROWSER, Some(agent), Some(quest)), "a view opened from a quest watches that quest");

        let mismatched = sqlx::query("INSERT INTO terminal_sessions (owner_user_id, runner_id, kind) VALUES ($1, $2, 'browser')").bind(ann).bind(mac).execute(&pool).await;
        assert!(mismatched.is_err(), "a browser view always names its agent");
        let shell_with_quest = sqlx::query("INSERT INTO terminal_sessions (owner_user_id, runner_id, kind, task_id) VALUES ($1, $2, 'shell', $3)").bind(ann).bind(mac).bind(quest).execute(&pool).await;
        assert!(shell_with_quest.is_err(), "only browser views watch a quest");
        Ok(())
    }

    #[test]
    fn old_servers_send_shell_jobs_and_frames_stay_in_their_lane() {
        let old: TerminalJob = serde_json::from_str(r#"{"id":"6f1c2c1e-6c55-4d53-9a3f-2f4b3e3b8a11","cols":80,"rows":24}"#).unwrap();
        assert_eq!((old.kind.as_str(), old.agent_id, old.task_id), (SHELL, None, None));
        let quest = Uuid::new_v4();
        let view = TerminalJob { id: Uuid::new_v4(), cols: 80, rows: 24, kind: BROWSER.into(), agent_id: Some(Uuid::new_v4()), task_id: Some(quest) };
        let read: TerminalJob = serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
        assert_eq!(read.task_id, Some(quest), "a view opened from a quest reaches the runner with its quest");
        assert!(TermFrame::Frame { seq: 1, data: "A".repeat(VIEW_FRAME_MAX), width: 1280, height: 720 }.runner_output_fits());
        assert!(!TermFrame::Frame { seq: 1, data: "A".repeat(VIEW_FRAME_MAX + 1), width: 1280, height: 720 }.runner_output_fits());
        assert!(!TermFrame::Mouse { event: "mousePressed".into(), x: 1.0, y: 1.0, button: None, clicks: None, dx: None, dy: None, modifiers: 0 }.runner_output_fits());
        assert!(TermFrame::Mouse { event: "mousePressed".into(), x: 1.0, y: 1.0, button: Some("left".into()), clicks: Some(1), dx: None, dy: None, modifiers: 0 }.viewer_input_fits());
        assert!(!TermFrame::Mouse { event: "mouseMoved".into(), x: f64::NAN, y: 1.0, button: None, clicks: None, dx: None, dy: None, modifiers: 0 }.viewer_input_fits());
        assert!(!TermFrame::Frame { seq: 1, data: String::new(), width: 1, height: 1 }.viewer_input_fits(), "a viewer cannot send frames to the device");
        assert!(!TermFrame::Open { url: "x".repeat(3000) }.viewer_input_fits());
        assert!(TermFrame::Paste { text: "hunter2".into() }.viewer_input_fits());
    }
}
