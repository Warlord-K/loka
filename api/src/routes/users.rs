use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::access::Viewer;
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;

#[derive(Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub async fn list(State(state): State<AppState>, Extension(viewer): Extension<Viewer>) -> Result<Json<Vec<UserSummary>>, StatusCode> {
    let rows = state
        .world
        .people_in(viewer.org())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let admin = viewer.role == loka_shared::access::Role::Admin;
    let users = rows
        .into_iter()
        .map(|p| UserSummary { id: p.id, name: p.name, email: if admin || p.id == viewer.user { p.email } else { None } })
        .collect();

    Ok(Json(users))
}
