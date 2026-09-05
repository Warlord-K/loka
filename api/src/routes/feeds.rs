use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use chrono::Utc;
use loka_shared::feed_auth::{Method, Sender, Verdict};
use loka_shared::feed_intake::{self, IntakeChange};
use loka_shared::feeds::{self, AgentFeed, Checked, Intaken, NewFeed};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};
use uuid::Uuid;

use loka_shared::access::Viewer;
use crate::routes::address::client_address;
use crate::AppState;

const WEBHOOK_WINDOW: Duration = Duration::from_secs(60);
const WEBHOOK_PER_WINDOW: u32 = 60;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);
const FAILURES_PER_WINDOW: u32 = 20;

type ApiError = (StatusCode, Json<Value>);

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "feed route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn refuse(status: StatusCode, message: &str) -> ApiError {
    (status, Json(json!({ "error": message })))
}

fn failed(e: anyhow::Error) -> ApiError {
    let text = e.to_string();
    if e.downcast_ref::<sqlx::Error>().is_some() || text.starts_with("environment") {
        error!(error = %text, "feed route failed");
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Try again in a minute.");
    }
    refuse(StatusCode::BAD_REQUEST, &text)
}

fn counted(map: &'static OnceLock<Mutex<HashMap<String, (Instant, u32)>>>, key: &str, window: Duration, add: bool) -> u32 {
    let Ok(mut hits) = map.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return 0;
    };
    if hits.len() > 10_000 {
        hits.retain(|_, (at, _)| at.elapsed() < window);
    }
    let entry = hits.entry(key.to_string()).or_insert((Instant::now(), 0));
    if entry.0.elapsed() >= window {
        *entry = (Instant::now(), 0);
    }
    if add {
        entry.1 += 1;
    }
    entry.1
}

static HITS: OnceLock<Mutex<HashMap<String, (Instant, u32)>>> = OnceLock::new();
static FAILURES: OnceLock<Mutex<HashMap<String, (Instant, u32)>>> = OnceLock::new();

fn over_rate(token: &str) -> bool {
    counted(&HITS, token, WEBHOOK_WINDOW, true) > WEBHOOK_PER_WINDOW
}

fn failing_too_often(ip: &str) -> bool {
    counted(&FAILURES, ip, FAILURE_WINDOW, false) >= FAILURES_PER_WINDOW
}

fn count_failure(ip: &str) {
    counted(&FAILURES, ip, FAILURE_WINDOW, true);
}

pub async fn webhook(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let quiet = |status: StatusCode| (status, Json(json!({ "error": status.canonical_reason().unwrap_or("refused") })));
    let address = client_address(&headers, Some(peer));
    let ip_text = address.ip.clone().unwrap_or_default();
    let sender = Sender { ip: ip_text.parse::<IpAddr>().ok(), certain: address.certain };
    if failing_too_often(&ip_text) {
        return Err(quiet(StatusCode::TOO_MANY_REQUESTS));
    }
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        count_failure(&ip_text);
        return Err(quiet(StatusCode::NOT_FOUND));
    }
    if over_rate(&token) {
        return Err(quiet(StatusCode::TOO_MANY_REQUESTS));
    }
    let Some(feed) = state.world.feed_by_token(&token).await.map_err(|e| quiet(internal(e)))? else {
        count_failure(&ip_text);
        return Err(quiet(StatusCode::NOT_FOUND));
    };
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let checked = match feed.check_request(sender, header, &body, Utc::now()) {
        Verdict::Accepted(method) => {
            state.world.note_feed_verified(feed.id).await.map_err(|e| quiet(internal(e)))?;
            info!(feed = %feed.name, method = method.label(), "Webhook feed call verified");
            if method == Method::Address { Checked::Address } else { Checked::Secret }
        }
        Verdict::Grace => {
            state.world.note_feed_unverified(&feed).await.map_err(|e| quiet(internal(e)))?;
            warn!(feed = %feed.name, agent = %feed.agent_id, "Webhook feed call without the secret accepted during the grace period");
            Checked::Unverified
        }
        refusal @ (Verdict::WrongAddress | Verdict::UnsureAddress | Verdict::Refused) => {
            count_failure(&ip_text);
            if let Err(e) = state.world.note_feed_refusal(&feed, refusal).await {
                error!(error = %e, "could not record a refused webhook call");
            }
            warn!(feed = %feed.name, ip = %ip_text, verdict = ?refusal, "Webhook feed call refused");
            return Err(quiet(if refusal == Verdict::Refused { StatusCode::UNAUTHORIZED } else { StatusCode::FORBIDDEN }));
        }
    };
    let taken = state.world.ingest_webhook(&feed, &header, &body, checked).await.map_err(|e| quiet(internal(e)))?;
    let mut answer = match taken {
        Intaken::Delivered(items) => {
            info!(feed = %feed.name, agent = %feed.agent_id, items, checked = checked.label(), "Webhook feed delivered items");
            json!({ "accepted": items, "paused": feed.paused })
        }
        Intaken::Skipped(why) => json!({ "accepted": 0, "paused": feed.paused, "skipped": why }),
        Intaken::Invalid(why) => {
            warn!(feed = %feed.name, agent = %feed.agent_id, reason = %why, "Webhook feed call turned away by its checks");
            return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": format!("This feed's checks turned the call away: {why}.") }))));
        }
    };
    if checked == Checked::Unverified {
        answer["warning"] = json!("This call had no webhook secret. Calls without it will be refused after the grace period; add the secret from the feed in Loka.");
    }
    Ok((StatusCode::ACCEPTED, Json(answer)))
}

