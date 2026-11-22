use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use chrono::Utc;
use loka_shared::access::Viewer;
use loka_shared::quick;
use loka_shared::world::{AgentSession, NewEvent, NewMessage, WorldAgent, WorldEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, warn};
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

const TEMPLATE: &str = include_str!("../../templates/world_voice.txt");
const QUICK_NOTE: &str = "\n\nThis call belongs to a quick chat: a side conversation with {person}, and the thread above is that chat. What you send with send_to_agent goes to your quick-chat self, which answers with this chat's context, model and tools. Your main chat is not part of this call.\n";
pub const VOICE_APP: &str = "xai/grok-voice/realtime";
const VOICE_ALIAS: &str = "grok-voice";
pub const USD_PER_SECOND: f64 = 0.00083;
const TOKEN_SECONDS: u64 = 120;
const SAMPLE_RATE: u32 = 24000;
const TURN_SILENCE_MS: u32 = 1200;
const TURN_PREFIX_MS: u32 = 333;
const GROK_VOICES: &[&str] = &[
    "carina", "zagan", "helix", "orion", "luna", "iris", "altair", "zenith", "perseus", "helios", "lux", "kepler", "rigel", "cosmo",
    "celeste", "ursa", "sirius", "lumen", "castor", "naksh", "atlas", "aurora", "liora", "ara", "eve", "leo", "rex", "sal",
];
const FEMININE: &[&str] = &["eve", "ara", "luna", "aurora", "celeste", "iris", "liora", "carina"];
const MASCULINE: &[&str] = &["leo", "rex", "orion", "atlas", "helix", "kepler", "rigel", "castor"];

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "voice route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn max_minutes() -> u64 {
    std::env::var("WORLD_VOICE_MAX_MINUTES").ok().and_then(|v| v.parse().ok()).filter(|m| *m > 0).unwrap_or(30)
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}...", flat.chars().take(max).collect::<String>())
}

fn seed(text: &str) -> usize {
    text.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
}

pub fn grok_voice(agent: &WorldAgent) -> String {
    voice_from_look(&agent.look, agent.persona.as_deref().unwrap_or(""), &agent.slug)
}

fn voice_from_look(look: &Value, persona: &str, slug: &str) -> String {
    let named = |key: &str| look.get(key).and_then(Value::as_str).map(|v| v.trim().to_ascii_lowercase()).unwrap_or_default();
    let chosen = named("live_voice");
    if GROK_VOICES.contains(&chosen.as_str()) {
        return chosen;
    }
    let pool = match named("voice").as_bytes() {
        [b'a' | b'b', b'f', b'_', ..] => FEMININE,
        [b'a' | b'b', b'm', b'_', ..] => MASCULINE,
        _ => persona_pool(persona),
    };
    pool[seed(slug) % pool.len()].to_string()
}

fn persona_pool(persona: &str) -> &'static [&'static str] {
    let lower = persona.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphabetic()).collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(w));
    if has(&["she", "her", "hers", "woman", "girl", "lady", "queen"]) {
        FEMININE
    } else if has(&["he", "him", "his", "man", "guy", "boy", "king"]) {
        MASCULINE
    } else {
        GROK_VOICES
    }
}

async fn mint_token(key: &str) -> anyhow::Result<String> {
    let resp = reqwest::Client::new()
        .post("https://rest.fal.ai/tokens/")
        .header("Authorization", format!("Key {key}"))
        .timeout(Duration::from_secs(10))
        .json(&json!({ "allowed_apps": [VOICE_ALIAS], "token_expiration": TOKEN_SECONDS }))
        .send()
        .await?
        .error_for_status()?;
    let body: Value = resp.json().await?;
    let token = match &body {
        Value::String(s) => Some(s.clone()),
        other => ["token", "detail"].iter().find_map(|k| other.get(*k).and_then(Value::as_str).map(str::to_string)),
    };
    token.filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("fal token response had no token"))
}

