use axum::{
    extract::{Path, State},
    http::{header::CACHE_CONTROL, StatusCode},
    Extension, Json,
};
use std::collections::HashSet;

use loka_shared::environment::{self, EnvironmentEntry, InvalidEnvironmentGrant, SharedKey, WantedKeys};
use loka_shared::providers::{self, Provider, PROVIDERS};
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::{auth::UserId, AppState};
use loka_shared::access::Viewer;

#[derive(Serialize)]
pub struct EnvironmentResponse {
    configured: bool,
    entries: Vec<EnvironmentEntry>,
    variants: Vec<loka_shared::key_environments::Variant>,
    choices: Vec<loka_shared::key_environments::Choice>,
    wanted: Vec<WantedKeys>,
    providers: Vec<ProviderStatus>,
}

#[derive(Serialize)]
pub struct ProviderStatus {
    #[serde(flatten)]
    provider: Provider,
    status: &'static str,
}

#[derive(Deserialize)]
pub struct CheckProvider {
    value: Option<String>,
    #[serde(default)]
    live: bool,
}

#[derive(Serialize)]
pub struct CheckResult {
    problem: Option<String>,
    live: &'static str,
    detail: Option<String>,
}

#[derive(Deserialize)]
pub struct SaveEnvironment {
    value: String,
}

#[derive(Deserialize)]
pub struct SetEnvironmentAgents {
    agent_ids: Vec<Uuid>,
}

