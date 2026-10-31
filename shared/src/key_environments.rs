use std::collections::BTreeSet;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::environment::{encrypt, master_key, open, valid_name, valid_value};
use crate::world::WorldStore;

pub const DEFAULT: &str = "default";
pub const MAX_ENVIRONMENTS_PER_KEY: i64 = 12;
pub const MAX_HOURS: i64 = 24 * 30;

pub fn valid_environment(label: &str) -> bool {
    let mut chars = label.chars();
    label != DEFAULT
        && label.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn normalize_environment(label: &str) -> String {
    label.trim().to_ascii_lowercase().replace([' ', '_'], "-")
}

pub(crate) fn variant_aad_name(name: &str, environment: &str) -> String {
    format!("{name}@{environment}")
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Variant {
    pub name: String,
    pub environment: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Choice {
    pub agent_id: Uuid,
    pub name: String,
    pub environment: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub chosen_by: String,
    pub chosen_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChoiceError {
    NotYourAgent,
    NoSuchEnvironment,
    BadRequest,
}

impl std::fmt::Display for ChoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ChoiceError::NotYourAgent => "that agent is not yours",
            ChoiceError::NoSuchEnvironment => "there is no value for that key in that environment",
            ChoiceError::BadRequest => "invalid key environment request",
        })
    }
}

impl std::error::Error for ChoiceError {}

pub const ACTIVE_CHOICE: &str = "(c.expires_at IS NULL OR c.expires_at > NOW())";