pub async fn fal_key_for(state: &AppState, user: Uuid) -> Option<String> {
    match state.world.provider_key(user, "FAL_KEY").await {
        Ok(key) => key,
        Err(e) => {
            warn!(error = %e, "could not read the caller's FAL_KEY");
            loka_shared::providers::shared_server_value("FAL_KEY")
        }
    }
}

async fn token_or_status(state: &AppState, user: Uuid) -> Result<String, StatusCode> {
    let key = fal_key_for(state, user).await.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    mint_token(&key).await.map_err(|e| {
        warn!(error = %e, "fal realtime token request failed");
        StatusCode::BAD_GATEWAY
    })
}

fn event_line(ev: &WorldEvent) -> Option<String> {
    if matches!(ev.event_type.as_str(), "agent.status" | "voice.transcript" | "session.compacted") {
        return None;
    }
    let p = &ev.payload;
    let detail = ["title", "body", "answer", "name", "label", "error", "pr_url", "verdict", "status"]
        .iter()
        .find_map(|k| p.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()))
        .map(|s| format!(": {}", clip(s, 160)))
        .unwrap_or_default();
    Some(format!("- {} {}{}", ev.created_at.format("%b %d %H:%M"), ev.event_type, detail))
}

fn bullet_or_none(lines: Vec<String>) -> String {
    if lines.is_empty() {
        "(none)".to_string()
    } else {
        lines.join("\n")
    }
}

async fn status_text(state: &AppState, agent: &WorldAgent) -> Result<(String, String), StatusCode> {
    let tasks = state.world.agent_tasks(Some(agent.id), true, 10).await.map_err(internal)?;
    let running = state.world.active_task_runs(agent.id).await.map_err(internal)?;
    let status = format!(
        "{}{}{}",
        agent.status,
        agent.status_detail.as_deref().filter(|d| !d.trim().is_empty()).map(|d| format!(" ({})", clip(d, 160))).unwrap_or_default(),
        if running > 0 { format!(", {running} quest session(s) running now") } else { String::new() }
    );
    let quests = tasks
        .iter()
        .map(|t| {
            format!(
                "- {} [{}]{}{}",
                clip(&t.title, 120),
                t.status,
                t.repo.as_deref().map(|r| format!(" in {r}")).unwrap_or_default(),
                t.pr_url.as_deref().map(|u| format!(", PR {u}")).unwrap_or_default()
            )
        })
        .collect();
    Ok((status, bullet_or_none(quests)))
}

async fn instructions(state: &AppState, agent: &WorldAgent, user: Uuid, person: &str, quick: Option<&AgentSession>) -> Result<String, StatusCode> {
    let owner = match agent.owner_user_id {
        Some(id) => state.world.user_display_name(id).await.map_err(internal)?,
        None => None,
    }
    .unwrap_or_else(|| "the team".to_string());
    let rules: Vec<String> = state
        .world
        .rules_for(agent.owner_user_id, agent.org_id)
        .await
        .map_err(internal)?
        .iter()
        .filter(|r| r.scope == "all" || r.scope == "talk")
        .map(|r| format!("- {}", r.body.trim()))
        .collect();
    let rules = if rules.is_empty() {
        String::new()
    } else {
        format!("\nRules from {owner}. Always follow them:\n{}\n", rules.join("\n"))
    };
    let (status, quests) = status_text(state, agent).await?;
    let messages = match quick {
        Some(session) => {
            let mut all = state.world.quick_messages(session.id, 20).await.map_err(internal)?;
            all.retain(|m| m.author_kind != "system");
            all
        }
        None => state.world.list_messages(agent.id, user, 20).await.map_err(internal)?,
    };
    let thread = messages
        .into_iter()
        .filter(|m| m.session_role.as_deref() != Some("aside"))
        .map(|m| {
            let who = match m.author_kind.as_str() {
                "user" => person.to_string(),
                "agent" if m.agent_id != agent.id => "you, to another agent".to_string(),
                "agent" if m.author_agent_id.is_none() || m.author_agent_id == Some(agent.id) => "you".to_string(),
                "agent" => "another agent".to_string(),
                other => other.to_string(),
            };
            let by_voice = if m.meta.get("kind").and_then(Value::as_str) == Some("voice") { " (by voice)" } else { "" };
            format!("- {who}{by_voice}: {}", clip(&m.body, 400))
        })
        .collect();
    let owned = state.world.owned_agent_ids(user).await.map_err(internal)?;
    let events = state
        .world
        .recent_events(Some(agent.id), 30)
        .await
        .map_err(internal)?
        .iter()
        .filter(|e| !loka_shared::event_scope::private_from(e, user, &owned))
        .filter_map(event_line)
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let persona = agent.persona.as_deref().map(str::trim).filter(|p| !p.is_empty()).unwrap_or("A capable, friendly engineer who takes ownership of their work.");
    let main = if agent.is_main {
        "\nYou are the main agent: the guide and coordinator of Loka. You know who does what and route work to the right agent through send_to_agent.\n"
    } else {
        ""
    };
    let text = TEMPLATE
        .replace("{persona}", &format!("{persona}{main}"))
        .replace("{rules}", &rules)
        .replace("{status}", &status)
        .replace("{quests}", &quests)
        .replace("{thread}", &bullet_or_none(thread))
        .replace("{events}", &bullet_or_none(events))
        .replace("{now}", &Utc::now().format("%Y-%m-%d %H:%M UTC").to_string())
        .replace("{name}", &agent.name)
        .replace("{owner}", &owner)
        .replace("{person}", person);
    let text = match quick {
        Some(_) => format!("{text}{}", QUICK_NOTE.replace("{person}", person)),
        None => text,
    };
    Ok(text.chars().take(19_000).collect())
}

