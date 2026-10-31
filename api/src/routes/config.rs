use axum::{extract::State, http::StatusCode, Extension, Json};
use serde_json::{json, Value};

use crate::auth::UserId;
use crate::AppState;

pub async fn get(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
) -> Result<Json<Value>, StatusCode> {
    resolved(&state, user.0).await.map(Json)
}

pub async fn resolved(state: &AppState, user: uuid::Uuid) -> Result<Value, StatusCode> {
    let db = state.db.clone();
    let config = state.config.clone();
    let shared_workspace = state.db.is_shared_workspace();
    let webhook_url_configured = std::env::var("AGENT_WEBHOOK_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("TUNNEL_AGENT_HOSTNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .is_some();
    let result = build_config_response(&db, &config, shared_workspace, webhook_url_configured)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut result = result;
    if let Some(fields) = result.as_object_mut() {
        fields.insert("voiceRealtime".into(), Value::Bool(crate::routes::voice::fal_key_for(state, user).await.is_some()));
    }
    let runners = state.runners.list(user).await.unwrap_or_default();
    let discovered = runners.iter().map(|r| &r.capabilities).collect::<Vec<_>>();
    let opencode = loka_shared::backends::opencode_enabled()
        || discovered.iter().any(|caps| loka_shared::backends::runner_offers_opencode(caps));
    if let Some(backends) = result.get_mut("backends").and_then(Value::as_object_mut) {
        merge_discovered_models(backends, &discovered);
        if opencode {
            backends
                .entry(loka_shared::backends::OPENCODE)
                .or_insert_with(|| json!({ "models": [], "default_model": "" }));
        } else {
            backends.remove(loka_shared::backends::OPENCODE);
        }
    }
    let providers = crate::routes::environment::provider_statuses(state, user)
        .await
        .map(|list| {
            list.iter()
                .map(|p| {
                    let p = serde_json::to_value(p).unwrap_or_default();
                    json!({ "id": p["id"], "env": p["env"], "status": p["status"] })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(fields) = result.as_object_mut() {
        fields.insert("opencodeEnabled".into(), Value::Bool(opencode));
        fields.insert("providers".into(), Value::Array(providers));
    }
    Ok(result)
}

fn merge_discovered_models(backends: &mut serde_json::Map<String, Value>, capabilities: &[&Value]) {
    for caps in capabilities {
        let Some(found) = caps.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (cli, list) in found {
            let pairs: Vec<(String, String)> = list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|pair| {
                    let id = pair.get(0)?.as_str()?.to_string();
                    let label = pair.get(1).and_then(Value::as_str).unwrap_or(&id).to_string();
                    Some((id, label))
                })
                .collect();
            if pairs.is_empty() {
                continue;
            }
            let entry = backends
                .entry(cli.clone())
                .or_insert_with(|| json!({ "models": [], "default_model": pairs[0].0 }));
            let configured: Vec<String> = entry
                .get("models")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect();
            let mut models: Vec<String> = Vec::new();
            for id in pairs.iter().map(|(id, _)| id).chain(configured.iter()) {
                if !models.contains(id) {
                    models.push(id.clone());
                }
            }
            let mut labels = entry
                .get("labels")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (id, label) in &pairs {
                if label != id {
                    labels.entry(id.clone()).or_insert_with(|| json!(label));
                }
            }
            entry["models"] = json!(models);
            entry["labels"] = Value::Object(labels);
        }
    }
}

async fn build_config_response(
    db: &loka_shared::db::Database,
    config: &crate::ApiConfig,
    shared_workspace: bool,
    webhook_url_configured: bool,
) -> anyhow::Result<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut repos: Vec<Value> = config
        .repos
        .iter()
        .map(|r| {
            seen.insert(r.repo.clone());
            json!({
                "repo": r.repo,
                "name": r.repo.split('/').next_back().unwrap_or(&r.repo),
                "branch": r.branch,
            })
        })
        .collect();

    if let Ok(db_profiles) = db.get_all_repo_profiles().await {
        for p in db_profiles {
            if seen.insert(p.repo.clone()) {
                repos.push(json!({
                    "repo": p.repo,
                    "name": p.repo.split('/').next_back().unwrap_or(&p.repo),
                    "branch": p.branch,
                }));
            }
        }
    }

    let mut backends = serde_json::Map::new();
    for (name, cfg) in &config.backends {
        backends.insert(
            name.clone(),
            json!({
                "models": cfg.models,
                "default_model": cfg.default_model.as_deref().unwrap_or(cfg.models.first().map(|s| s.as_str()).unwrap_or("")),
            }),
        );
    }

    if backends.is_empty() {
        backends.insert(
            "claude".to_string(),
            json!({ "models": ["haiku", "sonnet", "opus"], "default_model": "sonnet" }),
        );
    }
    backends.entry("omp").or_insert_with(|| {
        json!({ "models": ["fal/kimi-k3", "fal/glm-5.3"], "default_model": "fal/kimi-k3" })
    });
    if let Some(backend) = backends.get_mut(config.default_cli()).and_then(Value::as_object_mut) {
        backend.insert("default".into(), Value::Bool(true));
    }

    let skills: Vec<&str> = config.skills.iter().map(|s| s.name.as_str()).collect();
    let mcp_servers: Vec<&str> = config.mcp_servers.iter().map(|s| s.name.as_str()).collect();

    Ok(json!({
        "repos": repos,
        "backends": backends,
        "skills": skills,
        "mcpServers": mcp_servers,
        "sharedWorkspace": shared_workspace,
        "webhookUrlConfigured": webhook_url_configured,
    }))
}

#[cfg(test)]
mod tests {
    use super::merge_discovered_models;
    use serde_json::json;

    #[test]
    fn discovered_models_come_first_and_keep_config_extras() {
        let mut backends = json!({
            "claude": {"models": ["sonnet", "custom"], "default_model": "sonnet"}
        })
        .as_object()
        .cloned()
        .unwrap();
        let caps = json!({"models": {
            "claude": [["opus", "Opus 5.5"], ["sonnet", "Sonnet 5"]],
            "omp": [["fal/kimi-k3", "fal Kimi-K3"]]
        }});
        merge_discovered_models(&mut backends, &[&caps]);
        assert_eq!(backends["claude"]["models"], json!(["opus", "sonnet", "custom"]));
        assert_eq!(backends["claude"]["default_model"], "sonnet");
        assert_eq!(backends["claude"]["labels"]["opus"], "Opus 5.5");
        assert_eq!(backends["omp"]["default_model"], "fal/kimi-k3");
    }
}