async fn owned(state: &AppState, agent_id: Uuid, feed_id: Uuid) -> Result<AgentFeed, StatusCode> {
    state
        .world
        .get_feed(feed_id)
        .await
        .map_err(internal)?
        .filter(|f| f.agent_id == agent_id)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn list(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Vec<Value>>, StatusCode> {
    Ok(Json(state.world.agent_feeds(id).await.map_err(internal)?.iter().map(AgentFeed::view).collect()))
}

fn with_secret(feed: &AgentFeed, secret: Option<String>) -> Value {
    let url = feed.webhook_url().unwrap_or_default();
    let setup = feed_intake::for_source(&feed.intake().source).sender_setup;
    let Some(secret) = secret else {
        return json!({
            "feed": feed.view(),
            "secret": Value::Null,
            "setup": format!("Point the sender at the URL. Calls are accepted from {} without a secret, and refused from anywhere else.", feed.allowed_ips.join(", ")),
            "examples": { "curl": format!("curl -X POST {url} \\\n  -H 'Content-Type: application/json' \\\n  -d '{{\"title\": \"deploy finished\"}}'") },
        });
    };
    json!({
        "feed": feed.view(),
        "secret": secret,
        "setup": setup,
        "examples": {
            "curl": format!("curl -X POST {url} \\\n  -H 'Authorization: Bearer {secret}' \\\n  -H 'Content-Type: application/json' \\\n  -d '{{\"title\": \"deploy finished\"}}'"),
            "alertmanager": format!("receivers:\n  - name: loka\n    webhook_configs:\n      - url: {url}\n        send_resolved: true\n        http_config:\n          authorization:\n            type: Bearer\n            credentials: {secret}"),
            "github": format!("Payload URL: {url}\nContent type: application/json\nSecret: {secret}"),
        },
    })
}

#[derive(Deserialize)]
pub struct CreateFeed {
    name: Option<String>,
    format: Option<String>,
    filter: Option<String>,
    #[serde(default)]
    allowed_ips: Vec<String>,
    auth: Option<String>,
    #[serde(flatten)]
    intake: IntakeChange,
}

fn by_address(auth: Option<&str>) -> Result<Option<bool>, ApiError> {
    match auth.map(str::trim).map(str::to_lowercase).as_deref() {
        None | Some("") => Ok(None),
        Some("secret") => Ok(Some(false)),
        Some("address") => Ok(Some(true)),
        Some(_) => Err(refuse(StatusCode::BAD_REQUEST, "A webhook checks its sender by secret or by address.")),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateFeed>,
) -> Result<Json<Value>, ApiError> {
    let agent = state
        .world
        .get_agent(id)
        .await
        .map_err(failed)?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, "That agent is gone."))?;
    let source = body.intake.source.as_deref().map(feed_intake::clean_source).filter(|s| !s.is_empty()).ok_or_else(|| {
        refuse(StatusCode::BAD_REQUEST, "Say what will send to this webhook, like GitHub, Alertmanager or the name of your own tool.")
    })?;
    let preset = feed_intake::for_source(&source);
    let intake = body.intake.onto(&preset.intake).map_err(failed)?;
    let format = body.format.as_deref().map(str::trim).filter(|f| !f.is_empty() && *f != "auto").unwrap_or(preset.format).to_lowercase();
    let name = body.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&intake.source).to_string();
    let by_address = by_address(body.auth.as_deref())?.unwrap_or(false);
    let feed = state
        .world
        .create_feed(NewFeed {
            agent_id: agent.id,
            owner_user_id: agent.owner_user_id.or(Some(viewer.user)),
            created_by_agent: None,
            kind: "webhook",
            name: &name,
            url: None,
            format: &format,
            filter: body.filter.as_deref(),
            poll_secs: feeds::DEFAULT_POLL_SECS,
            seen: Vec::new(),
            primed: true,
            allowed_ips: body.allowed_ips,
            by_address,
            intake,
        })
        .await
        .map_err(failed)?;
    if by_address {
        return Ok(Json(with_secret(&feed, None)));
    }
    let secret = state
        .world
        .reveal_feed_secret(feed.id)
        .await
        .map_err(failed)?
        .ok_or_else(|| refuse(StatusCode::INTERNAL_SERVER_ERROR, "The feed was made but its secret could not be shown. Rotate it."))?;
    let fresh = state.world.get_feed(feed.id).await.map_err(failed)?.unwrap_or(feed);
    Ok(Json(with_secret(&fresh, Some(secret))))
}