fn turn_detection() -> Value {
    json!({ "type": "server_vad", "silence_duration_ms": TURN_SILENCE_MS, "prefix_padding_ms": TURN_PREFIX_MS })
}

fn tools() -> Value {
    json!([
        {
            "type": "function",
            "name": "send_to_agent",
            "description": "Hand a request to your full self (your main mind with tools, code, quests and memory). Use it for any real work or anything you cannot answer from what you already know. The result arrives later as an [update from your full self] message.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "A clear, self-contained request in the person's words, with every detail needed." }
                },
                "required": ["text"]
            }
        },
        {
            "type": "function",
            "name": "agent_status",
            "description": "Check what you are doing right now: your status and your open quests.",
            "parameters": { "type": "object", "properties": {} }
        }
    ])
}

#[derive(Deserialize, Default)]
pub struct StartQuery {
    #[serde(default)]
    greeted: bool,
}

pub async fn start(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Query(query): Query<StartQuery>,
) -> Result<Json<Value>, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    start_call(&state, user.0, &agent, None, query.greeted).await
}

async fn start_call(state: &AppState, user: Uuid, agent: &WorldAgent, quick: Option<&AgentSession>, greeted: bool) -> Result<Json<Value>, StatusCode> {
    let id = agent.id;
    let token = token_or_status(state, user).await?;
    let person = state.world.user_display_name(user).await.map_err(internal)?.unwrap_or_else(|| "someone".to_string());
    let mut text = instructions(state, agent, user, &person, quick).await?;
    let greeting = if greeted { stored_greeting(state, id, &grok_voice(agent)).await?.map(|g| g.phrase) } else { None };
    if let Some(phrase) = &greeting {
        text.push_str(&format!(
            "\n\nYou already opened this call by saying \"{phrase}\". Do not greet again. Wait for {person} to speak, then answer naturally.\n"
        ));
    }
    let session = json!({
        "instructions": text,
        "voice": grok_voice(agent),
        "turn_detection": turn_detection(),
        "audio": {
            "input": { "format": { "type": "audio/pcm", "rate": SAMPLE_RATE }, "transcription": { "model": "grok-transcribe" } },
            "output": { "format": { "type": "audio/pcm", "rate": SAMPLE_RATE } }
        },
        "tools": tools(),
    });
    Ok(Json(json!({
        "app": VOICE_APP,
        "token": token,
        "token_seconds": TOKEN_SECONDS,
        "sample_rate": SAMPLE_RATE,
        "usd_per_minute": USD_PER_SECOND * 60.0,
        "max_minutes": max_minutes(),
        "session": session,
        "greeting": greeting,
    })))
}

