use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde::Serialize;
use uuid::Uuid;

use crate::world::WorldStore;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub env: &'static str,
    #[serde(skip)]
    pub aliases: &'static [&'static str],
    pub unlocks: &'static str,
    pub docs: &'static str,
    pub hint: &'static str,
    #[serde(skip)]
    pub prefix: &'static str,
    pub testable: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "openai",
        label: "OpenAI",
        env: "OPENAI_API_KEY",
        aliases: &["CODEX_API_KEY"],
        unlocks: "Codex agents on cloud devices",
        docs: "https://platform.openai.com/api-keys",
        hint: "starts with sk-",
        prefix: "sk-",
        testable: true,
    },
    Provider {
        id: "anthropic",
        label: "Anthropic",
        env: "ANTHROPIC_API_KEY",
        aliases: &[],
        unlocks: "Claude agents on cloud devices, billed to your API account",
        docs: "https://console.anthropic.com/settings/keys",
        hint: "starts with sk-ant-",
        prefix: "sk-ant-",
        testable: true,
    },
    Provider {
        id: "claude_subscription",
        label: "Claude subscription",
        env: "CLAUDE_CODE_OAUTH_TOKEN",
        aliases: &[],
        unlocks: "Claude agents on cloud devices, on your Claude plan (run claude setup-token)",
        docs: "https://docs.anthropic.com/en/docs/claude-code/setup",
        hint: "starts with sk-ant-oat",
        prefix: "sk-ant-oat",
        testable: false,
    },
    Provider {
        id: "openrouter",
        label: "OpenRouter",
        env: "OPENROUTER_API_KEY",
        aliases: &[],
        unlocks: "OpenCode and OpenRouter models in omp",
        docs: "https://openrouter.ai/keys",
        hint: "starts with sk-or-",
        prefix: "sk-or-",
        testable: true,
    },
    Provider {
        id: "fal",
        label: "fal",
        env: "FAL_KEY",
        aliases: &[],
        unlocks: "Live voice and fal models in omp",
        docs: "https://fal.ai/dashboard/keys",
        hint: "looks like id:secret",
        prefix: "",
        testable: true,
    },
    Provider {
        id: "xai",
        label: "xAI",
        env: "XAI_API_KEY",
        aliases: &[],
        unlocks: "Grok models",
        docs: "https://console.x.ai",
        hint: "starts with xai-",
        prefix: "xai-",
        testable: true,
    },
    Provider {
        id: "gemini",
        label: "Google Gemini",
        env: "GEMINI_API_KEY",
        aliases: &[],
        unlocks: "Gemini models",
        docs: "https://aistudio.google.com/apikey",
        hint: "starts with AIza",
        prefix: "AIza",
        testable: true,
    },
];

pub fn by_env(name: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.env == name)
}

pub fn by_id(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

pub fn env_names() -> impl Iterator<Item = &'static str> {
    PROVIDERS
        .iter()
        .flat_map(|provider| std::iter::once(provider.env).chain(provider.aliases.iter().copied()))
}

pub fn model_key_label(cli: &str, model: &str, omp_config: Option<&str>) -> String {
    match cli {
        "claude" => "ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN".into(),
        "codex" => "OPENAI_API_KEY".into(),
        "opencode" => "OPENROUTER_API_KEY".into(),
        "cursor" => "CURSOR_API_KEY".into(),
        "omp" => {
            let provider = model.split_once('/').map(|(provider, _)| provider).unwrap_or(model);
            let custom: Vec<String> = omp_config
                .map(|text| crate::omp_config::provider_secret_names(text, provider).into_iter().collect())
                .unwrap_or_default();
            if !custom.is_empty() {
                custom.join(" or ")
            } else if let Some(known) = by_id(provider).or_else(|| (provider == "google").then(|| by_id("gemini")).flatten()) {
                known.env.into()
            } else {
                format!("the {provider} provider key")
            }
        }
        _ => "its model key".into(),
    }
}

pub fn share_server_keys() -> bool {
    !matches!(
        std::env::var("WORLD_SHARE_SERVER_KEYS").ok().as_deref().map(str::trim),
        Some("off" | "false" | "0" | "no")
    )
}

pub fn server_value(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

pub fn shared_server_value(name: &str) -> Option<String> {
    share_server_keys().then(|| server_value(name)).flatten()
}

pub fn server_key_names() -> Vec<&'static str> {
    if !share_server_keys() {
        return Vec::new();
    }
    PROVIDERS.iter().filter(|provider| server_value(provider.env).is_some()).map(|provider| provider.env).collect()
}