pub async fn reveal(State(state): State<AppState>, Path((id, feed_id)): Path<(Uuid, Uuid)>) -> Result<Json<Value>, ApiError> {
    owned(&state, id, feed_id).await.map_err(|s| refuse(s, "That feed is gone."))?;
    let secret = state
        .world
        .reveal_feed_secret(feed_id)
        .await
        .map_err(failed)?
        .ok_or_else(|| refuse(StatusCode::CONFLICT, "This secret was already shown once. Rotate it to get a new one."))?;
    let feed = owned(&state, id, feed_id).await.map_err(|s| refuse(s, "That feed is gone."))?;
    Ok(Json(with_secret(&feed, Some(secret))))
}

pub async fn rotate(State(state): State<AppState>, Path((id, feed_id)): Path<(Uuid, Uuid)>) -> Result<Json<Value>, ApiError> {
    owned(&state, id, feed_id).await.map_err(|s| refuse(s, "That feed is gone."))?;
    let secret = state
        .world
        .rotate_feed_secret(feed_id, true)
        .await
        .map_err(failed)?
        .ok_or_else(|| refuse(StatusCode::INTERNAL_SERVER_ERROR, "Could not rotate the secret."))?;
    let feed = owned(&state, id, feed_id).await.map_err(|s| refuse(s, "That feed is gone."))?;
    Ok(Json(with_secret(&feed, Some(secret))))
}

#[derive(Deserialize)]
pub struct UpdateFeed {
    paused: Option<bool>,
    allowed_ips: Option<Vec<String>>,
    auth: Option<String>,
    format: Option<String>,
    #[serde(flatten)]
    intake: IntakeChange,
}

pub async fn update(
    State(state): State<AppState>,
    Path((id, feed_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateFeed>,
) -> Result<Json<Value>, ApiError> {
    let gone = || refuse(StatusCode::NOT_FOUND, "That feed is gone.");
    let mut feed = owned(&state, id, feed_id).await.map_err(|s| refuse(s, "That feed is gone."))?;
    let by_address = by_address(body.auth.as_deref())?;
    let shapes = by_address.is_some() || body.allowed_ips.is_some() || body.format.is_some() || !body.intake.is_empty();
    if shapes && feed.kind != "webhook" {
        return Err(refuse(StatusCode::BAD_REQUEST, "Only webhook feeds check their sender and their calls."));
    }
    if by_address == Some(false) {
        feed = state.world.set_feed_by_address(feed_id, false).await.map_err(failed)?.ok_or_else(gone)?;
    }
    if let Some(ranges) = body.allowed_ips {
        feed = state.world.set_feed_ip_ranges(feed_id, &ranges).await.map_err(failed)?.ok_or_else(gone)?;
    }
    if by_address == Some(true) {
        feed = state.world.set_feed_by_address(feed_id, true).await.map_err(failed)?.ok_or_else(gone)?;
    }
    if !body.intake.is_empty() || body.format.is_some() {
        let renamed = body.intake.source.as_deref().map(feed_intake::clean_source).filter(|s| !s.eq_ignore_ascii_case(&feed.intake().source));
        let preset_format = renamed.as_deref().map(|source| feed_intake::for_source(source).format).filter(|f| *f != "auto");
        let format = body.format.as_deref().map(str::trim).filter(|f| !f.is_empty()).map(str::to_lowercase).or(preset_format.map(String::from));
        let intake = body.intake.onto(&feed.intake()).map_err(failed)?;
        feed = state.world.set_feed_intake(feed_id, &intake, format.as_deref()).await.map_err(failed)?.ok_or_else(gone)?;
    }
    if let Some(paused) = body.paused {
        feed = state.world.set_feed_paused(feed_id, paused).await.map_err(failed)?.ok_or_else(gone)?;
    }
    Ok(Json(feed.view()))
}

pub async fn remove(State(state): State<AppState>, Path((id, feed_id)): Path<(Uuid, Uuid)>) -> Result<StatusCode, StatusCode> {
    owned(&state, id, feed_id).await?;
    state.world.delete_feed(feed_id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_block_an_address_for_a_while() {
        let ip = format!("198.51.100.{}", std::process::id() % 250);
        for _ in 0..FAILURES_PER_WINDOW - 1 {
            count_failure(&ip);
        }
        assert!(!failing_too_often(&ip));
        count_failure(&ip);
        assert!(failing_too_often(&ip));
        assert!(!failing_too_often("203.0.113.250"));
    }
}
