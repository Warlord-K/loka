use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_yaml::{Mapping, Value};
use uuid::Uuid;

use crate::environment::reference_names;
use crate::world::{looks_secret, mask_text, mask_url, WorldStore};

pub const MAX_CONFIG_LEN: usize = 64 * 1024;
const MASK: &str = "***";
const SENSITIVE_SUFFIXES: &[&str] = &["key", "token", "secret", "password", "authorization"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmpModel {
    pub id: String,
    pub name: Option<String>,
}

pub fn parse(text: &str) -> Result<Mapping> {
    if text.len() > MAX_CONFIG_LEN {
        bail!("the config is larger than 64 KB");
    }
    if text.contains('\0') {
        bail!("the config contains a NUL byte");
    }
    let value: Value = serde_yaml::from_str(text).map_err(|e| anyhow!("the config is not valid YAML: {e}"))?;
    let Value::Mapping(root) = value else {
        bail!("the config must be a YAML mapping with a providers section");
    };
    match root.get("providers") {
        Some(Value::Mapping(providers)) if !providers.is_empty() => {}
        _ => bail!("the config needs a providers mapping"),
    }
    check_references(&Value::Mapping(root.clone()))?;
    Ok(root)
}

fn check_references(value: &Value) -> Result<()> {
    match value {
        Value::String(text) => {
            reference_names(text)?;
        }
        Value::Sequence(items) => items.iter().try_for_each(check_references)?,
        Value::Mapping(map) => {
            for (key, value) in map {
                if key.as_str().is_some_and(|k| k.contains("${secret:")) {
                    bail!("secret references are not allowed in keys");
                }
                check_references(value)?;
            }
        }
        Value::Tagged(tagged) => check_references(&tagged.value)?,
        _ => {}
    }
    Ok(())
}

pub fn secret_names(text: &str) -> Result<BTreeSet<String>> {
    let root = parse(text)?;
    let mut names = BTreeSet::new();
    collect_names(&Value::Mapping(root), &mut names)?;
    Ok(names)
}

pub fn provider_secret_names(text: &str, provider: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(section) = parse(text).ok().and_then(|root| root.get("providers")?.get(provider).cloned()) {
        let _ = collect_names(&section, &mut names);
    }
    names
}

fn collect_names(value: &Value, names: &mut BTreeSet<String>) -> Result<()> {
    match value {
        Value::String(text) => names.extend(reference_names(text)?.into_iter().map(String::from)),
        Value::Sequence(items) => {
            for item in items {
                collect_names(item, names)?;
            }
        }
        Value::Mapping(map) => {
            for value in map.values() {
                collect_names(value, names)?;
            }
        }
        Value::Tagged(tagged) => collect_names(&tagged.value, names)?,
        _ => {}
    }
    Ok(())
}

fn sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase().replace(['-', '_'], "");
    SENSITIVE_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

fn mask_value(key: Option<&str>, parent: Option<&str>, text: &str) -> String {
    if text.contains("${secret:") {
        return text.to_string();
    }
    if parent.is_some_and(|p| p.eq_ignore_ascii_case("headers")) || key.is_some_and(sensitive_key) {
        return MASK.to_string();
    }
    if key.is_some_and(|k| k.to_ascii_lowercase().ends_with("url")) {
        return mask_url(text);
    }
    if looks_secret(text) {
        return mask_text(text);
    }
    text.to_string()
}

fn mask_tree(value: &mut Value, key: Option<&str>, parent: Option<&str>) {
    match value {
        Value::String(text) => *text = mask_value(key, parent, text),
        Value::Sequence(items) => items.iter_mut().for_each(|item| mask_tree(item, key, parent)),
        Value::Mapping(map) => {
            for (child_key, child) in map.iter_mut() {
                mask_tree(child, child_key.as_str(), key);
            }
        }
        Value::Tagged(tagged) => mask_tree(&mut tagged.value, key, parent),
        _ => {}
    }
}

pub fn masked(text: &str) -> Result<(String, bool)> {
    let root = parse(text)?;
    let mut value = Value::Mapping(root.clone());
    mask_tree(&mut value, None, None);
    if value == Value::Mapping(root) {
        return Ok((text.to_string(), false));
    }
    Ok((serde_yaml::to_string(&value)?, true))
}

fn endpoint_changed(map: &serde_yaml::Mapping, previous: &serde_yaml::Mapping, key: Option<&str>) -> bool {
    map.iter().any(|(child_key, child)| {
        let Some(name) = child_key.as_str().filter(|k| k.to_ascii_lowercase().ends_with("url")) else {
            return false;
        };
        let Some(text) = child.as_str() else {
            return false;
        };
        match previous.get(child_key).and_then(Value::as_str) {
            Some(old) => text != old && text != mask_value(Some(name), key, old),
            None => true,
        }
    })
}

fn holds_mask(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(MASK),
        Value::Sequence(items) => items.iter().any(holds_mask),
        Value::Mapping(map) => map.values().any(holds_mask),
        Value::Tagged(tagged) => holds_mask(&tagged.value),
        _ => false,
    }
}