impl WorldStore {
    pub async fn save_environment_variant(&self, owner: Uuid, name: &str, environment: &str, value: &str) -> Result<bool> {
        if !valid_name(name) || !valid_environment(environment) || !valid_value(value) {
            return Err(anyhow!("invalid environment entry"));
        }
        let key = master_key()?;
        let (ciphertext, nonce) = encrypt(key, owner, &variant_aad_name(name, environment), value.as_bytes())?;
        let mut tx = self.pool().begin().await?;
        let base: Option<i32> = sqlx::query_scalar("SELECT 1 FROM world_environment WHERE owner_user_id = $1 AND name = $2 FOR UPDATE")
            .bind(owner)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
        if base.is_none() {
            return Ok(false);
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_environment_variants WHERE owner_user_id = $1 AND name = $2 AND environment <> $3",
        )
        .bind(owner)
        .bind(name)
        .bind(environment)
        .fetch_one(&mut *tx)
        .await?;
        if count >= MAX_ENVIRONMENTS_PER_KEY {
            return Err(anyhow!("a key can have at most {MAX_ENVIRONMENTS_PER_KEY} environments"));
        }
        sqlx::query(
            "INSERT INTO world_environment_variants (owner_user_id, name, environment, ciphertext, nonce)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (owner_user_id, name, environment) DO UPDATE
             SET ciphertext = EXCLUDED.ciphertext, nonce = EXCLUDED.nonce, updated_at = NOW()",
        )
        .bind(owner)
        .bind(name)
        .bind(environment)
        .bind(ciphertext)
        .bind(nonce)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn delete_environment_variant(&self, owner: Uuid, name: &str, environment: &str) -> Result<bool> {
        if !valid_name(name) || !valid_environment(environment) {
            return Err(anyhow!("invalid environment entry"));
        }
        Ok(sqlx::query("DELETE FROM world_environment_variants WHERE owner_user_id = $1 AND name = $2 AND environment = $3")
            .bind(owner)
            .bind(name)
            .bind(environment)
            .execute(self.pool())
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn list_environment_variants(&self, owner: Uuid) -> Result<Vec<Variant>> {
        Ok(sqlx::query_as::<_, Variant>(
            "SELECT name, environment, updated_at FROM world_environment_variants WHERE owner_user_id = $1 ORDER BY name, environment",
        )
        .bind(owner)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_environment_choices(&self, owner: Uuid) -> Result<Vec<Choice>> {
        Ok(sqlx::query_as::<_, Choice>(&format!(
            "SELECT c.agent_id, c.name, c.environment, c.expires_at, c.chosen_by, c.chosen_at
             FROM world_environment_choices c
             JOIN agent_profiles a ON a.id = c.agent_id AND a.owner_user_id = c.owner_user_id
             WHERE c.owner_user_id = $1 AND {ACTIVE_CHOICE}
             ORDER BY c.agent_id, c.name"
        ))
        .bind(owner)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn choose_environment(
        &self,
        owner: Uuid,
        agent_id: Uuid,
        name: Option<&str>,
        environment: &str,
        hours: Option<i64>,
        by: &str,
    ) -> Result<std::result::Result<Vec<String>, ChoiceError>> {
        let environment = normalize_environment(environment);
        if name.is_some_and(|name| !valid_name(name))
            || (environment != DEFAULT && !valid_environment(&environment))
            || hours.is_some_and(|hours| !(1..=MAX_HOURS).contains(&hours))
            || by.is_empty()
            || by.len() > 80
        {
            return Ok(Err(ChoiceError::BadRequest));
        }
        let mut tx = self.pool().begin().await?;
        let mine: Option<i32> = sqlx::query_scalar("SELECT 1 FROM agent_profiles WHERE id = $1 AND owner_user_id = $2 FOR UPDATE")
            .bind(agent_id)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await?;
        if mine.is_none() {
            return Ok(Err(ChoiceError::NotYourAgent));
        }
        if environment == DEFAULT {
            let names: Vec<String> = sqlx::query_scalar(
                "DELETE FROM world_environment_choices WHERE agent_id = $1 AND owner_user_id = $2 AND ($3::text IS NULL OR name = $3) RETURNING name",
            )
            .bind(agent_id)
            .bind(owner)
            .bind(name)
            .fetch_all(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Ok(names.into_iter().collect::<BTreeSet<_>>().into_iter().collect()));
        }
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM world_environment_variants
             WHERE owner_user_id = $1 AND environment = $2 AND ($3::text IS NULL OR name = $3)
             ORDER BY name FOR SHARE",
        )
        .bind(owner)
        .bind(&environment)
        .bind(name)
        .fetch_all(&mut *tx)
        .await?;
        if names.is_empty() {
            return Ok(Err(ChoiceError::NoSuchEnvironment));
        }
        sqlx::query(
            "INSERT INTO world_environment_choices (agent_id, owner_user_id, name, environment, expires_at, chosen_by)
             SELECT $1, $2, name, $3, CASE WHEN $4::bigint IS NULL THEN NULL ELSE NOW() + make_interval(hours => $4::int) END, $5
             FROM UNNEST($6::text[]) AS name
             ON CONFLICT (agent_id, name) DO UPDATE
             SET environment = EXCLUDED.environment, expires_at = EXCLUDED.expires_at,
                 chosen_by = EXCLUDED.chosen_by, chosen_at = NOW(), owner_user_id = EXCLUDED.owner_user_id",
        )
        .bind(agent_id)
        .bind(owner)
        .bind(&environment)
        .bind(hours)
        .bind(by)
        .bind(&names)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(names))
    }

    pub async fn environment_value_for_agent(&self, owner: Uuid, agent_id: Uuid, name: &str) -> Result<Option<String>> {
        let row: Option<(String, Vec<u8>, Vec<u8>)> = sqlx::query_as(&format!(
            "SELECT CASE WHEN v.environment IS NULL THEN e.name ELSE e.name || '@' || v.environment END,
                    COALESCE(v.ciphertext, e.ciphertext), COALESCE(v.nonce, e.nonce)
             FROM world_environment e
             LEFT JOIN world_environment_choices c
               ON c.agent_id = $2 AND c.owner_user_id = e.owner_user_id AND c.name = e.name AND {ACTIVE_CHOICE}
             LEFT JOIN world_environment_variants v
               ON v.owner_user_id = c.owner_user_id AND v.name = c.name AND v.environment = c.environment
             WHERE e.owner_user_id = $1 AND e.name = $3"
        ))
        .bind(owner)
        .bind(agent_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        let Some((aad_name, ciphertext, nonce)) = row else {
            return Ok(None);
        };
        let plaintext = open(owner, &aad_name, &ciphertext, &nonce)?;
        let value = String::from_utf8(plaintext).map_err(|_| anyhow!("environment decryption failed"))?;
        Ok(Some(value.trim().to_string()).filter(|v| !v.is_empty()))
    }
}

pub fn key_of(aad_name: &str) -> &str {
    aad_name.split_once('@').map_or(aad_name, |(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_labels_are_simple_and_never_default() {
        assert!(valid_environment("staging"));
        assert!(valid_environment("acct-2"));
        assert!(!valid_environment("default"));
        assert!(!valid_environment("Staging"));
        assert!(!valid_environment("2nd"));
        assert!(!valid_environment("a@b"));
        assert!(!valid_environment(""));
        assert_eq!(normalize_environment(" Work Account "), "work-account");
        assert_eq!(key_of("ANTHROPIC_API_KEY@staging"), "ANTHROPIC_API_KEY");
        assert_eq!(key_of("ANTHROPIC_API_KEY"), "ANTHROPIC_API_KEY");
    }

    #[tokio::test]
    async fn agents_switch_between_key_environments() -> Result<()> {
        let Ok(url) = std::env::var("KEYS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        crate::environment::initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut people = Vec::new();
        for who in ["owner", "other"] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
                .bind(format!("{who}-{tag}@test.dev"))
                .fetch_one(&pool)
                .await?;
            crate::orgs::join_default_for_tests(&pool, id, None).await?;
            people.push(id);
        }
        let (owner, other) = (people[0], people[1]);
        let mut agents = Vec::new();
        for (who, person) in [("mine", owner), ("theirs", other)] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility) VALUES ($1, $1, 'claude', 'opus', $2, 'private') RETURNING id",
            )
            .bind(format!("{who}-{tag}"))
            .bind(person)
            .fetch_one(&pool)
            .await?;
            agents.push(id);
        }
        let (mine, theirs) = (agents[0], agents[1]);
        store.save_environment(owner, "ANTHROPIC_API_KEY", "sk-ant-api03-default-value").await?;
        store.save_environment(owner, "DEPLOY_TOKEN", "deploy-default").await?;
        store.set_environment_agents(owner, "DEPLOY_TOKEN", &[mine]).await?;
        assert!(!store.save_environment_variant(owner, "MISSING_KEY", "staging", "x-value").await?);
        assert!(store.save_environment_variant(owner, "ANTHROPIC_API_KEY", "staging", "sk-ant-api03-staging-value").await?);
        assert!(store.save_environment_variant(owner, "DEPLOY_TOKEN", "staging", "deploy-staging").await?);

        let before = store.key_version(mine).await?;
        assert_eq!(store.provider_keys_for_agent(mine).await?.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-ant-api03-default-value"));
        assert_eq!(store.choose_environment(owner, theirs, None, "staging", None, "person").await?, Err(ChoiceError::NotYourAgent));
        assert_eq!(store.choose_environment(owner, mine, None, "prod", None, "person").await?, Err(ChoiceError::NoSuchEnvironment));
        let switched = store.choose_environment(owner, mine, None, "Staging", Some(2), "agent:main").await?.unwrap();
        assert_eq!(switched, vec!["ANTHROPIC_API_KEY".to_string(), "DEPLOY_TOKEN".to_string()]);
        assert_ne!(store.key_version(mine).await?, before);
        assert_eq!(store.provider_keys_for_agent(mine).await?.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-ant-api03-staging-value"));
        assert_eq!(store.environment_for_agent(mine).await?.get("DEPLOY_TOKEN").map(String::as_str), Some("deploy-staging"));
        assert!(store.environment_for_agent(theirs).await?.is_empty());
        assert_eq!(store.list_environment_choices(owner).await?.len(), 2);

        sqlx::query("UPDATE world_environment_choices SET expires_at = NOW() - INTERVAL '1 minute' WHERE agent_id = $1 AND name = 'DEPLOY_TOKEN'")
            .bind(mine)
            .execute(&pool)
            .await?;
        assert_eq!(store.environment_for_agent(mine).await?.get("DEPLOY_TOKEN").map(String::as_str), Some("deploy-default"));

        let swapped: (Vec<u8>, Vec<u8>) = sqlx::query_as(
            "SELECT ciphertext, nonce FROM world_environment_variants WHERE owner_user_id = $1 AND name = 'ANTHROPIC_API_KEY'",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE world_environment_variants SET ciphertext = $2, nonce = $3 WHERE owner_user_id = $1 AND name = 'DEPLOY_TOKEN'")
            .bind(owner)
            .bind(&swapped.0)
            .bind(&swapped.1)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE world_environment_choices SET expires_at = NULL WHERE agent_id = $1")
            .bind(mine)
            .execute(&pool)
            .await?;
        assert!(store.environment_for_agent(mine).await.is_err(), "a value copied onto another key must not decrypt");

        assert_eq!(store.choose_environment(owner, mine, Some("ANTHROPIC_API_KEY"), "default", None, "person").await?.unwrap(), vec!["ANTHROPIC_API_KEY".to_string()]);
        assert_eq!(store.provider_keys_for_agent(mine).await?.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-ant-api03-default-value"));
        assert!(store.delete_environment_variant(owner, "DEPLOY_TOKEN", "staging").await?);
        assert!(store.list_environment_choices(owner).await?.is_empty());
        sqlx::query("UPDATE agent_profiles SET owner_user_id = $2 WHERE id = $1").bind(mine).bind(other).execute(&pool).await?;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_environment_choices WHERE agent_id = $1").bind(mine).fetch_one(&pool).await?;
        assert_eq!(left, 0);
        Ok(())
    }
}
