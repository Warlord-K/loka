use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::{self, Role};
use loka_shared::api_tokens::{ApiToken, CreatedToken, Scope, DEFAULT_DAYS};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserId;
use crate::routes::world::internal;
use crate::AppState;

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    scope: String,
    #[serde(default)]
    agent_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    expires_in_days: Option<i64>,
}

fn refuse(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

pub async fn list(State(state): State<AppState>, Extension(user): Extension<UserId>) -> Result<Json<Vec<ApiToken>>, StatusCode> {
    Ok(Json(state.world.list_api_tokens(user.0).await.map_err(internal)?))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Json(body): Json<NewToken>,
) -> Result<(StatusCode, Json<CreatedToken>), (StatusCode, Json<Value>)> {
    let scope = Scope::parse(&body.scope).ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "scope must be read or write"))?;
    if scope == Scope::Write && role < Role::Write {
        return Err(refuse(StatusCode::FORBIDDEN, "your access level only allows read tokens"));
    }
    let viewer = crate::access::viewer_for(&state, user.0).await.map_err(|s| refuse(s, "lookup failed"))?;
    if let Some(ids) = &body.agent_ids {
        for id in ids {
            let agent = state.world.get_agent(*id).await.map_err(|e| refuse(internal(e), "lookup failed"))?;
            if !agent.is_some_and(|a| access::can_see(&viewer, &a)) {
                return Err(refuse(StatusCode::BAD_REQUEST, "one of those agents does not exist or is hidden from you"));
            }
        }
    }
    let created = state
        .world
        .create_api_token(user.0, &body.name, scope, body.agent_ids, body.expires_in_days.unwrap_or(DEFAULT_DAYS))
        .await
        .map_err(|e| refuse(StatusCode::BAD_REQUEST, &e.to_string()))?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn revoke(State(state): State<AppState>, Extension(user): Extension<UserId>, Path(id): Path<Uuid>) -> Result<StatusCode, StatusCode> {
    if state.world.revoke_api_token(user.0, id).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
