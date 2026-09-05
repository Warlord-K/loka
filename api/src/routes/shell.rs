use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::shell::{shell_message_body, ShellEnded, ShellRun};
use loka_shared::world::{NewEvent, NewMessage, WorldAgent};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

type ApiError = (StatusCode, Json<Value>);

fn problem(status: StatusCode, code: &str, message: impl Into<String>) -> ApiError {
    (
        status,
        Json(json!({ "error": code, "message": message.into() })),
    )
}

fn internal(error: anyhow::Error) -> ApiError {
    tracing::error!(error = %error, "shell route failed");
    problem(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Shell operation failed",
    )
}

async fn owned_run(state: &AppState, user: Uuid, id: Uuid) -> Result<ShellRun, ApiError> {
    state
        .world
        .shell_run(id)
        .await
        .map_err(internal)?
        .filter(|run| run.owner_user_id == user)
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "not_found", "Shell run not found"))
}

pub async fn start_shell(
    state: &AppState,
    user: Uuid,
    agent: &WorldAgent,
    command: &str,
    share_output: bool,
    run_on: Option<Uuid>,
) -> Result<loka_shared::world::AgentMessage, ApiError> {
    if agent.owner_user_id != Some(user) {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "not_owner",
            "Only the owner of this Loka can run shell commands",
        ));
    }
    let runner = match run_on {
        Some(id) => state.runners.list(user).await.map_err(internal)?.into_iter().find(|r| r.id == id && r.owner_user_id == user && r.status != "revoked"),
        None => state.runners.agent_runner(user, agent.id).await.map_err(internal)?,
    };
    let Some(runner) = runner else {
        return Err(problem(
            StatusCode::CONFLICT,
            "no_device",
            "This agent has no execution device, so shell commands cannot run",
        ));
    };
    if runner.status != "online" {
        return Err(problem(
            StatusCode::CONFLICT,
            "device_offline",
            format!("{} is offline. Start it, then try again.", runner.name),
        ));
    }
    let command = command.trim();
    let user_msg = state
        .world
        .insert_message(NewMessage {
            agent_id: agent.id,
            author_kind: "user",
            author_user_id: Some(user),
            author_agent_id: None,
            body: &format!("!{command}"),
            session_id: None,
            run_id: None,
            meta: if share_output { json!({ "shell_command": true, "hidden": true }) } else { json!({ "shell_command": true }) },
        })
        .await
        .map_err(internal)?;
    let run = state
        .world
        .start_shell_run(agent.id, user, Some(runner.id), command)
        .await
        .map_err(internal)?;
    let card = state
        .world
        .insert_message(NewMessage {
            agent_id: agent.id,
            author_kind: "system",
            author_user_id: None,
            author_agent_id: Some(agent.id),
            body: &format!("$ {command}\n\nRunning on {}...", runner.name),
            session_id: None,
            run_id: None,
            meta: json!({
                "kind": "shell",
                "shell_run_id": run.id,
                "status": "queued",
                "command": command,
                "device": runner.name,
                "auto_share": share_output,
            }),
        })
        .await
        .map_err(internal)?;
    state
        .world
        .link_shell_message(run.id, card.id)
        .await
        .map_err(internal)?;
    state
        .world
        .emit(
            NewEvent::new("shell.started")
                .agent(agent.id)
                .actor(Some(user))
                .payload(json!({
                    "run_id": run.id,
                    "message_id": card.id,
                    "user_message_id": user_msg.id,
                    "command": command,
                    "device": runner.name,
                    "quiet": true,
                })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok(user_msg)
}

pub async fn announce_shell_end(state: &AppState, ended: &ShellEnded) -> anyhow::Result<()> {
    if let Some(message_id) = ended.message_id {
        let body = shell_message_body(&ended.command, ended);
        state
            .world
            .finish_shell_message(
                message_id,
                &body,
                &json!({
                    "status": ended.status,
                    "exit_code": ended.exit_code,
                    "duration_ms": ended.duration_ms,
                    "truncated": ended.truncated,
                    "error": ended.error,
                }),
            )
            .await
            ?;
    }
    if let Some(message_id) = ended.message_id {
        if state.world.take_shell_auto_share(message_id).await? {
            let outcome = match ended.exit_code {
                Some(code) => format!("exit {code}"),
                None => ended.error.clone().unwrap_or_else(|| ended.status.clone()),
            };
            send_output(state, ended.agent_id, ended.owner_user_id, &ended.command, &outcome, &ended.output, true).await?;
        }
    }
    state
        .world
        .emit(
            NewEvent::new("shell.finished")
                .agent(ended.agent_id)
                .actor(Some(ended.owner_user_id))
                .payload(json!({
                    "run_id": ended.id,
                    "message_id": ended.message_id,
                    "status": ended.status,
                    "exit_code": ended.exit_code,
                    "duration_ms": ended.duration_ms,
                    "truncated": ended.truncated,
                    "quiet": true,
                })),
            None,
        )
        .await
        ?;
    Ok(())
}

pub async fn get_run(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ShellRun>, ApiError> {
    Ok(Json(owned_run(&state, user.0, id).await?))
}

pub async fn cancel(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ShellRun>, ApiError> {
    owned_run(&state, user.0, id).await?;
    let Some(run) = state.world.cancel_shell_run(id).await.map_err(internal)? else {
        return Err(problem(
            StatusCode::CONFLICT,
            "already_finished",
            "That shell run has already finished",
        ));
    };
    if run.status == "cancelled" && run.started_at.is_none() {
        let ended = ShellEnded {
            id: run.id,
            agent_id: run.agent_id,
            owner_user_id: run.owner_user_id,
            message_id: run.message_id,
            command: run.command.clone(),
            status: "cancelled".to_string(),
            exit_code: None,
            duration_ms: None,
            truncated: false,
            output: String::new(),
            error: None,
        };
        announce_shell_end(&state, &ended).await.map_err(internal)?;
        let run = state.world.shell_run(id).await.map_err(internal)?.unwrap_or(run);
        return Ok(Json(run));
    }
    Ok(Json(run))
}

pub async fn share(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let run = owned_run(&state, user.0, id).await?;
    if !matches!(run.status.as_str(), "done" | "failed" | "cancelled") {
        return Err(problem(
            StatusCode::CONFLICT,
            "still_running",
            "Wait for the command to finish before sharing its output",
        ));
    }
    let outcome = match run.exit_code {
        Some(code) => format!("exit {code}"),
        None => run.error.clone().unwrap_or_else(|| run.status.clone()),
    };
    let msg = send_output(&state, run.agent_id, user.0, &run.command, &outcome, &run.output, false).await.map_err(internal)?;
    state.world.mark_shell_shared(run.id).await.map_err(internal)?;
    Ok((StatusCode::CREATED, Json(json!({ "message_id": msg }))))
}

fn output_body(command: &str, outcome: &str, output: &str, asked: bool) -> String {
    let mut output = output.trim_end().to_string();
    if output.chars().count() > 12_000 {
        let keep: String = output.chars().skip(output.chars().count() - 12_000).collect();
        output = format!("...\n{keep}");
    }
    let how = if asked {
        format!("The owner pressed Run on the command you gave them, `{command}`, and it ran on their device ({outcome}).")
    } else {
        format!("The owner ran `{command}` on their device ({outcome}).")
    };
    if output.is_empty() {
        format!("{how} There was no output.")
    } else if asked {
        format!("{how} Output:\n```\n{output}\n```\nCarry on from here without asking them to confirm it ran.")
    } else {
        format!("{how} They shared the output with you:\n```\n{output}\n```")
    }
}

async fn send_output(state: &AppState, agent_id: Uuid, user: Uuid, command: &str, outcome: &str, output: &str, asked: bool) -> anyhow::Result<Uuid> {
    let body = output_body(command, outcome, output, asked);
    let msg = state
        .world
        .insert_message(NewMessage {
            agent_id,
            author_kind: "user",
            author_user_id: Some(user),
            author_agent_id: None,
            body: &body,
            session_id: None,
            run_id: None,
            meta: if asked { json!({ "shell_share": true, "hidden": true }) } else { json!({ "shell_share": true }) },
        })
        .await?;
    let author = state.world.user_display_name(user).await?.unwrap_or_else(|| "someone".to_string());
    state
        .world
        .emit(
            NewEvent::new("message.created").agent(agent_id).actor(Some(user)).payload(json!({
                "message_id": msg.id,
                "author": author,
                "author_kind": "user",
                "body": body,
                "files": [],
                "shell_share": true,
            })),
            Some((agent_id, "message")),
        )
        .await?;
    Ok(msg.id)
}

#[cfg(test)]
mod tests {
    use super::output_body;

    #[test]
    fn shared_output_says_how_it_ran() {
        let asked = output_body("gh auth status", "exit 0", "ok\n", true);
        assert!(asked.contains("pressed Run") && asked.contains("```\nok\n```") && asked.contains("Carry on"));
        let manual = output_body("ls", "exit 0", "a", false);
        assert!(manual.starts_with("The owner ran `ls`") && manual.contains("shared the output"));
        assert!(asked.starts_with("The owner pressed Run on the command you gave them, `gh auth status`, and it ran"));
        assert!(output_body("true", "exit 0", "", true).ends_with("There was no output."));
        let long = output_body("x", "exit 0", &"y".repeat(13_000), false);
        assert!(long.contains("...\n") && long.len() < 12_400);
    }
}
