use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::watches::Watch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "github route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

#[derive(Serialize)]
pub struct GithubDevice {
    id: Uuid,
    name: String,
    online: bool,
    available: bool,
    login: Option<String>,
    problem: Option<String>,
    checked_at: Option<String>,
}

#[derive(Serialize)]
pub struct GithubSettings {
    enabled: bool,
    primed_at: Option<chrono::DateTime<chrono::Utc>>,
    last_item_at: Option<chrono::DateTime<chrono::Utc>>,
    devices: Vec<GithubDevice>,
}

async fn settings(state: &AppState, user: Uuid) -> Result<GithubSettings, StatusCode> {
    let feed = state.world.github_feed_settings(user).await.map_err(internal)?;
    let devices = state
        .runners
        .list(user)
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|r| r.platform == "macos")
        .map(|r| {
            let cap = r.capabilities.get("github_feed").cloned().unwrap_or(Value::Null);
            let text = |key: &str| cap.get(key).and_then(Value::as_str).map(str::to_string);
            GithubDevice {
                id: r.id,
                online: r.status == "online",
                available: cap.get("available").and_then(Value::as_bool).unwrap_or(false),
                login: text("login"),
                problem: text("problem"),
                checked_at: text("checked_at"),
                name: r.name,
            }
        })
        .collect();
    Ok(GithubSettings { enabled: feed.enabled, primed_at: feed.primed_at, last_item_at: feed.last_item_at, devices })
}

pub async fn get(State(state): State<AppState>, Extension(user): Extension<UserId>) -> Result<Json<GithubSettings>, StatusCode> {
    Ok(Json(settings(&state, user.0).await?))
}

#[derive(Deserialize)]
pub struct SetGithub {
    enabled: bool,
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<SetGithub>,
) -> Result<Json<GithubSettings>, StatusCode> {
    state.world.set_github_feed(user.0, body.enabled).await.map_err(internal)?;
    Ok(Json(settings(&state, user.0).await?))
}

pub async fn watches(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Vec<Watch>>, StatusCode> {
    Ok(Json(state.world.watches(id).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct NewWatch {
    #[serde(default = "github_prefix")]
    event_type: String,
    #[serde(rename = "match")]
    matcher: Value,
    days: Option<i64>,
    note: Option<String>,
}

fn github_prefix() -> String {
    loka_shared::github_feed::PREFIX.to_string()
}

pub async fn add_watch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<NewWatch>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let refuse = |message: String| Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message }))));
    if loka_shared::world::normalize_event_type(&body.event_type).is_none() {
        return refuse(format!("{:?} is not an event type", body.event_type));
    }
    let matcher = match loka_shared::watches::check_match(&body.matcher) {
        Ok(matcher) => matcher,
        Err(e) => return refuse(e.to_string()),
    };
    let watch = match state.world.add_watch(id, &body.event_type, matcher, body.days, body.note.as_deref()).await {
        Ok(watch) => watch,
        Err(e) if e.to_string().starts_with("at most") => return refuse(e.to_string()),
        Err(e) => return Err(internal(e)),
    };
    Ok((StatusCode::CREATED, Json(serde_json::json!(watch))))
}

pub async fn remove_watch(State(state): State<AppState>, Path((id, watch)): Path<(Uuid, Uuid)>) -> Result<StatusCode, StatusCode> {
    if state.world.remove_watch(id, watch).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
