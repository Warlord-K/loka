use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::readiness::{self, Readiness};

use crate::auth::UserId;
use crate::AppState;

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Readiness>, StatusCode> {
    let mut signals = readiness::gather(state.db.pool(), user.0).await.map_err(|error| {
        tracing::error!(%error, "readiness failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    signals.fal_key = crate::routes::voice::fal_key_for(&state, user.0).await.is_some();
    Ok(Json(readiness::evaluate(&signals)))
}
