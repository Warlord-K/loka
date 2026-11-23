use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::Viewer;
use loka_shared::quick::{self, MemoryAnswer, Opening, QuickBrief};
use loka_shared::world::{AgentMessage, AgentSession, AsideChoice, WorldAgent};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use crate::routes::world::{attachment_meta, internal, load, opencode_allowed_for};
use crate::AppState;

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: &str) -> Refusal {
    (status, Json(json!({ "error": error })))
}

fn failed(status: StatusCode) -> Refusal {
    refuse(status, "Something went wrong. Try again.")
}

fn attached_names(attachments: &Value) -> Vec<String> {
    attachments
        .as_array()
        .map(|list| list.iter().filter_map(|a| a.get("name").and_then(Value::as_str).map(String::from)).collect())
        .unwrap_or_default()
}

async fn asker_name(state: &AppState, user: Uuid) -> Result<String, StatusCode> {
    Ok(state.world.user_display_name(user).await.map_err(internal)?.unwrap_or_else(|| "Your person".to_string()))
}

pub struct Said<'a> {
    pub text: &'a str,
    pub attachments: &'a Value,
    pub spoken: bool,
    pub quote: Option<&'a Value>,
}

#[derive(Deserialize)]
pub struct StartQuick {
    text: String,
    #[serde(default)]
    attachments: Vec<Uuid>,
    #[serde(default)]
    spoken: bool,
    #[serde(default)]
    cli: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    quote: Option<Value>,
}

pub async fn start(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<StartQuick>,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "Write something first."));
    }
    let agent = load(&state, id).await.map_err(failed)?;
    if agent.owner_user_id != Some(viewer.user) {
        return Err(refuse(StatusCode::FORBIDDEN, "Quick chats are only for the agent's owner."));
    }
    let choice = AsideChoice::parse(body.cli.as_deref(), body.model.as_deref()).map_err(|e| refuse(StatusCode::BAD_REQUEST, &e.to_string()))?;
    if choice.cli.as_deref() == Some(loka_shared::backends::OPENCODE)
        && agent.cli != loka_shared::backends::OPENCODE
        && !opencode_allowed_for(&state, &agent).await.map_err(failed)?
    {
        return Err(refuse(StatusCode::BAD_REQUEST, "OpenCode is not available here."));
    }
    if let Some(problem) = state.world.quick_device_problem(id).await.map_err(|e| failed(internal(e)))? {
        return Err(refuse(StatusCode::CONFLICT, &problem));
    }
    let attachments = attachment_meta(&state, &viewer, &body.attachments).await.map_err(failed)?;
    let quote = state.world.clean_quote(&viewer, body.quote.as_ref()).await.map_err(|e| failed(internal(e)))?;
    let name = asker_name(&state, viewer.user).await.map_err(failed)?;
    let session = state
        .world
        .open_quick(Opening { agent_id: id, user_id: viewer.user, asker: &name, text, choice: &choice, thread_root: None })
        .await
        .map_err(|e| refuse(StatusCode::CONFLICT, &e.to_string()))?;
    let memory = state.world.quick_memory_note(&session).await.map_err(|e| failed(internal(e)))?;
    let prompt = quick::opening_prompt(&name, &quick::with_quote(text, quote.as_ref()), &attached_names(&attachments), memory.as_deref());
    let said = Said { text, attachments: &attachments, spoken: body.spoken, quote: quote.as_ref() };
    let message = post_person(&state, &agent, &session, viewer.user, &said).await?;
    let run = state.world.queue_quick(&session, &prompt).await.map_err(|e| refuse(StatusCode::CONFLICT, &e.to_string()))?;
    Ok((StatusCode::CREATED, Json(json!({ "session_id": session.id, "run_id": run.id, "message": message }))))
}

async fn post_person(
    state: &AppState,
    agent: &WorldAgent,
    session: &AgentSession,
    user: Uuid,
    said: &Said<'_>,
) -> Result<AgentMessage, Refusal> {
    let mut meta = json!({ "attachments": said.attachments });
    if said.spoken {
        meta["spoken"] = json!(true);
    }
    if let Some(quote) = said.quote {
        meta["quote"] = quote.clone();
    }
    state
        .world
        .post_quick_message(agent, session, Some(user), said.text, None, meta)
        .await
        .map_err(|e| failed(internal(e)))
}

