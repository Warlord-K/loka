use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::world::WorldStore;

pub const WATCHES_MAX: i64 = 64;
pub const WATCH_DAYS_DEFAULT: i64 = 30;
pub const WATCH_DAYS_MAX: i64 = 180;
const MATCH_KEYS_MAX: usize = 4;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Watch {
    pub id: Uuid,
    pub event_type: String,
    #[sqlx(rename = "match")]
    #[serde(rename = "match")]
    pub matcher: Value,
    pub task_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Watch {
    pub fn label(&self) -> String {
        let fields = self.matcher.as_object().cloned().unwrap_or_default();
        if let Some(channel) = fields.get("slack_channel_id").and_then(Value::as_str) {
            let place = match self.note.as_deref().filter(|n| n.starts_with('#')) {
                Some(name) => format!("{name} ({channel})"),
                None => channel.to_string(),
            };
            let mut out = match fields.get("thread_root").and_then(Value::as_str) {
                Some(root) => format!("Slack thread {root} in {place}"),
                None => format!("Slack channel {place}"),
            };
            if let Some(link) = self.note.as_deref().filter(|n| n.starts_with("https://")) {
                out.push_str(&format!(" {link}"));
            }
            if let Some(at) = self.expires_at {
                out.push_str(&format!(" (until {})", at.format("%Y-%m-%d")));
            }
            return out;
        }
        let target = match (fields.get("repo").and_then(Value::as_str), fields.get("number").and_then(Value::as_i64)) {
            (Some(repo), Some(n)) => format!("{repo}#{n}"),
            (Some(repo), None) => repo.to_string(),
            _ => self.matcher.to_string(),
        };
        let mut out = format!("{} on {target}", self.event_type);
        if let Some(task) = self.task_id {
            out.push_str(&format!(" (quest {}, ends with its pull request)", &task.to_string()[..8]));
        } else if let Some(at) = self.expires_at {
            out.push_str(&format!(" (until {})", at.format("%Y-%m-%d")));
        }
        out
    }
}

pub fn check_match(raw: &Value) -> Result<Value> {
    let Some(fields) = raw.as_object() else {
        bail!("match must be an object like {{\"repo\": \"owner/name\", \"number\": 12}}");
    };
    if fields.is_empty() {
        bail!("match needs at least one field");
    }
    if fields.len() > MATCH_KEYS_MAX {
        bail!("match takes at most {MATCH_KEYS_MAX} fields");
    }
    let mut out = Map::new();
    for (key, value) in fields {
        if key.is_empty() || key.len() > 32 || !key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            bail!("{key:?} is not a payload field name; use lowercase letters, digits and underscores");
        }
        let value = match value {
            Value::String(s) if !s.trim().is_empty() && s.len() <= 200 => {
                let s = s.trim();
                Value::String(if key == "repo" { s.to_lowercase() } else { s.to_string() })
            }
            Value::Number(n) if n.is_i64() => value.clone(),
            Value::Bool(_) => value.clone(),
            _ => bail!("match values must be short text, whole numbers or true/false"),
        };
        out.insert(key.clone(), value);
    }
    if let Some(repo) = out.get("repo").and_then(Value::as_str) {
        let mut parts = repo.split('/');
        let ok = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && !p.bytes().all(|b| b == b'.') && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')));
        if !(ok(parts.next()) && ok(parts.next()) && parts.next().is_none()) {
            bail!("repo must look like owner/name");
        }
    }
    Ok(Value::Object(out))
}

pub fn parse_pr_ref(raw: &str) -> Option<(String, i64)> {
    let raw = raw.trim().trim_end_matches('/');
    let raw = raw.strip_prefix("https://github.com/").unwrap_or(raw);
    let (repo, number) = if let Some((repo, n)) = raw.split_once('#') {
        (repo.to_string(), n)
    } else {
        let parts: Vec<&str> = raw.split('/').collect();
        match parts.as_slice() {
            [owner, name, "pull" | "pulls" | "issues", n, ..] => (format!("{owner}/{name}"), *n),
            _ => return None,
        }
    };
    let number = number.parse::<i64>().ok().filter(|n| *n > 0)?;
    check_match(&serde_json::json!({ "repo": repo })).ok()?;
    Some((repo.to_lowercase(), number))
}

