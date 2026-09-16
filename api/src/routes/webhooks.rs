use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::{info, warn};

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

fn verify_raw_hmac(secret: Option<&str>, signature_header: Option<&str>, body: &[u8]) -> bool {
    match signature_header {
        Some(signature) => verify_hex_hmac(secret, signature.strip_prefix("sha256=").unwrap_or(signature), body),
        None => {
            warn!("Webhook missing signature header");
            false
        }
    }
}

fn verify_hex_hmac(secret: Option<&str>, hex_sig: &str, body: &[u8]) -> bool {
    let Some(secret) = secret.filter(|secret| !secret.is_empty()) else {
        warn!("Webhook rejected because no signing secret is configured");
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig.trim()) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

pub async fn linear_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let secret = std::env::var("LINEAR_WEBHOOK_SECRET").ok();
    let signature = headers
        .get("linear-signature")
        .and_then(|v| v.to_str().ok());
    if !verify_raw_hmac(secret.as_deref(), signature, body.as_bytes()) {
        warn!("Linear webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let action = payload["action"].as_str().unwrap_or("");
    let entity_type = payload["type"].as_str().unwrap_or("");
    if action != "create" || entity_type != "Issue" {
        return StatusCode::OK;
    }

    let data = match payload.get("data") {
        Some(d) => d,
        None => return StatusCode::BAD_REQUEST,
    };

    let external_id = data["id"].as_str().unwrap_or("");
    if external_id.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    if let Ok(Some(_)) = state.db.find_task_by_external("linear", external_id).await {
        info!(external_id, "Linear: task already exists, skipping");
        return StatusCode::OK;
    }

    let identifier = data["identifier"].as_str().unwrap_or("");
    let raw_title = data["title"].as_str().unwrap_or("Untitled");
    let title = if identifier.is_empty() {
        raw_title.to_string()
    } else {
        format!("{}: {}", identifier, raw_title)
    };
    let description = data["description"].as_str().unwrap_or("");
    let url = data["url"].as_str().unwrap_or("");
    let priority = linear_priority(data["priority"].as_i64());

    let user_id = match state.db.first_user_id().await {
        Ok(Some(id)) => id,
        _ => {
            warn!("Linear webhook: no user found to assign task");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    match state
        .db
        .create_task_full(
            user_id,
            &title,
            if description.is_empty() {
                None
            } else {
                Some(description)
            },
            None,
            priority,
            None,
            None,
            None,
            None,
            Some("linear"),
            Some(external_id),
            if url.is_empty() { None } else { Some(url) },
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    {
        Ok(task) => {
            info!(task_id = %task.id, external_id, "Linear: task ingested");
            let _ = state
                .db
                .insert_log(
                    task.id,
                    "webhook",
                    &format!("Ingested from Linear: {}", url),
                    "info",
                    None,
                )
                .await;
            StatusCode::OK
        }
        Err(e) => {
            warn!(error = %e, "Linear: failed to create task");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn linear_priority(p: Option<i64>) -> &'static str {
    match p.unwrap_or(0) {
        1 => "urgent",
        2 => "high",
        4 => "low",
        _ => "medium",
    }
}

pub async fn clickup_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let secret = std::env::var("CLICKUP_WEBHOOK_SECRET").ok();
    let signature = headers.get("x-signature").and_then(|v| v.to_str().ok());
    if !verify_raw_hmac(secret.as_deref(), signature, body.as_bytes()) {
        warn!("ClickUp webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let event = payload["event"].as_str().unwrap_or("");
    if event != "taskCreated" {
        return StatusCode::OK;
    }

    let task_id = payload["task_id"].as_str().unwrap_or("");
    if task_id.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    if let Ok(Some(_)) = state.db.find_task_by_external("clickup", task_id).await {
        info!(task_id, "ClickUp: task already exists, skipping");
        return StatusCode::OK;
    }

    let fetched = match fetch_clickup_task(task_id).await {
        Ok(Some(t)) => Some(t),
        Ok(None) => None,
        Err(e) => {
            warn!(task_id, error = %e, "ClickUp: enrichment failed, falling back to payload");
            None
        }
    };

    let payload_title = payload
        .pointer("/task/name")
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload["history_items"]
                .as_array()
                .and_then(|items| items.iter().find(|h| h["field"] == "name"))
                .and_then(|h| h["after"].as_str())
        });

    let title = fetched
        .as_ref()
        .and_then(|t| t["name"].as_str())
        .or(payload_title)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("ClickUp task {}", task_id));

    let description = fetched.as_ref().and_then(|t| {
        t["markdown_description"]
            .as_str()
            .or_else(|| t["description"].as_str())
            .or_else(|| t["text_content"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });

    let external_url = fetched
        .as_ref()
        .and_then(|t| t["url"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://app.clickup.com/t/{}", task_id));

    let priority = fetched
        .as_ref()
        .and_then(|t| t["priority"].pointer("/priority"))
        .and_then(|v| v.as_str())
        .map(clickup_priority)
        .unwrap_or("medium");

    let user_id = match state.db.first_user_id().await {
        Ok(Some(id)) => id,
        _ => {
            warn!("ClickUp webhook: no user found to assign task");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    match state
        .db
        .create_task_full(
            user_id,
            &title,
            description.as_deref(),
            None,
            priority,
            None,
            None,
            None,
            None,
            Some("clickup"),
            Some(task_id),
            Some(&external_url),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    {
        Ok(task) => {
            info!(task_id_db = %task.id, task_id, enriched = fetched.is_some(), "ClickUp: task ingested");
            let _ = state
                .db
                .insert_log(
                    task.id,
                    "webhook",
                    &format!("Ingested from ClickUp: {}", external_url),
                    "info",
                    None,
                )
                .await;
            StatusCode::OK
        }
        Err(e) => {
            warn!(error = %e, "ClickUp: failed to create task");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn fetch_clickup_task(task_id: &str) -> anyhow::Result<Option<serde_json::Value>> {
    let Ok(api_token) = std::env::var("CLICKUP_API_TOKEN") else {
        return Ok(None);
    };

    let url =
        format!("https://api.clickup.com/api/v2/task/{task_id}?include_markdown_description=true");
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", api_token)
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("ClickUp API returned {status}: {text}");
    }
    Ok(Some(resp.json::<serde_json::Value>().await?))
}

fn clickup_priority(p: &str) -> &'static str {
    match p.to_ascii_lowercase().as_str() {
        "urgent" => "urgent",
        "high" => "high",
        "low" => "low",
        _ => "medium",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn signatures_fail_closed_and_verify_exactly() {
        let body = br#"{"action":"create"}"#;
        let good = sign("s3cret", body);
        assert!(verify_raw_hmac(Some("s3cret"), Some(&good), body));
        assert!(!verify_raw_hmac(Some(""), Some(&good), body));
        assert!(!verify_raw_hmac(Some("s3cret"), Some(&sign("other", body)), body));
        assert!(!verify_raw_hmac(Some("s3cret"), Some("zz"), body));
    }
}
