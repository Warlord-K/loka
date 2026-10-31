use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::access::Viewer;
use loka_shared::cleanup::{CleanupKind, CleanupRequest, CleanupResult, StorageReport};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

type ApiError = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, message: &str) -> ApiError {
    (status, Json(json!({ "error": message })))
}

pub async fn report(State(state): State<AppState>, Extension(viewer): Extension<Viewer>) -> Result<Json<StorageReport>, ApiError> {
    state.world.storage_report(viewer.user, viewer.org).await.map(Json).map_err(|error| {
        tracing::error!(%error, "storage report failed");
        refuse(StatusCode::INTERNAL_SERVER_ERROR, "Could not measure your storage. Try again in a minute.")
    })
}

#[derive(Deserialize)]
pub struct CleanupBody {
    kind: String,
    older_than_days: i32,
    agent_id: Option<Uuid>,
    #[serde(default)]
    apply: bool,
}

pub async fn cleanup(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Json(body): Json<CleanupBody>,
) -> Result<Json<CleanupResult>, ApiError> {
    let kind = CleanupKind::parse(&body.kind).ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "Unknown cleanup."))?;
    let request = CleanupRequest {
        owner: viewer.user,
        org: viewer.org,
        agent: body.agent_id,
        kind,
        older_than_days: body.older_than_days,
        by_agent: None,
    };
    state.world.clean_up(request, body.apply).await.map(Json).map_err(|error| {
        let text = error.to_string();
        if text.contains("older_than_days") || text.contains("not yours") {
            refuse(StatusCode::BAD_REQUEST, &text)
        } else {
            tracing::error!(%error, "cleanup failed");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "Could not clean up. Nothing was removed.")
        }
    })
}