fn internal(error: anyhow::Error) -> StatusCode {
    error!(%error, "environment route failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<
    (
        [(axum::http::HeaderName, &'static str); 1],
        Json<EnvironmentResponse>,
    ),
    StatusCode,
> {
    let entries = state
        .world
        .list_environment(user.0)
        .await
        .map_err(internal)?;
    let wanted = state.world.wanted_keys(user.0).await.map_err(internal)?;
    let providers = provider_statuses(&state, user.0).await.map_err(internal)?;
    let variants = state.world.list_environment_variants(user.0).await.map_err(internal)?;
    let choices = state.world.list_environment_choices(user.0).await.map_err(internal)?;
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(EnvironmentResponse {
            configured: environment::configured(),
            entries,
            variants,
            choices,
            wanted,
            providers,
        }),
    ))
}

pub async fn provider_statuses(state: &AppState, user: Uuid) -> anyhow::Result<Vec<ProviderStatus>> {
    let own: HashSet<String> = state.world.own_provider_names(user).await?.into_iter().collect();
    let mut shared: HashSet<String> = providers::server_key_names().into_iter().map(str::to_string).collect();
    if providers::share_server_keys() {
        for runner in state.runners.list(user).await.unwrap_or_default() {
            let advertised = runner.capabilities.get("provider_keys").and_then(serde_json::Value::as_array);
            shared.extend(advertised.into_iter().flatten().filter_map(|name| name.as_str().map(str::to_string)));
        }
    }
    Ok(PROVIDERS
        .iter()
        .map(|provider| ProviderStatus {
            provider: *provider,
            status: if own.contains(provider.env) {
                "own"
            } else if shared.contains(provider.env) {
                "shared"
            } else {
                "missing"
            },
        })
        .collect())
}

pub async fn check_provider(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<String>,
    Json(body): Json<CheckProvider>,
) -> Result<Json<CheckResult>, StatusCode> {
    let provider = providers::by_id(&id).ok_or(StatusCode::NOT_FOUND)?;
    let value = match body.value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
        Some(value) => value,
        None => state
            .world
            .environment_value(user.0, provider.env)
            .await
            .map_err(internal)?
            .ok_or(StatusCode::NOT_FOUND)?,
    };
    let problem = providers::format_problem(provider, &value);
    if !body.live || !provider.testable {
        return Ok(Json(CheckResult { problem, live: "skipped", detail: None }));
    }
    let (live, detail) = match providers::live_check(provider, &value).await {
        Ok(()) => ("ok", None),
        Err(detail) => ("failed", Some(detail)),
    };
    Ok(Json(CheckResult { problem, live, detail }))
}

pub async fn save(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(name): Path<String>,
    Json(body): Json<SaveEnvironment>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) || !environment::valid_value(&body.value) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !environment::configured() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    state
        .world
        .save_environment(user.0, &name, &body.value)
        .await
        .map_err(internal)?;
    state.world.release_key_pauses(user.0, None, Some(&name)).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_agents(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(name): Path<String>,
    Json(body): Json<SetEnvironmentAgents>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state
        .world
        .set_environment_agents(user.0, &name, &body.agent_ids)
        .await
    {
        Ok(true) => {
            state.world.release_key_pauses(user.0, None, Some(&name)).await.map_err(internal)?;
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) if error.is::<InvalidEnvironmentGrant>() => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(internal(error)),
    }
}

pub async fn delete_entry(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(name): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state
        .world
        .delete_environment(user.0, &name)
        .await
        .map_err(internal)?
    {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn dismiss_wanted(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((agent_id, name)): Path<(Uuid, String)>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state.world.dismiss_wanted_key(user.0, agent_id, &name).await.map_err(internal)? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Serialize)]
pub struct SharedKeysResponse {
    configured: bool,
    keys: Vec<SharedKey>,
}

#[derive(Deserialize)]
pub struct SaveSharedKey {
    value: Option<String>,
    note: Option<String>,
}

pub async fn list_shared(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Result<([(axum::http::HeaderName, &'static str); 1], Json<SharedKeysResponse>), StatusCode> {
    let keys = state.world.list_shared_keys(viewer.org()).await.map_err(internal)?;
    Ok(([(CACHE_CONTROL, "no-store")], Json(SharedKeysResponse { configured: environment::configured(), keys })))
}

pub async fn save_shared(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(viewer): Extension<Viewer>,
    Path(name): Path<String>,
    Json(body): Json<SaveSharedKey>,
) -> Result<StatusCode, StatusCode> {
    let value = body.value.filter(|v| !v.is_empty());
    let note = body.note.map(|n| n.trim().to_string());
    if !environment::valid_name(&name) || value.as_deref().is_some_and(|v| !environment::valid_value(v)) || note.as_deref().is_some_and(|n| n.chars().count() > 500) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !environment::configured() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    match state.world.save_shared_key(viewer.org(), user.0, &name, value.as_deref(), note.as_deref()).await.map_err(internal)? {
        true => {
            Ok(StatusCode::NO_CONTENT)
        }
        false => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn set_shared_agents(
    State(state): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Path(name): Path<String>,
    Json(body): Json<SetEnvironmentAgents>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state.world.set_shared_key_agents(viewer.org(), &name, &body.agent_ids).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) if error.is::<InvalidEnvironmentGrant>() => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(internal(error)),
    }
}

pub async fn delete_shared(State(state): State<AppState>, Extension(viewer): Extension<Viewer>, Path(name): Path<String>) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state.world.delete_shared_key(viewer.org(), &name).await.map_err(internal)? {
        true => {
            Ok(StatusCode::NO_CONTENT)
        }
        false => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn save_variant(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((name, label)): Path<(String, String)>,
    Json(body): Json<SaveEnvironment>,
) -> Result<StatusCode, StatusCode> {
    let label = loka_shared::key_environments::normalize_environment(&label);
    if !environment::valid_name(&name) || !loka_shared::key_environments::valid_environment(&label) || !environment::valid_value(&body.value) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !environment::configured() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    match state.world.save_environment_variant(user.0, &name, &label, &body.value).await {
        Ok(true) => {
            state.world.release_key_pauses(user.0, None, Some(&name)).await.map_err(internal)?;
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) if error.to_string().starts_with("a key can have at most") => Err(StatusCode::CONFLICT),
        Err(error) => Err(internal(error)),
    }
}

pub async fn delete_variant(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path((name, label)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    if !environment::valid_name(&name) || !loka_shared::key_environments::valid_environment(&label) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match state.world.delete_environment_variant(user.0, &name, &label).await.map_err(internal)? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
pub struct ChooseEnvironment {
    agent_id: Uuid,
    name: Option<String>,
    environment: String,
    hours: Option<i64>,
}

pub async fn choose(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<ChooseEnvironment>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use loka_shared::key_environments::ChoiceError;
    let name = body.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    match state.world.choose_environment(user.0, body.agent_id, name, &body.environment, body.hours, "person").await.map_err(internal)? {
        Ok(names) => {
            for key in &names {
                state.world.release_key_pauses(user.0, Some(body.agent_id), Some(key)).await.map_err(internal)?;
            }
            Ok(Json(serde_json::json!({ "keys": names })))
        }
        Err(ChoiceError::NotYourAgent) => Err(StatusCode::FORBIDDEN),
        Err(ChoiceError::NoSuchEnvironment) => Err(StatusCode::NOT_FOUND),
        Err(ChoiceError::BadRequest) => Err(StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
pub struct NewCommandRule {
    agent_id: Option<Uuid>,
    kind: String,
    rule: String,
}

pub async fn list_command_rules(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let rules = state.world.list_command_rules(user.0).await.map_err(internal)?;
    Ok(Json(serde_json::json!({ "rules": rules })))
}

pub async fn add_command_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Json(body): Json<NewCommandRule>,
) -> Result<axum::response::Response, StatusCode> {
    use axum::response::IntoResponse;
    use loka_shared::command_rules::RuleError;
    match state.world.add_command_rule(user.0, body.agent_id, &body.kind, &body.rule, "person").await.map_err(internal)? {
        Ok(rule) => Ok(Json(rule).into_response()),
        Err(RuleError::NotYourAgent) => Err(StatusCode::FORBIDDEN),
        Err(error) => Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": error.to_string() }))).into_response()),
    }
}

pub async fn delete_command_rule(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    match state.world.remove_command_rule(user.0, id).await.map_err(internal)? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}