impl WorldStore {
    pub async fn watches(&self, agent_id: Uuid) -> Result<Vec<Watch>> {
        sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1 AND expires_at <= NOW()")
            .bind(agent_id)
            .execute(self.pool())
            .await?;
        Ok(sqlx::query_as::<_, Watch>(
            "SELECT id, event_type, match, task_id, expires_at, note, created_at FROM agent_subscriptions
             WHERE agent_id = $1 AND (match <> '{}'::jsonb OR task_id IS NOT NULL)
             ORDER BY task_id NULLS LAST, created_at",
        )
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn add_watch(&self, agent_id: Uuid, event_type: &str, matcher: Value, days: Option<i64>, note: Option<&str>) -> Result<Watch> {
        self.upsert_watch(agent_id, event_type, matcher, days, note, false).await
    }

    pub async fn extend_watch(&self, agent_id: Uuid, event_type: &str, matcher: Value, days: i64, note: Option<&str>) -> Result<Watch> {
        self.upsert_watch(agent_id, event_type, matcher, Some(days), note, true).await
    }

    async fn upsert_watch(&self, agent_id: Uuid, event_type: &str, matcher: Value, days: Option<i64>, note: Option<&str>, extend_only: bool) -> Result<Watch> {
        let Some(event_type) = crate::world::normalize_event_type(event_type) else {
            bail!("{event_type:?} is not an event type");
        };
        let matcher = check_match(&matcher)?;
        let days = days.unwrap_or(WATCH_DAYS_DEFAULT).clamp(1, WATCH_DAYS_MAX);
        let note = note.map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(200).collect::<String>());
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM agent_profiles WHERE id = $1 FOR UPDATE").bind(agent_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1 AND expires_at <= NOW()")
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        let expires = if extend_only {
            "CASE WHEN agent_subscriptions.expires_at IS NULL THEN NULL ELSE GREATEST(agent_subscriptions.expires_at, EXCLUDED.expires_at) END"
        } else {
            "EXCLUDED.expires_at"
        };
        let watch = sqlx::query_as::<_, Watch>(&format!(
            "INSERT INTO agent_subscriptions (agent_id, event_type, match, expires_at, note)
             VALUES ($1, $2, $3, NOW() + make_interval(days => $4), $5)
             ON CONFLICT (agent_id, event_type, match, COALESCE(task_id, '00000000-0000-0000-0000-000000000000'::uuid))
             DO UPDATE SET expires_at = {expires}, note = COALESCE(EXCLUDED.note, agent_subscriptions.note)
             RETURNING id, event_type, match, task_id, expires_at, note, created_at"
        ))
        .bind(agent_id)
        .bind(&event_type)
        .bind(&matcher)
        .bind(days as i32)
        .bind(note)
        .fetch_one(&mut *tx)
        .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_subscriptions WHERE agent_id = $1 AND task_id IS NULL AND match <> '{}'::jsonb",
        )
        .bind(agent_id)
        .fetch_one(&mut *tx)
        .await?;
        if count > WATCHES_MAX {
            bail!("at most {WATCHES_MAX} watches per agent; stop watching something first");
        }
        tx.commit().await?;
        Ok(watch)
    }

    pub async fn remove_watches(&self, agent_id: Uuid, event_type: &str, matcher: Option<&Value>) -> Result<u64> {
        let matcher = match matcher {
            Some(m) => Some(check_match(m)?),
            None => None,
        };
        Ok(sqlx::query(
            "DELETE FROM agent_subscriptions
             WHERE agent_id = $1 AND task_id IS NULL AND match <> '{}'::jsonb
               AND ($2 = '' OR event_type = $2)
               AND ($3::jsonb IS NULL OR match @> $3)",
        )
        .bind(agent_id)
        .bind(event_type)
        .bind(matcher)
        .execute(self.pool())
        .await?
        .rows_affected())
    }

    pub async fn remove_watch(&self, agent_id: Uuid, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1 AND id = $2 AND task_id IS NULL AND match <> '{}'::jsonb")
            .bind(agent_id)
            .bind(id)
            .execute(self.pool())
            .await?
            .rows_affected()
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_are_checked_and_normalised() {
        assert_eq!(check_match(&json!({ "repo": " Acme/World ", "number": 12 })).unwrap(), json!({ "repo": "acme/world", "number": 12 }));
        assert!(check_match(&json!({})).is_err());
        assert!(check_match(&json!([])).is_err());
        assert!(check_match(&json!({ "Repo": "a/b" })).is_err());
        assert!(check_match(&json!({ "repo": "nope" })).is_err());
        assert!(check_match(&json!({ "number": 1.5 })).is_err());
        assert!(check_match(&json!({ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5 })).is_err());
        assert!(check_match(&json!({ "title": { "nested": true } })).is_err());
    }

    #[test]
    fn pull_request_refs_parse() {
        assert_eq!(parse_pr_ref("Acme/World#12"), Some(("acme/world".into(), 12)));
        assert_eq!(parse_pr_ref("https://github.com/Acme/World/pull/12"), Some(("acme/world".into(), 12)));
        assert_eq!(parse_pr_ref("https://github.com/acme/world/issues/3/"), Some(("acme/world".into(), 3)));
        assert_eq!(parse_pr_ref("acme/world"), None);
        assert_eq!(parse_pr_ref("acme/world#0"), None);
        assert_eq!(parse_pr_ref("../x#4"), None);
    }
}