pub async fn live_check(provider: &Provider, key: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "Could not reach the provider".to_string())?;
    let request = match provider.id {
        "openai" => client.get("https://api.openai.com/v1/models").bearer_auth(key),
        "anthropic" => client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "openrouter" => client.get("https://openrouter.ai/api/v1/key").bearer_auth(key),
        "xai" => client.get("https://api.x.ai/v1/models").bearer_auth(key),
        "gemini" => client
            .get("https://generativelanguage.googleapis.com/v1beta/models")
            .header("x-goog-api-key", key),
        "fal" => client
            .post("https://rest.fal.ai/tokens/")
            .header("Authorization", format!("Key {key}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "allowed_apps": ["grok-voice"], "token_expiration": 60 }).to_string()),
        _ => return Ok(()),
    };
    let response = request.send().await.map_err(|_| "Could not reach the provider".to_string())?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err(format!("{} rejected this key", provider.label)),
        429 => Err(format!("{} accepted the key but is rate limiting it", provider.label)),
        status => Err(format!("{} answered with status {status}", provider.label)),
    }
}

pub fn format_problem(provider: &Provider, value: &str) -> Option<String> {
    let value = value.trim();
    if value.chars().any(char::is_whitespace) {
        return Some("Keys cannot contain spaces or line breaks".into());
    }
    if value.len() < 16 {
        return Some(format!("That looks too short for a {} key", provider.label));
    }
    if provider.id == "fal" && !value.contains(':') {
        return Some(format!("{} keys {}", provider.label, provider.hint));
    }
    if provider.id == "anthropic" && value.starts_with("sk-ant-oat") {
        return Some("That is a Claude subscription token, save it as CLAUDE_CODE_OAUTH_TOKEN".into());
    }
    if !provider.prefix.is_empty() && !value.starts_with(provider.prefix) {
        return Some(format!("{} keys usually {}", provider.label, provider.hint));
    }
    None
}

pub fn expand(keys: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in keys {
        if let Some(provider) = by_env(&name) {
            for alias in provider.aliases {
                out.insert(alias.to_string(), value.clone());
            }
        }
        out.insert(name, value);
    }
    out
}

impl WorldStore {
    pub async fn own_provider_names(&self, owner: Uuid) -> Result<Vec<String>> {
        let names: Vec<&str> = PROVIDERS.iter().map(|provider| provider.env).collect();
        Ok(sqlx::query_scalar(
            "SELECT name FROM world_environment WHERE owner_user_id = $1 AND name = ANY($2) ORDER BY name",
        )
        .bind(owner)
        .bind(&names)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn provider_key(&self, owner: Uuid, name: &str) -> Result<Option<String>> {
        if by_env(name).is_none() {
            return Err(anyhow!("unknown provider key {name}"));
        }
        if let Some(own) = self.environment_value(owner, name).await? {
            return Ok(Some(own));
        }
        Ok(shared_server_value(name))
    }

    pub async fn provider_keys_for_agent(&self, agent_id: Uuid) -> Result<BTreeMap<String, String>> {
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT owner_user_id FROM agent_profiles WHERE id = $1 AND visibility = 'private'",
        )
        .bind(agent_id)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        let Some(owner) = owner else {
            return Ok(BTreeMap::new());
        };
        let mut keys = BTreeMap::new();
        for name in self.own_provider_names(owner).await? {
            if let Some(value) = self.environment_value_for_agent(owner, agent_id, &name).await? {
                keys.insert(name, value);
            }
        }
        Ok(expand(keys))
    }

    pub async fn secrets_for_agent(&self, agent_id: Uuid) -> Result<BTreeMap<String, String>> {
        let mut secrets = self.provider_keys_for_agent(agent_id).await?;
        secrets.extend(self.environment_for_agent(agent_id).await?);
        Ok(secrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_names_are_settable_environment_keys() {
        for name in env_names() {
            assert!(crate::environment::valid_name(name), "{name}");
        }
    }

    #[test]
    fn format_checks_are_light() {
        let openai = by_id("openai").unwrap();
        assert!(format_problem(openai, "sk-proj-abcdefghijklmnop").is_none());
        assert!(format_problem(openai, "abcdefghijklmnopqrstu").is_some());
        assert!(format_problem(openai, "sk-short").is_some());
        let fal = by_id("fal").unwrap();
        assert!(format_problem(fal, "0123abcd-ef:0123456789abcdef").is_none());
        assert!(format_problem(fal, "0123abcdef0123456789abcdef").is_some());
        let anthropic = by_id("anthropic").unwrap();
        assert!(format_problem(anthropic, "sk-ant-oat01-abcdefghijklmnop").is_some());
        assert!(format_problem(anthropic, "sk-ant-api03-abcdefghijklmnop").is_none());
    }

    #[test]
    fn aliases_follow_their_key() {
        let keys = expand(BTreeMap::from([("OPENAI_API_KEY".to_string(), "sk-x".to_string())]));
        assert_eq!(keys.get("CODEX_API_KEY").map(String::as_str), Some("sk-x"));
        assert_eq!(keys.len(), 2);
    }
}
