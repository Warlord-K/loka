use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use loka_shared::access::Role;
use loka_shared::academy::{AcademyError, AdventureCard, Attempt, ChallengeCard, ChallengeDetail, Progress};
use loka_shared::loka::{NpcLine, Region, RegionDetail, TalkError};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> StatusCode {
    error!(error = %e, "loka route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn fail(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": code, "message": message.into() }))).into_response()
}

fn academy_error(e: AcademyError) -> Response {
    match e {
        AcademyError::NotFound => fail(StatusCode::NOT_FOUND, "not_found", "That challenge is not yours or no longer exists."),
        AcademyError::Busy => fail(StatusCode::CONFLICT, "pending", "Your last answer is still being graded."),
        AcademyError::NoQuest => fail(StatusCode::CONFLICT, "no_quest", "None of your agents is working on a quest right now."),
        AcademyError::Limited(m) => fail(StatusCode::TOO_MANY_REQUESTS, "rate_limited", m),
        AcademyError::Blocked(m) => fail(StatusCode::CONFLICT, "blocked", m),
        AcademyError::Invalid(m) => fail(StatusCode::BAD_REQUEST, "invalid", m),
        AcademyError::Off => fail(StatusCode::CONFLICT, "academy_off", "The Academy is turned off. Turn it on under Progress to get challenges while you wait."),
        AcademyError::Other(e) => internal(e).into_response(),
    }
}

pub async fn list_regions(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
) -> Result<Json<Vec<Region>>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    Ok(Json(state.world.regions_for(user.0, org, role == Role::Admin).await.map_err(internal)?))
}

pub async fn get_region(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<Json<RegionDetail>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    state.world.region_for(user.0, org, role == Role::Admin, id).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub async fn presets() -> Json<Value> {
    Json(loka_shared::loka::presets())
}

pub async fn npc_history(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<NpcLine>>, StatusCode> {
    let npc = state.world.get_item(id).await.map_err(internal)?.filter(|i| i.kind == "npc").ok_or(StatusCode::NOT_FOUND)?;
    if npc.owner_user_id != Some(user.0) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(state.world.npc_history(user.0, id).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct TalkBody {
    text: String,
}

pub async fn talk_to_npc(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<TalkBody>,
) -> Response {
    state.world.talk_to_npc(user.0, id, &body.text).await.map(Json).map_err(|e| match e {
        TalkError::NotFound => fail(StatusCode::NOT_FOUND, "not_found", "That character is not in your Loka."),
        TalkError::Busy => fail(StatusCode::CONFLICT, "busy", "They are still answering you."),
        TalkError::Blocked(m) => fail(StatusCode::CONFLICT, "blocked", m),
        TalkError::Invalid(m) => fail(StatusCode::BAD_REQUEST, "invalid", m),
        TalkError::Other(e) => internal(e).into_response(),
    })
    .into_response()
}

pub async fn list_challenges(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Vec<ChallengeCard>>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    Ok(Json(state.world.challenge_cards(user.0, org).await.map_err(internal)?))
}

pub async fn get_challenge(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChallengeDetail>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    state.world.challenge_detail(user.0, org, id).await.map_err(internal)?.map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub async fn list_adventures(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Vec<AdventureCard>>, StatusCode> {
    let org = crate::access::viewer_for(&state, user.0).await?.org;
    Ok(Json(state.world.adventures_for(user.0, org).await.map_err(internal)?))
}

#[derive(Deserialize)]
pub struct AnswerBody {
    answer: String,
}

pub async fn submit_attempt(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<AnswerBody>,
) -> Response {
    state.world.submit_attempt(user.0, id, &body.answer).await.map(Json).map_err(academy_error).into_response()
}

pub async fn get_attempt(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Attempt>, StatusCode> {
    state.world.get_attempt(id).await.map_err(internal)?.filter(|a| a.user_id == user.0).map(Json).ok_or(StatusCode::NOT_FOUND)
}

pub async fn progress(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Progress>, StatusCode> {
    Ok(Json(state.world.progress(user.0).await.map_err(internal)?))
}

#[derive(Deserialize, Default)]
pub struct OfferBody {
    task_id: Option<Uuid>,
}

pub async fn offer(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    body: Option<Json<OfferBody>>,
) -> Response {
    let task = body.and_then(|b| b.0.task_id);
    state.world.request_offer(user.0, task).await.map(Json).map_err(academy_error).into_response()
}

#[derive(Deserialize)]
pub struct AcademyBody {
    on: bool,
}

pub async fn set_academy(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<AcademyBody>,
) -> Result<Json<Progress>, StatusCode> {
    state.world.set_academy_on(user.0, body.on).await.map_err(internal)?;
    Ok(Json(state.world.progress(user.0).await.map_err(internal)?))
}