#[derive(Deserialize)]
pub struct ListQuery {
    agent: Option<Uuid>,
    #[serde(default)]
    threads: bool,
    limit: Option<i64>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<QuickBrief>>, StatusCode> {
    Ok(Json(
        state
            .world
            .quick_sessions(viewer.user, viewer.org(), q.agent, q.threads, q.limit.unwrap_or(60))
            .await
            .map_err(internal)?,
    ))
}

pub async fn deliver_lost_replies(state: &AppState, session: Uuid) {
    let coordinator = loka_agent::runner_server::RunnerCoordinator::new(state.db.clone(), state.world.clone());
    if let Err(error) = coordinator.deliver_lost_quick_replies(Some(session)).await {
        warn!(%error, %session, "could not deliver a lost quick chat reply");
    }
}

async fn own_session(state: &AppState, viewer: &Viewer, id: Uuid) -> Result<AgentSession, Refusal> {
    state
        .world
        .quick_session_for(viewer.user, viewer.org(), id)
        .await
        .map_err(|e| failed(internal(e)))?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, "That quick chat is gone."))
}

pub async fn read(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Refusal> {
    let session = own_session(&state, &viewer, id).await?;
    deliver_lost_replies(&state, session.id).await;
    let messages = state.world.quick_messages(session.id, 300).await.map_err(|e| failed(internal(e)))?;
    let busy = state.world.session_busy(session.id).await.map_err(|e| failed(internal(e)))?;
    Ok(Json(json!({
        "id": session.id,
        "agent_id": session.agent_id,
        "title": session.title,
        "backend": session.backend,
        "model": session.model,
        "status": session.status,
        "thread_root": quick::thread_root(&session),
        "busy": busy,
        "messages": messages,
    })))
}

#[derive(Deserialize)]
pub struct QuickPost {
    #[serde(default)]
    text: String,
    #[serde(default)]
    attachments: Vec<Uuid>,
    #[serde(default)]
    spoken: bool,
    #[serde(default)]
    answers: Option<Uuid>,
    #[serde(default)]
    quote: Option<Value>,
}

pub async fn send(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<QuickPost>,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    let session = own_session(&state, &viewer, id).await?;
    let agent = load(&state, session.agent_id).await.map_err(failed)?;
    if agent.owner_user_id != Some(viewer.user) {
        return Err(refuse(StatusCode::FORBIDDEN, "Quick chats are only for the agent's owner."));
    }
    let text = body.text.trim();
    if let Some(card) = body.answers {
        return answer_card(&state, &viewer, &agent, &session, card, text).await;
    }
    if text.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "Write something first."));
    }
    let attachments = attachment_meta(&state, &viewer, &body.attachments).await.map_err(failed)?;
    let quote = state.world.clean_quote(&viewer, body.quote.as_ref()).await.map_err(|e| failed(internal(e)))?;
    let said = Said { text, attachments: &attachments, spoken: body.spoken, quote: quote.as_ref() };
    let (message, run) = continue_session(&state, &agent, &session, viewer.user, &said).await?;
    Ok((StatusCode::CREATED, Json(json!({ "session_id": session.id, "run_id": run, "message": message }))))
}

async fn ready_prompt(state: &AppState, agent: &WorldAgent, session: &AgentSession, said: &Said<'_>) -> Result<String, Refusal> {
    if session.status == "archived" {
        return Err(refuse(StatusCode::CONFLICT, "This quick chat is closed. Start a new one."));
    }
    if let Some(problem) = state.world.quick_device_problem(agent.id).await.map_err(|e| failed(internal(e)))? {
        return Err(refuse(StatusCode::CONFLICT, &problem));
    }
    let name = quick::asker_name(session);
    let names = attached_names(said.attachments);
    let prompt_text = quick::with_quote(said.text, said.quote);
    let started = state.world.quick_has_started(session.id).await.map_err(|e| failed(internal(e)))?;
    let prompt = match (started, quick::thread_root(session)) {
        (true, _) => quick::next_prompt(&name, &prompt_text, &names),
        (false, root) => {
            let memory = state.world.quick_memory_note(session).await.map_err(|e| failed(internal(e)))?;
            match root {
                Some(root) => {
                    let transcript = state.world.quick_thread_transcript(agent.id, root).await.map_err(|e| failed(internal(e)))?;
                    quick::thread_prompt(&name, &transcript, &prompt_text, &names, memory.as_deref())
                }
                None => quick::opening_prompt(&name, &prompt_text, &names, memory.as_deref()),
            }
        }
    };
    let plans = loka_shared::quests::waiting_plans_note(&state.world.waiting_plans(agent.id).await.map_err(|e| failed(internal(e)))?);
    Ok(if plans.is_empty() { prompt } else { format!("{prompt}\n\n{plans}") })
}