async fn quick_scope(state: &AppState, viewer: &Viewer, id: Uuid, closed_ok: bool) -> Result<(AgentSession, WorldAgent), Refusal> {
    let session = state
        .world
        .quick_session_for(viewer.user, viewer.org(), id)
        .await
        .map_err(|e| refuse_with(internal(e), "Something went wrong. Try again."))?
        .ok_or_else(|| refuse_with(StatusCode::NOT_FOUND, "That quick chat is gone."))?;
    let agent = state
        .world
        .get_agent(session.agent_id)
        .await
        .map_err(|e| refuse_with(internal(e), "Something went wrong. Try again."))?
        .ok_or_else(|| refuse_with(StatusCode::NOT_FOUND, "That agent is gone."))?;
    refuse_quick_call(&session, agent.owner_user_id, viewer.user, closed_ok).map_or(Ok((session, agent)), |(status, error)| Err(refuse_with(status, error)))
}

fn refuse_quick_call(session: &AgentSession, agent_owner: Option<Uuid>, user: Uuid, closed_ok: bool) -> Option<(StatusCode, &'static str)> {
    if agent_owner != Some(user) || quick::asker(session) != Some(user) {
        return Some((StatusCode::FORBIDDEN, "Calls from a quick chat are only for the agent's owner."));
    }
    if session.status == "archived" && !closed_ok {
        return Some((StatusCode::CONFLICT, "This quick chat is closed. Start a new one to call."));
    }
    if quick::thread_root(session).is_some() {
        return Some((StatusCode::CONFLICT, "Calls are not available in thread replies."));
    }
    None
}

pub type Refusal = (StatusCode, Json<Value>);

fn refuse_with(status: StatusCode, error: &str) -> Refusal {
    (status, Json(json!({ "error": error })))
}

pub async fn start_quick(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Query(query): Query<StartQuery>,
) -> Result<Json<Value>, Refusal> {
    let (session, agent) = quick_scope(&state, &viewer, id, false).await?;
    if let Some(problem) = state.world.quick_device_problem(agent.id).await.map_err(|e| refuse_with(internal(e), "Something went wrong. Try again."))? {
        return Err(refuse_with(StatusCode::CONFLICT, &problem));
    }
    start_call(&state, viewer.user, &agent, Some(&session), query.greeted).await.map_err(|status| {
        let error = match status {
            StatusCode::SERVICE_UNAVAILABLE => "Live voice is not set up on this server.",
            StatusCode::BAD_GATEWAY => "Live voice did not answer. Try again.",
            _ => "Something went wrong. Try again.",
        };
        refuse_with(status, error)
    })
}

