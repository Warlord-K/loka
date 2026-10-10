use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::access::{Role, Viewer};
use loka_shared::starter_team::{Built, Plan, StarterOptions};
use serde::Serialize;
use tracing::error;

use crate::auth::UserId;
use crate::routes::world::ensure_assistant;
use crate::AppState;

fn internal(e: anyhow::Error) -> (StatusCode, String) {
    error!(error = %e, "starter team route failed");
    (StatusCode::INTERNAL_SERVER_ERROR, String::new())
}

pub async fn plan(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
) -> Result<Json<Plan>, (StatusCode, String)> {
    Ok(Json(state.world.starter_plan(user.0, viewer.org).await.map_err(internal)?))
}

#[derive(Serialize)]
pub struct BuiltView {
    #[serde(flatten)]
    pub(crate) built: Built,
    summary: String,
}

pub async fn build(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Extension(viewer): Extension<Viewer>,
    body: Option<Json<StarterOptions>>,
) -> Result<Json<BuiltView>, (StatusCode, String)> {
    if role < Role::Write {
        return Err((StatusCode::FORBIDDEN, String::new()));
    }
    let options = body.map(|Json(b)| b).unwrap_or_default();
    Ok(Json(run(&state, &user, viewer.org, &options).await?))
}

pub async fn run(state: &AppState, user: &UserId, org: Option<uuid::Uuid>, options: &StarterOptions) -> Result<BuiltView, (StatusCode, String)> {
    if state.world.main_agent_for(user.0).await.map_err(internal)?.is_none() {
        let _ = ensure_assistant(State(state.clone()), Extension(user.clone()), None).await.map_err(|code| (code, String::new()))?;
    }
    let built = state.world.build_starter_team(user.0, org, options).await.map_err(internal)?;
    let summary = built.summary();
    Ok(BuiltView { built, summary })
}
