use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use loka_shared::access::Role;
use loka_shared::teams::{NewTeam, Team, TeamDetail, TeamEdit, TeamMember};
use serde::Deserialize;
use tracing::error;
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

fn internal(e: anyhow::Error) -> (StatusCode, String) {
    error!(error = %e, "team route failed");
    (StatusCode::INTERNAL_SERVER_ERROR, String::new())
}

fn invalid(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn status(code: StatusCode) -> (StatusCode, String) {
    (code, String::new())
}

pub async fn owned_team(state: &AppState, user: Uuid, role: Role, id: Uuid) -> Result<Team, (StatusCode, String)> {
    let team = state.world.get_team(id).await.map_err(internal)?.ok_or(status(StatusCode::NOT_FOUND))?;
    let viewer = crate::access::viewer_for(state, user).await.map_err(status)?;
    let channel = match team.channel_id {
        Some(channel) => state.world.get_channel(channel).await.map_err(internal)?,
        None => None,
    };
    if !channel.is_some_and(|c| viewer.in_org(c.org_id)) {
        return Err(status(StatusCode::NOT_FOUND));
    }
    if team.owner_user_id != user && role != Role::Admin {
        return Err(status(StatusCode::NOT_FOUND));
    }
    Ok(team)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Vec<TeamDetail>>, (StatusCode, String)> {
    let teams = state.world.teams_visible_to(user.0, false).await.map_err(internal)?;
    let mut out = Vec::with_capacity(teams.len());
    for team in teams {
        out.push(state.world.team_detail(team).await.map_err(internal)?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateBody {
    name: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    rules: String,
    lead_agent_id: Option<Uuid>,
    #[serde(default)]
    agent_ids: Vec<Uuid>,
    #[serde(default)]
    user_ids: Vec<Uuid>,
    channel: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<loka_shared::orgs::Viewer>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<TeamDetail>), (StatusCode, String)> {
    let team = state
        .world
        .create_team(
            user.0,
            NewTeam {
                name: body.name,
                purpose: body.purpose,
                rules: body.rules,
                lead: body.lead_agent_id,
                agents: body.agent_ids,
                people: body.user_ids,
                channel: body.channel.filter(|c| !c.trim().is_empty()),
                created_by_agent: None,
                org: viewer.org,
            },
        )
        .await
        .map_err(invalid)?;
    Ok((StatusCode::CREATED, Json(state.world.team_detail(team).await.map_err(internal)?)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(edit): Json<TeamEdit>,
) -> Result<Json<TeamDetail>, (StatusCode, String)> {
    owned_team(&state, user.0, role, id).await?;
    let team = state.world.update_team(id, &edit).await.map_err(invalid)?;
    Ok(Json(state.world.team_detail(team).await.map_err(internal)?))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    owned_team(&state, user.0, role, id).await?;
    state.world.delete_team(id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct MembersBody {
    #[serde(default)]
    agent_ids: Vec<Uuid>,
    #[serde(default)]
    user_ids: Vec<Uuid>,
}

pub async fn add_members(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path(id): Path<Uuid>,
    Json(body): Json<MembersBody>,
) -> Result<Json<Vec<TeamMember>>, (StatusCode, String)> {
    owned_team(&state, user.0, role, id).await?;
    Ok(Json(state.world.add_team_members(id, &body.agent_ids, &body.user_ids).await.map_err(invalid)?))
}

pub async fn remove_member(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
    Path((id, kind, member)): Path<(Uuid, String, Uuid)>,
) -> Result<Json<Vec<TeamMember>>, (StatusCode, String)> {
    owned_team(&state, user.0, role, id).await?;
    let (agents, people) = match kind.as_str() {
        "agent" => (vec![member], Vec::new()),
        "user" => (Vec::new(), vec![member]),
        _ => return Err(status(StatusCode::BAD_REQUEST)),
    };
    Ok(Json(state.world.remove_team_members(id, &agents, &people).await.map_err(internal)?))
}
