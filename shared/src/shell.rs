use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashSet;
use uuid::Uuid;

use crate::runners::RunnerStore;
use crate::world::{WorldEvent, WorldStore};

pub const OUTPUT_CAP: usize = 250_000;
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const MAX_TIMEOUT_SECS: u64 = 600;
pub const STALE_SECS: f64 = 90.0;
pub const TRUNCATED_MARKER: &str = "[earlier output truncated, kept the tail]\n";

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ShellRun {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub owner_user_id: Uuid,
    pub runner_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub command: String,
    pub status: String,
    pub output: String,
    pub truncated: bool,
    pub total_bytes: i64,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub cancel_requested: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellJob {
    pub id: Uuid,
    pub command: String,
    pub workspace_key: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellOutput {
    pub chunk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellFinish {
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub output: String,
    pub truncated: bool,
    pub total_bytes: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ShellEnded {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub owner_user_id: Uuid,
    pub message_id: Option<Uuid>,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub truncated: bool,
    pub output: String,
    pub error: Option<String>,
}

pub fn shell_timeout() -> u64 {
    std::env::var("WORLD_SHELL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS)
}

impl WorldStore {
    pub async fn start_shell_run(
        &self,
        agent_id: Uuid,
        owner: Uuid,
        runner_id: Option<Uuid>,
        command: &str,
    ) -> Result<ShellRun> {
        Ok(sqlx::query_as::<_, ShellRun>(
            "INSERT INTO shell_runs (agent_id, owner_user_id, runner_id, command)
             VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(agent_id)
        .bind(owner)
        .bind(runner_id)
        .bind(command)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn link_shell_message(&self, run_id: Uuid, message_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE shell_runs SET message_id = $2 WHERE id = $1")
            .bind(run_id)
            .bind(message_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn owned_agent_ids(&self, owner: Uuid) -> Result<HashSet<Uuid>> {
        Ok(
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE owner_user_id = $1")
                .bind(owner)
                .fetch_all(self.pool())
                .await?
                .into_iter()
                .collect(),
        )
    }

    pub async fn shell_run(&self, id: Uuid) -> Result<Option<ShellRun>> {
        Ok(
            sqlx::query_as::<_, ShellRun>("SELECT * FROM shell_runs WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    pub async fn cancel_shell_run(&self, id: Uuid) -> Result<Option<ShellRun>> {
        Ok(sqlx::query_as::<_, ShellRun>(
            "UPDATE shell_runs SET
               status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END,
               cancel_requested = TRUE,
               finished_at = CASE WHEN status = 'queued' THEN NOW() ELSE finished_at END
             WHERE id = $1 AND status IN ('queued', 'running')
             RETURNING *",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn take_shell_auto_share(&self, message_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "UPDATE agent_messages SET meta = (meta - 'auto_share') || '{\"shared\": true}'::jsonb
             WHERE id = $1 AND meta->>'auto_share' = 'true' RETURNING id",
        )
        .bind(message_id)
        .fetch_optional(self.pool())
        .await?
        .is_some())
    }

    pub async fn mark_shell_shared(&self, run_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE agent_messages SET meta = meta || '{\"shared\": true}'::jsonb WHERE meta->>'shell_run_id' = $1::text AND meta->>'kind' = 'shell'")
            .bind(run_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn finish_shell_message(&self, message_id: Uuid, body: &str, meta: &serde_json::Value) -> Result<()> {
        sqlx::query("UPDATE agent_messages SET body = $2, meta = meta || $3::jsonb WHERE id = $1")
            .bind(message_id)
            .bind(body)
            .bind(meta)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

impl RunnerStore {
    pub async fn claim_shell(&self, runner_id: Uuid) -> Result<Option<(Uuid, Uuid, String)>> {
        Ok(sqlx::query_as::<_, (Uuid, Uuid, String)>(
            "WITH next AS (
               SELECT s.id FROM shell_runs s
               JOIN agent_profiles a ON a.id = s.agent_id
               JOIN world_runners w ON w.id = s.runner_id
               JOIN users u ON u.id = w.owner_user_id
               WHERE s.status = 'queued' AND s.runner_id = $1
                 AND a.owner_user_id = w.owner_user_id AND s.owner_user_id = w.owner_user_id
                 AND w.status IN ('online', 'offline') AND u.world_role IN ('write', 'admin')
               ORDER BY s.created_at LIMIT 1 FOR UPDATE OF s SKIP LOCKED
             )
             UPDATE shell_runs s SET status = 'running', started_at = NOW(), heartbeat_at = NOW()
             FROM next WHERE s.id = next.id
             RETURNING s.id, s.agent_id, s.command",
        )
        .bind(runner_id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn append_shell_output(&self, runner_id: Uuid, run_id: Uuid, chunk: &str) -> Result<Option<bool>> {
        Ok(sqlx::query_scalar::<_, bool>(
            "UPDATE shell_runs SET
               output = CASE WHEN length(output) >= $4 THEN output ELSE left(output || $3, $4::int) END,
               heartbeat_at = NOW()
             WHERE id = $1 AND runner_id = $2 AND status = 'running'
             RETURNING cancel_requested",
        )
        .bind(run_id)
        .bind(runner_id)
        .bind(chunk)
        .bind(OUTPUT_CAP as i64)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn finish_shell(&self, runner_id: Uuid, run_id: Uuid, finish: &ShellFinish) -> Result<Option<ShellEnded>> {
        let output = if finish.truncated {
            format!("{TRUNCATED_MARKER}{}", finish.output)
        } else {
            finish.output.clone()
        };
        Ok(sqlx::query_as::<_, ShellEnded>(
            "UPDATE shell_runs SET
               status = $3, exit_code = $4, duration_ms = $5, output = $6,
               truncated = $7, total_bytes = $8, error = $9, finished_at = NOW(), heartbeat_at = NOW()
             WHERE id = $1 AND runner_id = $2 AND status = 'running'
             RETURNING id, agent_id, owner_user_id, message_id, command, status, exit_code, duration_ms, truncated, output, error",
        )
        .bind(run_id)
        .bind(runner_id)
        .bind(&finish.status)
        .bind(finish.exit_code)
        .bind(finish.duration_ms)
        .bind(output)
        .bind(finish.truncated)
        .bind(finish.total_bytes)
        .bind(&finish.error)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn expire_stale_shell_runs(&self) -> Result<Vec<ShellEnded>> {
        Ok(sqlx::query_as::<_, ShellEnded>(
            "UPDATE shell_runs SET status = 'failed', error = 'the runner went away', finished_at = NOW()
             WHERE status = 'running' AND (heartbeat_at IS NULL OR heartbeat_at < NOW() - make_interval(secs => $1))
             RETURNING id, agent_id, owner_user_id, message_id, command, status, exit_code, duration_ms, truncated, output, error",
        )
        .bind(STALE_SECS)
        .fetch_all(self.pool())
        .await?)
    }

}

pub fn hidden_from(event: &WorldEvent, owned: &HashSet<Uuid>) -> bool {
    let shared_output = event.event_type == "message.created" && event.payload.get("shell_share").and_then(|v| v.as_bool()) == Some(true);
    if !event.event_type.starts_with("shell.") && !shared_output {
        return false;
    }
    event.agent_id.is_none_or(|agent| !owned.contains(&agent))
}

pub fn shell_message_body(command: &str, ended: &ShellEnded) -> String {
    let tail = ended.output.trim_end_matches('\n');
    let tail = if tail.chars().count() > 4000 {
        let keep: String = tail.chars().skip(tail.chars().count() - 4000).collect();
        format!("...\n{keep}")
    } else {
        tail.to_string()
    };
    let outcome = match ended.status.as_str() {
        "cancelled" => "cancelled".to_string(),
        _ => match ended.exit_code {
            Some(code) => {
                let secs = ended.duration_ms.unwrap_or(0) as f64 / 1000.0;
                format!("exit {code} · {secs:.1}s")
            }
            None => ended
                .error
                .clone()
                .unwrap_or_else(|| "failed".to_string()),
        },
    };
    if tail.is_empty() {
        format!("$ {command}\n\n({outcome})")
    } else {
        format!("$ {command}\n\n{tail}\n\n({outcome})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, agent: Uuid, payload: serde_json::Value) -> WorldEvent {
        WorldEvent {
            seq: 1,
            event_type: kind.into(),
            agent_id: Some(agent),
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: None,
            org_id: None,
            payload,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn shared_shell_output_stays_with_the_owner() {
        let agent = Uuid::new_v4();
        let mine: HashSet<Uuid> = [agent].into_iter().collect();
        let theirs = HashSet::new();
        let shared = event("message.created", agent, serde_json::json!({ "shell_share": true, "body": "secret" }));
        let chat = event("message.created", agent, serde_json::json!({ "body": "hi" }));
        assert!(hidden_from(&shared, &theirs));
        assert!(!hidden_from(&shared, &mine));
        assert!(!hidden_from(&chat, &theirs));
        assert!(hidden_from(&event("shell.started", agent, serde_json::json!({})), &theirs));
    }
}