fn restore_tree(new: &mut Value, old: &Value, key: Option<&str>, parent: Option<&str>) -> Result<()> {
    match (new, old) {
        (Value::String(text), Value::String(previous)) => {
            if text != previous && *text == mask_value(key, parent, previous) {
                *text = previous.clone();
            }
        }
        (Value::Sequence(items), Value::Sequence(previous)) => {
            for (item, prev) in items.iter_mut().zip(previous) {
                restore_tree(item, prev, key, parent)?;
            }
        }
        (Value::Mapping(map), Value::Mapping(previous)) => {
            if endpoint_changed(map, previous, key) && map.values().any(holds_mask) {
                bail!("this provider's address changed, so type its keys again instead of keeping the hidden ones");
            }
            for (child_key, child) in map.iter_mut() {
                if let Some(prev) = previous.get(child_key) {
                    restore_tree(child, prev, child_key.as_str(), key)?;
                }
            }
        }
        (Value::Tagged(tagged), Value::Tagged(previous)) => restore_tree(&mut tagged.value, &previous.value, key, parent)?,
        _ => {}
    }
    Ok(())
}

pub fn unmask(text: &str, previous: Option<&str>) -> Result<String> {
    let root = parse(text)?;
    let Some(previous) = previous.and_then(|p| parse(p).ok()) else {
        return Ok(text.to_string());
    };
    if !text.contains(MASK) {
        return Ok(text.to_string());
    }
    let mut value = Value::Mapping(root.clone());
    restore_tree(&mut value, &Value::Mapping(previous), None, None)?;
    if value == Value::Mapping(root) {
        return Ok(text.to_string());
    }
    let restored = serde_yaml::to_string(&value)?;
    parse(&restored)?;
    Ok(restored)
}