pub async fn greeting_quick(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<Response, Refusal> {
    let (_, agent) = quick_scope(&state, &viewer, id, false).await?;
    greeting(State(state), Extension(UserId(viewer.user)), Path(agent.id))
        .await
        .map_err(|status| refuse_with(status, "No greeting yet."))
}

pub async fn status_quick(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Refusal> {
    let (_, agent) = quick_scope(&state, &viewer, id, false).await?;
    status(State(state), Path(agent.id)).await.map_err(|status| refuse_with(status, "Something went wrong. Try again."))
}

struct Greeting {
    voice: String,
    phrase: String,
    audio: Vec<u8>,
    sample_rate: i32,
}

async fn stored_greeting(state: &AppState, agent: Uuid, voice: &str) -> Result<Option<Greeting>, StatusCode> {
    let row: Option<(String, String, Vec<u8>, i32)> =
        sqlx::query_as("SELECT voice, phrase, audio, sample_rate FROM agent_voice_greetings WHERE agent_id = $1 AND voice = $2")
            .bind(agent)
            .bind(voice)
            .fetch_optional(state.world.pool())
            .await
            .map_err(|e| internal(e.into()))?;
    Ok(row.map(|(voice, phrase, audio, sample_rate)| Greeting { voice, phrase, audio, sample_rate }))
}

fn greeting_phrase(person: Option<&str>) -> String {
    let first = person.and_then(|p| p.split_whitespace().next()).filter(|f| f.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'') && f.chars().count() <= 20);
    match first {
        Some(name) => format!("Hey {name}, what's up?"),
        None => "Hey, what's up?".to_string(),
    }
}

async fn render_greeting(key: &str, voice: &str, phrase: &str) -> anyhow::Result<Vec<u8>> {
    let token = mint_token(key).await?;
    let url = format!("wss://fal.run/{VOICE_APP}?fal_jwt_token={}&max_buffering=30", urlencoding::encode(&token));
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(url)).await??;
    let send = |event: Value| WsMessage::Binary(crate::msgpack::encode(&event).into());
    ws.send(send(json!({
        "type": "session.update",
        "session": {
            "instructions": "You are a voice actor reading one short line on a phone call. Say exactly the line you are given, warmly and casually, then stop. Add nothing.",
            "voice": voice,
            "turn_detection": null,
            "audio": { "output": { "format": { "type": "audio/pcm", "rate": SAMPLE_RATE } } },
        },
    })))
    .await?;
    let mut asked = false;
    let mut audio = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let ask = json!({
        "type": "conversation.item.create",
        "item": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": format!("Say exactly this line and nothing else: {phrase}") }] },
    });
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next()).await.map_err(|_| anyhow::anyhow!("greeting timed out"))?;
        let Some(frame) = next else { break };
        let event = match frame? {
            WsMessage::Binary(bytes) => crate::msgpack::decode(&bytes),
            WsMessage::Text(text) => serde_json::from_str(text.as_str()).ok(),
            WsMessage::Close(_) => break,
            _ => None,
        };
        let Some(event) = event else { continue };
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "session.updated" if !asked => {
                asked = true;
                ws.send(send(ask.clone())).await?;
                ws.send(send(json!({ "type": "response.create" }))).await?;
            }
            "response.output_audio.delta" | "response.audio.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    audio.extend(base64::engine::general_purpose::STANDARD.decode(delta)?);
                }
            }
            "response.done" if asked => break,
            "error" => anyhow::bail!("voice service error: {}", event.get("error").map(Value::to_string).unwrap_or_default()),
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    if audio.len() < SAMPLE_RATE as usize / 2 {
        anyhow::bail!("greeting came back empty");
    }
    Ok(audio)
}

pub async fn greeting(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Response, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let voice = grok_voice(&agent);
    let person = state.world.user_display_name(user.0).await.map_err(internal)?;
    let phrase = greeting_phrase(person.as_deref());
    let stored = stored_greeting(&state, id, &voice).await?.filter(|g| g.phrase == phrase);
    let greeting = match stored {
        Some(g) => g,
        None => {
            let key = fal_key_for(&state, user.0).await.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            let audio = render_greeting(&key, &voice, &phrase).await.map_err(|e| {
                warn!(error = %e, agent = %id, "could not record the call greeting");
                StatusCode::BAD_GATEWAY
            })?;
            sqlx::query(
                "INSERT INTO agent_voice_greetings (agent_id, voice, phrase, audio, sample_rate) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (agent_id) DO UPDATE SET voice = EXCLUDED.voice, phrase = EXCLUDED.phrase, audio = EXCLUDED.audio,
                     sample_rate = EXCLUDED.sample_rate, created_at = NOW()",
            )
            .bind(id)
            .bind(&voice)
            .bind(&phrase)
            .bind(&audio)
            .bind(SAMPLE_RATE as i32)
            .execute(state.world.pool())
            .await
            .map_err(|e| internal(e.into()))?;
            Greeting { voice, phrase, audio, sample_rate: SAMPLE_RATE as i32 }
        }
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    headers.insert("x-greeting-rate", HeaderValue::from(greeting.sample_rate));
    for (name, value) in [("x-greeting-voice", &greeting.voice), ("x-greeting-text", &greeting.phrase)] {
        if let Ok(v) = HeaderValue::from_str(&urlencoding::encode(value)) {
            headers.insert(name, v);
        }
    }
    Ok((headers, greeting.audio).into_response())
}

pub async fn token(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, StatusCode> {
    state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "token": token_or_status(&state, user.0).await?, "token_seconds": TOKEN_SECONDS })))
}

