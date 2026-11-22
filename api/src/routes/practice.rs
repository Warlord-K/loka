use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::Role;
use loka_shared::practice::{Attempt, AttemptCard, CoachOption, Practice, PracticeBrief, Ready, Refusal};
use loka_shared::reports::{self, Report, ReportActor};
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

pub const MAX_READY_BYTES: usize = 3 * 1024 * 1024 - 64 * 1024;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "practice route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn invalid(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn status(code: StatusCode) -> (StatusCode, String) {
    (code, String::new())
}

fn refused(e: anyhow::Error) -> (StatusCode, String) {
    let code = match e.downcast_ref::<Refusal>() {
        Some(Refusal::NotWaiting | Refusal::Ambiguous | Refusal::NoImage) => StatusCode::CONFLICT,
        Some(Refusal::TooSoon) => StatusCode::TOO_MANY_REQUESTS,
        Some(Refusal::Missing) => StatusCode::NOT_FOUND,
        None => StatusCode::BAD_REQUEST,
    };
    (code, e.to_string())
}

async fn org_of(state: &AppState, user: Uuid) -> Result<Uuid, StatusCode> {
    crate::access::viewer_for(state, user).await?.org.ok_or(StatusCode::FORBIDDEN)
}

async fn own_practice(state: &AppState, user: Uuid, id: Uuid) -> Result<(Report, Practice), StatusCode> {
    let org = org_of(state, user).await?;
    let report = state.world.get_report(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    if report.owner_user_id != user || report.org_id != Some(org) {
        return Err(StatusCode::NOT_FOUND);
    }
    let practice = Practice::of(&report).ok_or(StatusCode::NOT_FOUND)?;
    Ok((report, practice))
}

#[derive(Serialize)]
pub struct PracticeView {
    report: Report,
    practice: Practice,
}

#[derive(Serialize)]
pub struct PracticeDetail {
    report: Report,
    practice: Practice,
    attempts: Vec<Attempt>,
}

#[derive(Deserialize)]
pub struct CreatePractice {
    subject: String,
    title: Option<String>,
    coach: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<CreatePractice>,
) -> Result<Json<PracticeView>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    let coach = match body.coach.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(key) => Some(state.world.find_coach(user.0, org, key).await.map_err(invalid)?),
        None => None,
    };
    let report = state
        .world
        .create_practice(user.0, org, ReportActor::User(user.0), &body.subject, body.title.as_deref(), coach, None)
        .await
        .map_err(invalid)?;
    let practice = Practice::of(&report).ok_or_else(|| status(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(PracticeView { report, practice }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    subject: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<PracticeBrief>>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    state.world.list_practice(user.0, org, q.subject.as_deref(), q.query.as_deref(), q.limit.unwrap_or(100)).await.map(Json).map_err(invalid)
}

async fn readable_practice(state: &AppState, user: Uuid, role: Role, report: Report) -> Result<(Report, Practice), StatusCode> {
    let org = org_of(state, user).await?;
    if !reports::can_read(&report, user, role, org) {
        return Err(StatusCode::FORBIDDEN);
    }
    let practice = Practice::of(&report).ok_or(StatusCode::NOT_FOUND)?;
    Ok((report, practice))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<PracticeDetail>, StatusCode> {
    let report = state.world.get_report(id).await.map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let (report, practice) = readable_practice(&state, user.0, role, report).await?;
    let attempts = state.world.practice_attempts(id, 200, false).await.map_err(internal)?;
    Ok(Json(PracticeDetail { report, practice, attempts }))
}

#[derive(Deserialize)]
pub struct PatchPractice {
    coach_agent_id: Option<Uuid>,
    subject: Option<String>,
    title: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchPractice>,
) -> Result<Json<PracticeView>, (StatusCode, String)> {
    let (report, _) = own_practice(&state, user.0, id).await.map_err(status)?;
    let report = state
        .world
        .update_practice(&report, ReportActor::User(user.0), body.subject.as_deref(), body.coach_agent_id, body.title.as_deref())
        .await
        .map_err(invalid)?;
    let practice = Practice::of(&report).ok_or_else(|| status(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(PracticeView { report, practice }))
}

#[derive(Deserialize)]
pub struct CoachQuery {
    subject: Option<String>,
}

#[derive(Serialize)]
pub struct Coaches {
    agents: Vec<CoachOption>,
    subject: Option<String>,
    default_coach_agent_id: Option<Uuid>,
}

pub async fn coaches(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<CoachQuery>,
) -> Result<Json<Coaches>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    let subject = q.subject.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(loka_shared::practice::clean_subject).transpose().map_err(invalid)?;
    let default_coach_agent_id = match subject.as_deref() {
        Some(s) => state.world.default_coach(user.0, org, s).await.map_err(invalid)?,
        None => None,
    };
    let agents = state.world.practice_coaches(user.0, org).await.map_err(invalid)?;
    Ok(Json(Coaches { agents, subject, default_coach_agent_id }))
}

pub async fn ready(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<Ready>,
) -> Result<Json<Attempt>, (StatusCode, String)> {
    let (report, _) = own_practice(&state, user.0, id).await.map_err(status)?;
    state.world.practice_ready(&report, user.0, body).await.map(Json).map_err(invalid)
}

pub async fn attempt(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(key): Path<String>,
) -> Result<Json<AttemptCard>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    let id = state.world.resolve_attempt(user.0, role, org, &key).await.map_err(refused)?;
    let (card, report) = state.world.attempt_card(id).await.map_err(|e| status(internal(e)))?.ok_or_else(|| status(StatusCode::NOT_FOUND))?;
    readable_practice(&state, user.0, role, report).await.map_err(status)?;
    Ok(Json(card))
}

pub async fn resend(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(key): Path<String>,
) -> Result<Json<Attempt>, (StatusCode, String)> {
    let org = org_of(&state, user.0).await.map_err(status)?;
    let id = state.world.resolve_attempt(user.0, role, org, &key).await.map_err(refused)?;
    let (card, _) = state.world.attempt_card(id).await.map_err(|e| status(internal(e)))?.ok_or_else(|| status(StatusCode::NOT_FOUND))?;
    let (report, _) = own_practice(&state, user.0, card.report_id).await.map_err(|code| status(if code == StatusCode::NOT_FOUND { StatusCode::FORBIDDEN } else { code }))?;
    state.world.resend_practice(&report, id).await.map(Json).map_err(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn practice_routes_need_write_to_change_and_view_to_read() {
        use crate::access::min_role;
        use loka_shared::access::Role;
        assert_eq!(min_role(&Method::GET, "/api/world/practice"), Role::View);
        assert_eq!(min_role(&Method::GET, "/api/world/practice/attempts/abc"), Role::View);
        assert_eq!(min_role(&Method::POST, "/api/world/practice"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/api/world/practice/abc/ready"), Role::Write);
        assert_eq!(min_role(&Method::PATCH, "/api/world/practice/abc"), Role::Write);
        assert_eq!(min_role(&Method::POST, "/api/world/practice/attempts/abc/resend"), Role::Write);
    }

    #[test]
    fn refusals_map_to_conflict_rate_limit_and_not_found() {
        let code = |r: Refusal| refused(r.into()).0;
        assert_eq!(code(Refusal::NotWaiting), StatusCode::CONFLICT);
        assert_eq!(code(Refusal::Ambiguous), StatusCode::CONFLICT);
        assert_eq!(code(Refusal::TooSoon), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(code(Refusal::Missing), StatusCode::NOT_FOUND);
        assert_eq!(code(Refusal::NoImage), StatusCode::CONFLICT);
        assert_eq!(refused(anyhow::anyhow!("bad strokes")).0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn ready_bodies_fit_under_the_edge_and_above_the_stroke_cap() {
        const {
            assert!(MAX_READY_BYTES < 3 * 1024 * 1024);
            assert!(MAX_READY_BYTES > loka_shared::practice::MAX_STROKES_BYTES + 256 * 1024);
        }
    }

    #[test]
    fn ready_bodies_parse_with_or_without_a_note() {
        let id = Uuid::new_v4();
        let ready: Ready = serde_json::from_value(serde_json::json!({ "image_upload_id": id, "strokes": { "width": 1, "height": 1, "strokes": [] } })).unwrap();
        assert_eq!((ready.image_upload_id, ready.note), (id, None));
        assert!(serde_json::from_value::<Ready>(serde_json::json!({ "strokes": {} })).is_err());
    }
}