fn resolve_text(text: &str, environment: &BTreeMap<String, String>) -> Result<String> {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find("${secret:") {
        result.push_str(&remaining[..start]);
        let reference = &remaining[start + 9..];
        let end = reference.find('}').ok_or_else(|| anyhow!("invalid environment secret reference"))?;
        let name = &reference[..end];
        let value = environment
            .get(name)
            .ok_or_else(|| anyhow!("environment key {name} in the omp model config is not granted to this agent"))?;
        result.push_str(value);
        remaining = &reference[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

fn resolve_tree(value: &mut Value, environment: &BTreeMap<String, String>) -> Result<()> {
    match value {
        Value::String(text) if text.contains("${secret:") => *text = resolve_text(text, environment)?,
        Value::Sequence(items) => items.iter_mut().try_for_each(|item| resolve_tree(item, environment))?,
        Value::Mapping(map) => map.values_mut().try_for_each(|item| resolve_tree(item, environment))?,
        Value::Tagged(tagged) => resolve_tree(&mut tagged.value, environment)?,
        _ => {}
    }
    Ok(())
}

pub fn resolve(text: &str, environment: &BTreeMap<String, String>) -> Result<String> {
    let mut value = Value::Mapping(parse(text)?);
    resolve_tree(&mut value, environment)?;
    Ok(serde_yaml::to_string(&value)?)
}

pub fn resolve_available(text: &str, environment: &BTreeMap<String, String>) -> Result<(String, BTreeSet<String>)> {
    let root = parse(text)?;
    let mut missing = BTreeSet::new();
    let mut resolved = Mapping::new();
    for (key, value) in root {
        let entry = match (key.as_str(), value) {
            (Some("providers"), Value::Mapping(providers)) => {
                let mut kept = Mapping::new();
                for (name, mut provider) in providers {
                    match resolve_tree(&mut provider, environment) {
                        Ok(()) => {
                            kept.insert(name, provider);
                        }
                        Err(_) => collect_names(&provider, &mut missing)?,
                    }
                }
                Value::Mapping(kept)
            }
            (_, mut value) => match resolve_tree(&mut value, environment) {
                Ok(()) => value,
                Err(_) => {
                    collect_names(&value, &mut missing)?;
                    continue;
                }
            },
        };
        resolved.insert(key, entry);
    }
    missing.retain(|name| !environment.contains_key(name));
    Ok((serde_yaml::to_string(&resolved)?, missing))
}

pub fn merge(base: Option<&str>, custom: &str) -> Result<String> {
    let custom: Mapping = serde_yaml::from_str(custom)?;
    let mut merged = base
        .and_then(|text| serde_yaml::from_str::<Value>(text).ok())
        .and_then(|value| match value {
            Value::Mapping(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    for (key, value) in custom {
        match (merged.get_mut(&key), &value, key.as_str()) {
            (Some(Value::Mapping(existing)), Value::Mapping(incoming), Some("providers")) => {
                for (name, provider) in incoming {
                    existing.insert(name.clone(), provider.clone());
                }
            }
            _ => {
                merged.insert(key, value);
            }
        }
    }
    Ok(serde_yaml::to_string(&merged)?)
}

pub fn models(text: &str) -> Vec<OmpModel> {
    let Ok(root) = parse(text) else {
        return Vec::new();
    };
    let Some(Value::Mapping(providers)) = root.get("providers") else {
        return Vec::new();
    };
    providers
        .iter()
        .filter_map(|(name, provider)| Some((name.as_str()?, provider.get("models")?.as_sequence()?)))
        .flat_map(|(provider, models)| {
            models.iter().filter_map(move |model| {
                let id = model.get("id").and_then(Value::as_str)?;
                Some(OmpModel {
                    id: format!("{provider}/{id}"),
                    name: model.get("name").and_then(Value::as_str).map(String::from),
                })
            })
        })
        .collect()
}

impl WorldStore {
    pub async fn omp_config(&self, agent_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT config FROM agent_omp_configs WHERE agent_id = $1")
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn set_omp_config(&self, agent_id: Uuid, config: Option<&str>) -> Result<()> {
        match config {
            Some(config) => {
                parse(config)?;
                sqlx::query(
                    "INSERT INTO agent_omp_configs (agent_id, config) VALUES ($1, $2)
                     ON CONFLICT (agent_id) DO UPDATE SET config = EXCLUDED.config, updated_at = NOW()",
                )
                .bind(agent_id)
                .bind(config)
                .execute(self.pool())
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM agent_omp_configs WHERE agent_id = $1")
                    .bind(agent_id)
                    .execute(self.pool())
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn resolved_omp_config(&self, agent_id: Uuid, backend: &str) -> Result<Option<ResolvedOmpConfig>> {
        if backend != "omp" {
            return Ok(None);
        }
        let Some(config) = self.omp_config(agent_id).await? else {
            return Ok(None);
        };
        let names = secret_names(&config)?;
        let environment = if names.is_empty() {
            BTreeMap::new()
        } else {
            self.environment_for_agent(agent_id).await?
        };
        let secrets = names.iter().filter_map(|name| environment.get(name).cloned()).collect();
        let (models_yml, missing) = resolve_available(&config, &environment)?;
        if !missing.is_empty() {
            tracing::warn!(%agent_id, missing = ?missing, "Skipping omp providers whose environment keys are not granted");
        }
        Ok(Some(ResolvedOmpConfig { models_yml, secrets }))
    }
}

pub struct ResolvedOmpConfig {
    pub models_yml: String,
    pub secrets: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "providers:\n  mine:\n    baseUrl: https://llm.example.com/v1\n    apiKey: ${secret:MINE_KEY}\n    api: openai-completions\n    headers:\n      X-Team: Bearer ${secret:TEAM_TOKEN}\n    models:\n      - id: big-1\n        name: Big One\n      - id: small-1\n";

    #[test]
    fn detects_secret_names() {
        let names: Vec<_> = secret_names(CONFIG).unwrap().into_iter().collect();
        assert_eq!(names, vec!["MINE_KEY", "TEAM_TOKEN"]);
    }

    #[test]
    fn rejects_bad_configs() {
        assert!(parse("not: [valid").is_err());
        assert!(parse("- a\n- b\n").is_err());
        assert!(parse("other: 1\n").is_err());
        assert!(parse("providers:\n  a:\n    apiKey: ${secret:lower}\n").is_err());
        assert!(parse("providers:\n  a:\n    apiKey: ${secret:WORLD_X}\n").is_err());
        assert!(parse("providers:\n  ${secret:KEY}: 1\n").is_err());
        assert!(parse(&format!("providers:\n  a: \"{}\"\n", "x".repeat(MAX_CONFIG_LEN))).is_err());
    }

    #[test]
    fn resolves_references_without_breaking_yaml() {
        let environment = BTreeMap::from([
            ("MINE_KEY".to_string(), "sk-\"quoted\"\nnext: line".to_string()),
            ("TEAM_TOKEN".to_string(), "tok".to_string()),
        ]);
        let resolved = resolve(CONFIG, &environment).unwrap();
        let value: Value = serde_yaml::from_str(&resolved).unwrap();
        assert_eq!(value["providers"]["mine"]["apiKey"].as_str(), Some("sk-\"quoted\"\nnext: line"));
        assert_eq!(value["providers"]["mine"]["headers"]["X-Team"].as_str(), Some("Bearer tok"));
        assert!(value.get("next").is_none());
        assert!(resolve(CONFIG, &BTreeMap::new()).is_err());
    }

    #[test]
    fn drops_only_providers_with_missing_keys() {
        let config = format!("{CONFIG}  open:\n    baseUrl: https://open\n    models:\n      - id: free\n");
        let environment = BTreeMap::from([("MINE_KEY".to_string(), "k".to_string())]);
        let (text, missing) = resolve_available(&config, &environment).unwrap();
        let value: Value = serde_yaml::from_str(&text).unwrap();
        assert!(value["providers"].get("mine").is_none());
        assert!(value["providers"]["open"].is_mapping());
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), vec!["TEAM_TOKEN"]);
    }

    fn value_at(text: &str, key: &str) -> String {
        let value: Value = serde_yaml::from_str(text).unwrap();
        value["providers"]["mine"][key].as_str().unwrap().to_string()
    }

    #[test]
    fn masks_raw_values_but_shows_references() {
        let (text, was_masked) = masked(CONFIG).unwrap();
        assert!(!was_masked);
        assert_eq!(text, CONFIG);
        let raw = "providers:\n  mine:\n    baseUrl: https://u:p@llm.example.com/v1?key=abc\n    apiKey: sk-live-1234567890abcdef\n    headers:\n      Authorization: Bearer abc\n      X-Ref: ${secret:REF_KEY}\n    models:\n      - id: big-1\n";
        let (text, was_masked) = masked(raw).unwrap();
        assert!(was_masked);
        assert!(!text.contains("sk-live"));
        assert!(!text.contains("Bearer abc"));
        assert!(!text.contains("u:p@"));
        assert!(text.contains("${secret:REF_KEY}"));
        assert!(text.contains("big-1"));
        let restored = unmask(&text.replace("big-1", "big-2"), Some(raw)).unwrap();
        let value: Value = serde_yaml::from_str(&restored).unwrap();
        assert_eq!(value["providers"]["mine"]["apiKey"].as_str(), Some("sk-live-1234567890abcdef"));
        assert_eq!(value["providers"]["mine"]["headers"]["Authorization"].as_str(), Some("Bearer abc"));
        assert_eq!(value["providers"]["mine"]["baseUrl"].as_str(), Some("https://u:p@llm.example.com/v1?key=abc"));
        assert_eq!(value["providers"]["mine"]["models"][0]["id"].as_str(), Some("big-2"));
        let replaced = unmask(&text.replace("${secret:REF_KEY}", "${secret:OTHER_KEY}"), Some(raw)).unwrap();
        assert!(replaced.contains("OTHER_KEY"));
        let masked_url = value_at(&text, "baseUrl");
        let moved = text.replace(&masked_url, "https://collector.example.net/v1");
        assert!(unmask(&moved, Some(raw)).is_err(), "a hidden key never follows a provider to a new address");
        let retyped = moved.replace("apiKey: '***'", "apiKey: sk-new").replace("apiKey: \"***\"", "apiKey: sk-new").replace("Authorization: '***'", "Authorization: Bearer new").replace("Authorization: \"***\"", "Authorization: Bearer new");
        assert!(!retyped.contains(MASK), "{retyped}");
        assert!(unmask(&retyped, Some(raw)).unwrap().contains("collector.example.net"));
    }

    #[test]
    fn merges_providers_over_the_default() {
        let base = include_str!("../omp-models.yml");
        let custom = "providers:\n  fal:\n    baseUrl: https://override\n    models: []\n  mine:\n    baseUrl: https://x\n    models:\n      - id: m\n";
        let merged: Value = serde_yaml::from_str(&merge(Some(base), custom).unwrap()).unwrap();
        assert_eq!(merged["providers"]["fal"]["baseUrl"].as_str(), Some("https://override"));
        assert_eq!(merged["providers"]["mine"]["baseUrl"].as_str(), Some("https://x"));
        let alone: Value = serde_yaml::from_str(&merge(None, custom).unwrap()).unwrap();
        assert!(alone["providers"]["mine"].is_mapping());
    }

    #[tokio::test]
    async fn postgres_omp_config_resolves_granted_keys() -> Result<()> {
        use std::str::FromStr;

        use base64::{engine::general_purpose::STANDARD, Engine};
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let Ok(database_url) = std::env::var("OMP_CONFIG_TEST_DATABASE_URL") else {
            return Ok(());
        };
        const CHILD: &str = "OMP_CONFIG_DATABASE_TEST_CHILD";
        if std::env::var(CHILD).is_err() {
            let status = std::process::Command::new(std::env::current_exe()?)
                .arg("--exact")
                .arg("omp_config::tests::postgres_omp_config_resolves_granted_keys")
                .env(CHILD, "1")
                .env("OMP_CONFIG_TEST_DATABASE_URL", &database_url)
                .env("WORLD_SECRETS_KEY", STANDARD.encode([7u8; 32]))
                .status()?;
            assert!(status.success());
            return Ok(());
        }
        crate::environment::initialize();
        let admin = PgPoolOptions::new().max_connections(1).connect(&database_url).await?;
        let schema = format!("omp_config_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await?;
        let options = PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new().max_connections(2).connect_with(options).await?;
        let result: Result<()> = async {
            sqlx::raw_sql(
                "CREATE TABLE users (id UUID PRIMARY KEY);
                 CREATE TABLE agent_profiles (
                   id UUID PRIMARY KEY,
                   owner_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
                   visibility TEXT NOT NULL
                 );",
            )
            .execute(&pool)
            .await?;
            sqlx::raw_sql(include_str!("../../migrations/040_world_environment.sql")).execute(&pool).await?;
            sqlx::raw_sql(include_str!("../../migrations/096_shared_keys.sql")).execute(&pool).await?;
            sqlx::raw_sql(include_str!("../../migrations/076_agent_omp_configs.sql")).execute(&pool).await?;
            sqlx::raw_sql(crate::orgs::ORG_COLUMNS_FOR_TESTS).execute(&pool).await?;
            sqlx::raw_sql(include_str!("../../migrations/126_key_environments.sql")).execute(&pool).await?;
            let owner = Uuid::new_v4();
            let agent = Uuid::new_v4();
            sqlx::query("INSERT INTO users (id) VALUES ($1)").bind(owner).execute(&pool).await?;
            sqlx::query("INSERT INTO agent_profiles (id, owner_user_id, visibility) VALUES ($1, $2, 'private')")
                .bind(agent)
                .bind(owner)
                .execute(&pool)
                .await?;
            let store = WorldStore::new(pool.clone());
            assert!(store.resolved_omp_config(agent, "omp").await?.is_none());
            assert!(store.set_omp_config(agent, Some("nope: [")).await.is_err());
            store.set_omp_config(agent, Some(CONFIG)).await?;
            assert!(store.resolved_omp_config(agent, "claude").await?.is_none());
            let skipped = store.resolved_omp_config(agent, "omp").await?.unwrap();
            assert!(!skipped.models_yml.contains("mine"));
            assert!(skipped.secrets.is_empty());
            store.save_environment(owner, "MINE_KEY", "mine-secret").await?;
            store.save_environment(owner, "TEAM_TOKEN", "team-secret").await?;
            store.set_environment_agents(owner, "MINE_KEY", &[agent]).await?;
            store.set_environment_agents(owner, "TEAM_TOKEN", &[agent]).await?;
            let resolved = store.resolved_omp_config(agent, "omp").await?.unwrap();
            assert!(resolved.models_yml.contains("mine-secret"));
            assert!(resolved.models_yml.contains("Bearer team-secret"));
            assert_eq!(resolved.secrets.len(), 2);
            assert_eq!(store.omp_config(agent).await?.as_deref(), Some(CONFIG));
            sqlx::query("UPDATE agent_profiles SET visibility = 'public' WHERE id = $1").bind(agent).execute(&pool).await?;
            assert_eq!(store.resolved_omp_config(agent, "omp").await?.unwrap().secrets.len(), 2);
            store.set_omp_config(agent, None).await?;
            assert!(store.omp_config(agent).await?.is_none());
            Ok(())
        }
        .await;
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&admin).await?;
        result
    }

    #[test]
    fn lists_custom_models() {
        let ids: Vec<_> = models(CONFIG).into_iter().map(|m| (m.id, m.name)).collect();
        assert_eq!(
            ids,
            vec![("mine/big-1".to_string(), Some("Big One".to_string())), ("mine/small-1".to_string(), None)]
        );
    }
}