async fn continue_session(
    state: &AppState,
    agent: &WorldAgent,
    session: &AgentSession,
    user: Uuid,
    said: &Said<'_>,
) -> Result<(AgentMessage, Uuid), Refusal> {
    let prompt = ready_prompt(state, agent, session, said).await?;
    let message = post_person(state, agent, session, user, said).await?;
    let run = state.world.queue_quick(session, &prompt).await.map_err(|e| refuse(StatusCode::CONFLICT, &e.to_string()))?;
    Ok((message, run.id))
}

async fn answer_card(
    state: &AppState,
    viewer: &Viewer,
    agent: &WorldAgent,
    session: &AgentSession,
    card: Uuid,
    text: &str,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    if !state.world.is_memory_card(agent.id, card).await.map_err(|e| failed(internal(e)))? {
        return Err(refuse(StatusCode::BAD_REQUEST, "Only memory offers can be answered here."));
    }
    let keep = text == quick::KEEP;
    let answer = state
        .world
        .answer_memory(agent, viewer.user, card, keep)
        .await
        .map_err(|e| refuse(StatusCode::CONFLICT, &e.to_string()))?;
    let note = match answer {
        MemoryAnswer::AlreadyAnswered => return Err(refuse(StatusCode::CONFLICT, "That was already answered.")),
        MemoryAnswer::Kept(_) => Some(format!("Saved to {}'s notes.", agent.name)),
        MemoryAnswer::Skipped => None,
    };
    let message = state
        .world
        .post_quick_message(
            agent,
            session,
            Some(viewer.user),
            if keep { quick::KEEP } else { quick::SKIP },
            None,
            json!({ "answers": card }),
        )
        .await
        .map_err(|e| failed(internal(e)))?;
    if let Some(note) = note {
        state
            .world
            .post_quick_message(agent, session, None, &note, None, json!({ "system": true }))
            .await
            .map_err(|e| failed(internal(e)))?;
    }
    Ok((StatusCode::CREATED, Json(json!({ "session_id": session.id, "message": message }))))
}

pub async fn close(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Refusal> {
    let session = own_session(&state, &viewer, id).await?;
    state.world.close_quick(session.id).await.map_err(|e| failed(internal(e)))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reply_in_thread(
    state: &AppState,
    viewer: &Viewer,
    agent: &WorldAgent,
    root: Uuid,
    said: &Said<'_>,
) -> Result<Option<AgentMessage>, StatusCode> {
    let text = said.text;
    if agent.owner_user_id != Some(viewer.user) || text.is_empty() || text.starts_with('!') {
        return Ok(None);
    }
    if state.world.quick_device_problem(agent.id).await.map_err(internal)?.is_some() {
        return Ok(None);
    }
    let session = match state.world.quick_thread_session(agent.id, root).await.map_err(internal)? {
        Some(session) => session,
        None => {
            let name = asker_name(state, viewer.user).await?;
            let opened = state
                .world
                .open_quick(Opening {
                    agent_id: agent.id,
                    user_id: viewer.user,
                    asker: &name,
                    text,
                    choice: &AsideChoice::default(),
                    thread_root: Some(root),
                })
                .await;
            match opened {
                Ok(session) => session,
                Err(error) => {
                    warn!(%error, agent = %agent.slug, "thread stays in the main chat");
                    return Ok(None);
                }
            }
        }
    };
    if quick::asker(&session) != Some(viewer.user) {
        return Ok(None);
    }
    continue_session(state, agent, &session, viewer.user, said)
        .await
        .map(|(message, _)| Some(message))
        .map_err(|(status, _)| status)
}

pub async fn voice_request(
    state: &AppState,
    agent: &WorldAgent,
    session: &AgentSession,
    user: Uuid,
    text: &str,
    context: &str,
    call_id: &str,
) -> Result<(), Refusal> {
    let said = Said { text, attachments: &json!([]), spoken: true, quote: None };
    let prompt = ready_prompt(state, agent, session, &said).await?;
    let prompt = if context.is_empty() { prompt } else { format!("{prompt}\n\n[the call so far]\n{context}") };
    let prompt = loka_shared::voice::with_note(prompt, &json!({ "voice": true }));
    state
        .world
        .post_quick_message(agent, session, Some(user), text, None, json!({ "kind": "voice", "voice_request": true, "call_id": call_id, "spoken": true }))
        .await
        .map_err(|e| failed(internal(e)))?;
    state.world.queue_quick(session, &prompt).await.map_err(|e| refuse(StatusCode::CONFLICT, &e.to_string()))?;
    Ok(())
}