pub async fn status(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let (status, quests) = status_text(&state, &agent).await?;
    Ok(Json(json!({ "text": format!("Status: {status}\nOpen quests:\n{quests}") })))
}

#[derive(Deserialize)]
pub struct Line {
    role: String,
    text: String,
    key: Option<String>,
    #[serde(default)]
    partial: bool,
}

#[derive(Deserialize)]
pub struct Lines {
    #[serde(default)]
    lines: Vec<Line>,
    request: Option<String>,
    context: Option<String>,
    #[serde(default)]
    end: bool,
    seconds: Option<f64>,
    call_id: Option<String>,
    client: Option<String>,
    diagnostics: Option<Value>,
}

pub async fn lines(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<Lines>,
) -> Result<StatusCode, StatusCode> {
    let agent = state.world.get_agent(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    save_lines(&state, user.0, &agent, None, body).await
}

pub async fn lines_quick(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<Lines>,
) -> Result<StatusCode, Refusal> {
    let (session, agent) = quick_scope(&state, &viewer, id, true).await?;
    save_lines(&state, viewer.user, &agent, Some(&session), body)
        .await
        .map_err(|status| refuse_with(status, "Could not save that part of the call."))
}

async fn save_lines(state: &AppState, user: Uuid, agent: &WorldAgent, quick: Option<&AgentSession>, body: Lines) -> Result<StatusCode, StatusCode> {
    let id = agent.id;
    if body.lines.len() > 200 || body.lines.iter().any(|l| !matches!(l.role.as_str(), "user" | "agent")) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let call = body.call_id.as_deref().map(|c| clip(c, 64)).unwrap_or_default();
    let author = state.world.user_display_name(user).await.map_err(internal)?.unwrap_or_else(|| "someone".to_string());
    let thread = match quick {
        Some(_) => Some(user),
        None => (agent.owner_user_id != Some(user)).then_some(user),
    };
    let session = quick.map(|s| s.id);
    let mut stored = 0;
    let mut finals = 0;
    for line in &body.lines {
        let text = tidy(&line.text, 4000);
        if text.is_empty() {
            continue;
        }
        let user_line = line.role == "user";
        if let Some(turn) = line.key.as_deref().and_then(loka_shared::voice::turn_key).filter(|_| !call.is_empty()) {
            let changed = state
                .world
                .save_call_turn(loka_shared::voice::CallTurn {
                    agent_id: id,
                    call_id: &call,
                    turn: &turn,
                    from_user: user_line.then_some(user),
                    body: &text,
                    partial: line.partial,
                    thread,
                    session,
                })
                .await
                .map_err(internal)?;
            if changed {
                stored += 1;
                if !line.partial {
                    finals += 1;
                }
            }
            continue;
        }
        let mut meta = json!({ "kind": "voice", "call_id": call });
        if let Some(session) = session {
            meta["quick"] = json!(session);
        }
        state
            .world
            .insert_message(NewMessage {
                agent_id: id,
                author_kind: if user_line { "user" } else { "agent" },
                author_user_id: user_line.then_some(user),
                author_agent_id: None,
                body: &text,
                session_id: session,
                run_id: None,
                meta: loka_shared::guests::in_thread(meta, thread),
            })
            .await
            .map_err(internal)?;
        stored += 1;
        finals += 1;
    }
    if stored > 0 {
        let mut payload = json!({ "lines": stored, "call_id": call, "quiet": finals == 0 });
        if let Some(session) = session {
            payload["quick_session_id"] = json!(session);
        }
        state.world.emit(NewEvent::new("voice.transcript").agent(id).actor(Some(user)).payload(loka_shared::guests::in_thread(payload, thread)), None).await.map_err(internal)?;
    }
    if let Some(request) = body.request.as_deref().map(|r| tidy(r, 4000)).filter(|r| !r.is_empty()) {
        let context = body.context.as_deref().map(|c| tidy(c, 6000)).unwrap_or_default();
        if let Some(session) = quick {
            crate::routes::quick::voice_request(state, agent, session, user, &request, &context, &call).await.map_err(|(status, _)| status)?;
        } else {
            let msg = state
                .world
                .insert_message(NewMessage {
                    agent_id: id,
                    author_kind: "user",
                    author_user_id: Some(user),
                    author_agent_id: None,
                    body: &request,
                    session_id: None,
                    run_id: None,
                    meta: loka_shared::guests::in_thread(json!({ "kind": "voice", "voice_request": true, "call_id": call }), thread),
                })
                .await
                .map_err(internal)?;
            state
                .world
                .emit(
                    NewEvent::new("message.created").agent(id).actor(Some(user)).payload(loka_shared::guests::in_thread(
                        json!({
                            "message_id": msg.id,
                            "author": author,
                            "author_kind": "user",
                            "body": request,
                            "voice": true,
                            "transcript": context,
                        }),
                        thread,
                    )),
                    Some((id, "message")),
                )
                .await
                .map_err(internal)?;
        }
    }
    if body.end {
        let transcript = body.context.as_deref().map(|c| tidy(c, 8000)).unwrap_or_default();
        let seconds = body.seconds.unwrap_or(0.0).clamp(0.0, 4.0 * 3600.0);
        let mut payload = json!({
            "author": author,
            "agent": agent.name,
            "transcript": transcript,
            "seconds": seconds.round(),
            "cost_usd": (seconds * USD_PER_SECOND * 1000.0).round() / 1000.0,
            "call_id": call,
            "client": body.client.as_deref().map(|c| clip(c, 16)),
            "diagnostics": body.diagnostics.as_ref().and_then(call_diagnostics),
        });
        if let Some(session) = session {
            payload["quick_session_id"] = json!(session);
        }
        let wake = (!transcript.is_empty() && quick.is_none()).then_some((id, "voice.call"));
        state.world.emit(NewEvent::new("voice.ended").agent(id).actor(Some(user)).payload(loka_shared::guests::in_thread(payload, thread)), wake).await.map_err(internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn call_diagnostics(raw: &Value) -> Option<Value> {
    let fields = raw.as_object()?;
    let kept: serde_json::Map<String, Value> = fields
        .iter()
        .take(24)
        .filter(|(key, _)| key.len() <= 32 && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .filter_map(|(key, value)| {
            let value = match value {
                Value::Bool(_) | Value::Number(_) => value.clone(),
                Value::String(text) => Value::String(clip(text, 240)),
                _ => return None,
            };
            Some((key.clone(), value))
        })
        .collect();
    (!kept.is_empty()).then_some(Value::Object(kept))
}

fn tidy(text: &str, max: usize) -> String {
    text.replace('\u{2014}', " - ").trim().chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_session(asker: Uuid, status: &str, thread_root: Option<Uuid>) -> AgentSession {
        let mut asked_by = json!({ "user_id": asker, "name": "Ada" });
        if let Some(root) = thread_root {
            asked_by["thread_root"] = json!(root);
        }
        serde_json::from_value(json!({
            "id": Uuid::new_v4(),
            "agent_id": Uuid::new_v4(),
            "role": "quick",
            "backend": "claude",
            "model": "haiku",
            "status": status,
            "usage_baseline": {},
            "context_tokens": 0,
            "review_rounds": 0,
            "asked_by": asked_by,
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
        }))
        .expect("a session")
    }

    #[test]
    fn only_the_owner_can_call_from_their_own_quick_chat() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mine = quick_session(owner, "active", None);
        assert_eq!(refuse_quick_call(&mine, Some(owner), owner, false), None);
        assert_eq!(refuse_quick_call(&mine, Some(owner), other, false).map(|r| r.0), Some(StatusCode::FORBIDDEN));
        assert_eq!(refuse_quick_call(&mine, Some(other), owner, false).map(|r| r.0), Some(StatusCode::FORBIDDEN));
        assert_eq!(refuse_quick_call(&mine, None, owner, false).map(|r| r.0), Some(StatusCode::FORBIDDEN));
        let theirs = quick_session(other, "active", None);
        assert_eq!(refuse_quick_call(&theirs, Some(owner), owner, false).map(|r| r.0), Some(StatusCode::FORBIDDEN));
    }

    #[test]
    fn a_closed_quick_chat_cannot_start_a_call_but_can_still_save_the_last_lines() {
        let owner = Uuid::new_v4();
        let closed = quick_session(owner, "archived", None);
        assert_eq!(refuse_quick_call(&closed, Some(owner), owner, false).map(|r| r.0), Some(StatusCode::CONFLICT));
        assert_eq!(refuse_quick_call(&closed, Some(owner), owner, true), None);
    }

    #[test]
    fn thread_replies_have_no_calls() {
        let owner = Uuid::new_v4();
        let thread = quick_session(owner, "active", Some(Uuid::new_v4()));
        assert_eq!(refuse_quick_call(&thread, Some(owner), owner, false).map(|r| r.0), Some(StatusCode::CONFLICT));
    }

    #[test]
    fn a_quick_call_tells_the_voice_where_its_requests_go() {
        let note = QUICK_NOTE.replace("{person}", "Ada");
        assert!(note.contains("quick-chat self"));
        assert!(!note.contains('\u{2014}'));
    }

    #[test]
    fn calls_ask_the_provider_to_wait_for_a_real_pause() {
        let session = turn_detection();
        assert_eq!(session["type"], "server_vad");
        assert_eq!(session["silence_duration_ms"], 1200);
        assert_eq!(session["prefix_padding_ms"], 333);
    }

    #[test]
    fn a_chosen_call_voice_is_used_whatever_its_case() {
        assert_eq!(voice_from_look(&json!({ "live_voice": " Ara " }), "", "hana"), "ara");
    }

    #[test]
    fn a_cleared_or_unknown_voice_falls_back_to_a_default() {
        let fallback = voice_from_look(&json!({}), "She is calm.", "haiku-hana");
        assert!(FEMININE.contains(&fallback.as_str()));
        for look in [
            json!({ "live_voice": null, "voice": null }),
            json!({ "live_voice": "", "voice": "" }),
            json!({ "live_voice": "nobody", "voice": "xx" }),
            json!({ "live_voice": 7, "voice": false }),
        ] {
            assert_eq!(voice_from_look(&look, "She is calm.", "haiku-hana"), fallback, "{look}");
        }
        assert!(GROK_VOICES.contains(&voice_from_look(&json!({ "voice": "" }), "", "x").as_str()));
    }

    #[test]
    fn the_speech_voice_sets_the_call_voice_gender() {
        assert!(FEMININE.contains(&voice_from_look(&json!({ "voice": "af_heart" }), "He is calm.", "a").as_str()));
        assert!(MASCULINE.contains(&voice_from_look(&json!({ "voice": "BM_George" }), "She is calm.", "a").as_str()));
    }

    #[test]
    fn call_diagnostics_keep_only_small_plain_fields() {
        let kept = call_diagnostics(&json!({
            "configured": true,
            "commits": 4,
            "refused": "x".repeat(500),
            "nested": { "a": 1 },
            "list": [1],
            "bad key": 1,
        }))
        .unwrap();
        assert_eq!(kept["configured"], true);
        assert_eq!(kept["commits"], 4);
        assert!(kept["refused"].as_str().unwrap().chars().count() <= 243);
        assert!(kept.get("nested").is_none() && kept.get("list").is_none() && kept.get("bad key").is_none());
        assert!(call_diagnostics(&json!("text")).is_none());
        assert!(call_diagnostics(&json!({ "deep": {} })).is_none());
    }
}
