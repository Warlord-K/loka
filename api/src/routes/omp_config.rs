use std::collections::BTreeSet;

use axum::{
    extract::{Path, State},
    http::{header::CACHE_CONTROL, HeaderName, StatusCode},
    Extension, Json,
};
use loka_shared::environment::{self, InvalidEnvironmentGrant};
use loka_shared::omp_config::{self, OmpModel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::error;
use uuid::Uuid;

use crate::{auth::UserId, AppState};

type Failure = (StatusCode, Json<Value>);

#[derive(Serialize)]
pub struct OmpSecret {
    name: String,
    in_environment: bool,
    granted: bool,
}

#[derive(Serialize)]
pub struct OmpConfigResponse {
    config: Option<String>,
    masked: bool,
    secrets: Vec<OmpSecret>,
    models: Vec<OmpModel>,
    private: bool,
    can_equip: bool,
    environment_configured: bool,
}

#[derive(Deserialize)]
pub struct SaveOmpConfig {
    config: String,
}

#[derive(Deserialize)]
pub struct EquipSecrets {
    names: Vec<String>,
}

fn fail(status: StatusCode, message: &str) -> Failure {
    (status, Json(json!({ "error": message })))
}

fn internal(error: anyhow::Error) -> Failure {
    error!(%error, "omp config route failed");
    fail(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.")
}

async fn describe(state: &AppState, user: Uuid, agent_id: Uuid) -> Result<OmpConfigResponse, Failure> {
    let agent = state
        .world
        .get_agent(agent_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "No such agent."))?;
    let stored = state.world.omp_config(agent_id).await.map_err(internal)?;
    let (config, masked) = match stored.as_deref().map(omp_config::masked) {
        Some(Ok((text, masked))) => (Some(text), masked),
        Some(Err(_)) => (None, false),
        None => (None, false),
    };
    let names = stored
        .as_deref()
        .and_then(|text| omp_config::secret_names(text).ok())
        .unwrap_or_default();
    let owner = agent.owner_user_id;
    let entries = match owner {
        Some(owner) if !names.is_empty() => state.world.list_environment(owner).await.map_err(internal)?,
        _ => Vec::new(),
    };
    let secrets = names
        .into_iter()
        .map(|name| {
            let entry = entries.iter().find(|entry| entry.name == name);
            OmpSecret {
                in_environment: entry.is_some(),
                granted: entry.is_some_and(|entry| entry.agent_ids.contains(&agent_id)),
                name,
            }
        })
        .collect();
    Ok(OmpConfigResponse {
        models: stored.as_deref().map(omp_config::models).unwrap_or_default(),
        config,
        masked,
        secrets,
        private: agent.visibility == "private",
        can_equip: owner == Some(user),
        environment_configured: environment::configured(),
    })
}

type Described = ([(HeaderName, &'static str); 1], Json<OmpConfigResponse>);

fn respond(response: OmpConfigResponse) -> Described {
    ([(CACHE_CONTROL, "no-store")], Json(response))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<Described, Failure> {
    Ok(respond(describe(&state, user.0, id).await?))
}

pub async fn save(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveOmpConfig>,
) -> Result<Described, Failure> {
    let previous = state.world.omp_config(id).await.map_err(internal)?;
    let config = omp_config::unmask(&body.config, previous.as_deref())
        .map_err(|e| fail(StatusCode::BAD_REQUEST, &e.to_string()))?;
    state.world.set_omp_config(id, Some(&config)).await.map_err(internal)?;
    state.world.release_key_pauses(user.0, Some(id), None).await.map_err(internal)?;
    Ok(respond(describe(&state, user.0, id).await?))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    state.world.set_omp_config(id, None).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn equip(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
    Json(body): Json<EquipSecrets>,
) -> Result<Described, Failure> {
    let agent = state
        .world
        .get_agent(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "No such agent."))?;
    if agent.owner_user_id != Some(user.0) {
        return Err(fail(StatusCode::FORBIDDEN, "Only the owner can equip their keys."));
    }
    let wanted: BTreeSet<String> = body.names.into_iter().collect();
    if wanted.is_empty() || wanted.len() > 64 || !wanted.iter().all(|name| environment::valid_name(name)) {
        return Err(fail(StatusCode::BAD_REQUEST, "Give the key names to equip."));
    }
    let entries = state.world.list_environment(user.0).await.map_err(internal)?;
    for name in &wanted {
        let entry = entries
            .iter()
            .find(|entry| &entry.name == name)
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, &format!("{name} is not in your environment yet.")))?;
        if entry.agent_ids.contains(&id) {
            continue;
        }
        let mut agent_ids = entry.agent_ids.clone();
        agent_ids.push(id);
        match state.world.set_environment_agents(user.0, name, &agent_ids).await {
            Ok(true) => {}
            Ok(false) => return Err(fail(StatusCode::NOT_FOUND, &format!("{name} is not in your environment yet."))),
            Err(e) if e.is::<InvalidEnvironmentGrant>() => return Err(fail(StatusCode::BAD_REQUEST, "Only your own agents can hold your keys.")),
            Err(e) => return Err(internal(e)),
        }
    }
    state.world.release_key_pauses(user.0, Some(id), None).await.map_err(internal)?;
    Ok(respond(describe(&state, user.0, id).await?))
}
