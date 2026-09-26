use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::Role;
use loka_shared::reports::{self, NewReport, Report, ReportActor, ReportBrief, ReportFilter, ReportPatch, Revision, RevisionBrief};
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "report route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn invalid(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn status(code: StatusCode) -> (StatusCode, String) {
    (code, String::new())
}

async fn org_of(state: &AppState, user: Uuid) -> Result<Uuid, StatusCode> {
    crate::access::viewer_for(state, user).await?.org.ok_or(StatusCode::FORBIDDEN)
}

async fn readable(state: &AppState, user: Uuid, role: Role, id: Uuid) -> Result<Report, StatusCode> {
    let org = org_of(state, user).await?;
    let report = state.world.get_report(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if !reports::can_read(&report, user, role, org) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(report)
}

async fn editable(state: &AppState, user: Uuid, role: Role, id: Uuid) -> Result<Report, StatusCode> {
    let report = readable(state, user, role, id).await?;
    if role < Role::Write || !reports::can_edit(&report, user, role, report.org_id.unwrap_or_default()) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(report)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(filter): Query<ReportFilter>,
) -> Result<Json<Vec<ReportBrief>>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    state.world.list_reports(user.0, org, &filter).await.map(Json).map_err(invalid)
}

#[derive(Deserialize)]
pub struct FindQuery {
    key: String,
}

pub async fn find(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<FindQuery>,
) -> Result<Json<ReportBrief>, StatusCode> {
    let org = org_of(&state, user.0).await?;
    state.world.find_report_brief(user.0, org, &q.key).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
pub struct CreateReport {
    title: String,
    #[serde(default)]
    body: String,
    folder: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    visibility: Option<String>,
    #[serde(default)]
    pinned: bool,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<CreateReport>,
) -> Result<Json<Report>, (StatusCode, String)> {
    state
        .world
        .create_report(NewReport {
            owner_user_id: user.0,
            by: ReportActor::User(user.0),
            title: body.title,
            body: body.body,
            folder: body.folder,
            tags: body.tags,
            visibility: body.visibility,
            pinned: body.pinned,
            task_id: None,
        })
        .await
        .map(Json)
        .map_err(invalid)
}

#[derive(Serialize)]
pub struct ReportDetail {
    report: Report,
    backlinks: Vec<ReportBrief>,
    outlinks: Vec<ReportBrief>,
    can_edit: bool,
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<ReportDetail>, StatusCode> {
    let report = readable(&state, user.0, role, id).await?;
    let org = report.org_id.unwrap_or_default();
    let backlinks = state.world.report_backlinks(user.0, org, id).await.map_err(internal)?;
    let outlinks = state.world.report_outlinks(user.0, org, &report).await.map_err(internal)?;
    let can_edit = role >= Role::Write && reports::can_edit(&report, user.0, role, org);
    Ok(Json(ReportDetail { report, backlinks, outlinks, can_edit }))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(patch): Json<ReportPatch>,
) -> Result<Json<Report>, (StatusCode, String)> {
    editable(&state, user.0, role, id).await.map_err(status)?;
    state
        .world
        .update_report(id, ReportActor::User(user.0), &patch)
        .await
        .map_err(invalid)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, String::new()))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    editable(&state, user.0, role, id).await?;
    state.world.delete_report(id, ReportActor::User(user.0)).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revisions(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RevisionBrief>>, StatusCode> {
    readable(&state, user.0, role, id).await?;
    state.world.report_revisions(id).await.map(Json).map_err(internal)
}

pub async fn revision(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, revision)): Path<(Uuid, i32)>,
) -> Result<Json<Revision>, StatusCode> {
    readable(&state, user.0, role, id).await?;
    state.world.report_revision(id, revision).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub async fn restore(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, revision)): Path<(Uuid, i32)>,
) -> Result<Json<Report>, (StatusCode, String)> {
    editable(&state, user.0, role, id).await.map_err(status)?;
    state
        .world
        .restore_report_revision(id, revision, ReportActor::User(user.0))
        .await
        .map_err(invalid)?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, String::new()))
}
