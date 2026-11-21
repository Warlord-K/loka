use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    sync::OnceLock,
};

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::WorldStore;

const NONCE_LEN: usize = 12;
const AAD_PREFIX: &[u8] = b"world-environment:v1";
const MAX_NAME_LEN: usize = 128;
const MAX_VALUE_LEN: usize = 64 * 1024;

static MASTER_KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
static PREVIOUS_KEYS: OnceLock<Vec<[u8; 32]>> = OnceLock::new();

#[derive(Clone, FromRow, Serialize)]
pub struct EnvironmentEntry {
    pub name: String,
    pub updated_at: DateTime<Utc>,
    pub agent_ids: Vec<Uuid>,
    pub vault: bool,
}

#[derive(Clone, FromRow, Serialize)]
pub struct SharedKey {
    pub name: String,
    pub note: String,
    pub updated_at: DateTime<Utc>,
    pub agent_ids: Vec<Uuid>,
}

const SHARED_OWNER: Uuid = Uuid::nil();

#[derive(FromRow)]
struct EncryptedEntry {
    owner_user_id: Uuid,
    name: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
}

#[derive(FromRow)]
struct GrantedEntry {
    owner_user_id: Uuid,
    name: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    vault: bool,
}

#[derive(Debug)]
pub struct InvalidEnvironmentGrant;

impl fmt::Display for InvalidEnvironmentGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid environment agent grant")
    }
}

impl Error for InvalidEnvironmentGrant {}

pub fn configured() -> bool {
    MASTER_KEY.get().and_then(Option::as_ref).is_some()
}

pub fn initialize() {
    let value = std::env::var("WORLD_SECRETS_KEY").ok();
    std::env::remove_var("WORLD_SECRETS_KEY");
    let previous = std::env::var("WORLD_SECRETS_KEY_PREVIOUS").ok();
    std::env::remove_var("WORLD_SECRETS_KEY_PREVIOUS");
    let key = master_key_from(value.as_deref()).ok();
    let _ = MASTER_KEY.set(key);
    let _ = PREVIOUS_KEYS.set(previous_keys_from(previous.as_deref()));
}

#[cfg(test)]
pub(crate) fn initialize_for_tests() {
    let _ = MASTER_KEY.set(Some([7u8; 32]));
    let _ = PREVIOUS_KEYS.set(Vec::new());
}

fn previous_keys_from(value: Option<&str>) -> Vec<[u8; 32]> {
    value
        .unwrap_or_default()
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| master_key_from(Some(part)).ok())
        .collect()
}

pub fn rotating() -> bool {
    configured() && PREVIOUS_KEYS.get().is_some_and(|keys| !keys.is_empty())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rotation {
    pub current: usize,
    pub rotated: usize,
    pub unreadable: usize,
}

#[derive(FromRow)]
struct StoredSecret {
    table_key: String,
    aad_owner: Uuid,
    name: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
}

fn open_with_keys(current: &[u8; 32], previous: &[[u8; 32]], owner: Uuid, name: &str, ciphertext: &[u8], nonce: &[u8]) -> Option<(Vec<u8>, bool)> {
    if let Ok(plain) = decrypt(current, owner, name, ciphertext, nonce) {
        return Some((plain, false));
    }
    previous
        .iter()
        .find_map(|key| decrypt(key, owner, name, ciphertext, nonce).ok())
        .map(|plain| (plain, true))
}

pub(crate) fn open(owner: Uuid, name: &str, ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
    let previous = PREVIOUS_KEYS.get().map(Vec::as_slice).unwrap_or_default();
    open_with_keys(master_key()?, previous, owner, name, ciphertext, nonce)
        .map(|(plain, _)| plain)
        .ok_or_else(|| anyhow!("environment decryption failed"))
}

pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    let mut chars = name.bytes();
    if !matches!(chars.next(), Some(b'A'..=b'Z' | b'_'))
        || !chars.all(|c| matches!(c, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
    {
        return false;
    }
    if crate::loka_env::reserved(name) {
        return false;
    }
    if matches!(name, "DATABASE_URL") {
        return true;
    }
    !matches!(
        name,
        "PATH"
            | "HOME"
            | "SHELL"
            | "USER"
            | "LOGNAME"
            | "PWD"
            | "OLDPWD"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "TERM"
            | "ENV"
            | "BASH_ENV"
            | "BASHOPTS"
            | "SHELLOPTS"
            | "CDPATH"
            | "GLOBIGNORE"
            | "IFS"
            | "PROMPT_COMMAND"
            | "PS4"
            | "ZDOTDIR"
            | "CODEX_HOME"
            | "CLAUDE_CONFIG_DIR"
            | "NODE_OPTIONS"
            | "NODE_PATH"
            | "PYTHONPATH"
            | "PYTHONHOME"
            | "PYTHONSTARTUP"
            | "RUBYOPT"
            | "RUBYLIB"
            | "PERL5OPT"
            | "PERL5LIB"
            | "JAVA_TOOL_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "_JAVA_OPTIONS"
            | "RUSTC_WRAPPER"
            | "RUSTDOCFLAGS"
            | "REDIS_URL"
            | "CONFIG_PATH"
    ) && !name.starts_with("LD_")
        && !name.starts_with("DYLD_")
        && !name.starts_with("GIT_")
}

pub fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_VALUE_LEN
        && !value.contains('\0')
        && (!is_vault_reference(value) || valid_vault_reference(value))
}

pub const VAULT_PREFIX: &str = "op://";

const MAX_VAULT_REFERENCE_LEN: usize = 512;

const VAULT_QUERIES: &[&str] = &["attribute", "ssh-format"];

pub fn is_vault_reference(value: &str) -> bool {
    value.trim().starts_with(VAULT_PREFIX)
}

pub fn valid_vault_reference(value: &str) -> bool {
    if value.len() > MAX_VAULT_REFERENCE_LEN || value != value.trim() {
        return false;
    }
    let Some(rest) = value.strip_prefix(VAULT_PREFIX) else {
        return false;
    };
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let part_ok = |part: &str| {
        !part.trim().is_empty()
            && part != "."
            && part != ".."
            && !part.starts_with('-')
            && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    };
    let parts: Vec<&str> = path.split('/').collect();
    if !(3..=4).contains(&parts.len()) || !parts.iter().all(|part| part_ok(part)) {
        return false;
    }
    query.is_none_or(|query| {
        query.split_once('=').is_some_and(|(key, value)| {
            VAULT_QUERIES.contains(&key) && !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    })
}

pub struct AgentKeys {
    pub values: BTreeMap<String, String>,
    pub vault_refs: BTreeMap<String, String>,
}

pub fn validate_mcp_definition(definition: &Value) -> Result<()> {
    let object = definition
        .as_object()
        .ok_or_else(|| anyhow!("MCP definition must be an object"))?;
    for (field, value) in object {
        if field.contains("${secret:") {
            bail!("MCP secret references are only allowed in env and headers");
        }
        match field.as_str() {
            "env" | "headers" => {
                let values = value
                    .as_object()
                    .ok_or_else(|| anyhow!("MCP {field} must be an object"))?;
                if values.keys().any(|key| key.contains("${secret:")) {
                    bail!("MCP secret references are not allowed in {field} names");
                }
                for value in values.values() {
                    let text = value
                        .as_str()
                        .ok_or_else(|| anyhow!("MCP {field} values must be strings"))?;
                    reference_names(text)?;
                }
            }
            "shared" if !value.is_boolean() => bail!("MCP shared must be true or false"),
            _ if contains_secret_reference(value) => {
                bail!("MCP secret references are only allowed in env and headers")
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn secret_reference_names(definition: &Value) -> Result<BTreeSet<String>> {
    if !contains_secret_reference(definition) {
        return Ok(BTreeSet::new());
    }
    validate_mcp_definition(definition)?;
    let mut names = BTreeSet::new();
    let Some(object) = definition.as_object() else {
        return Ok(names);
    };
    for field in ["env", "headers"] {
        if let Some(values) = object.get(field).and_then(Value::as_object) {
            for value in values.values().filter_map(Value::as_str) {
                names.extend(reference_names(value)?.into_iter().map(String::from));
            }
        }
    }
    Ok(names)
}

pub const MASKED_VALUE: &str = "***";
pub const SLACK_KEY_VAR: &str = "WORLD_SLACK_CONNECTION_KEY";

pub fn is_mac_slack_server(definition: &Value) -> bool {
    let named = |text: &str| text.ends_with("slack_mcp.py");
    definition.get("command").and_then(Value::as_str).is_some_and(named)
        || definition.get("args").and_then(Value::as_array).is_some_and(|args| args.iter().filter_map(Value::as_str).any(named))
}

pub fn runnable_definition(mut definition: Value) -> Value {
    let slack = is_mac_slack_server(&definition);
    for field in ["env", "headers"] {
        if let Some(values) = definition.get_mut(field).and_then(Value::as_object_mut) {
            values.retain(|name, value| value.as_str() != Some(MASKED_VALUE) && !(slack && field == "env" && name == SLACK_KEY_VAR));
        }
    }
    definition
}

pub fn without_secret_references<'a>(
    mut environment: BTreeMap<String, String>,
    definitions: impl IntoIterator<Item = &'a Value>,
) -> BTreeMap<String, String> {
    for definition in definitions {
        for name in secret_reference_names(definition).unwrap_or_default() {
            environment.remove(&name);
        }
    }
    environment
}

pub fn missing_secrets(definition: &Value, environment: &BTreeMap<String, String>) -> Result<Vec<String>> {
    Ok(secret_reference_names(definition)?.into_iter().filter(|name| !environment.contains_key(name)).collect())
}

fn contains_secret_reference(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("${secret:"),
        Value::Array(values) => values.iter().any(contains_secret_reference),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.contains("${secret:") || contains_secret_reference(value)),
        _ => false,
    }
}

pub(crate) fn reference_names(mut text: &str) -> Result<Vec<&str>> {
    let mut names = Vec::new();
    while let Some(start) = text.find("${secret:") {
        let reference = &text[start + 9..];
        let end = reference
            .find('}')
            .ok_or_else(|| anyhow!("invalid environment secret reference"))?;
        let name = &reference[..end];
        if !valid_name(name) {
            bail!("invalid environment key {name}");
        }
        names.push(name);
        text = &reference[end + 1..];
    }
    Ok(names)
}

impl WorldStore {
    pub async fn list_environment(&self, owner: Uuid) -> Result<Vec<EnvironmentEntry>> {
        Ok(sqlx::query_as::<_, EnvironmentEntry>(
            "SELECT e.name, e.updated_at,
                    COALESCE(array_agg(g.agent_id ORDER BY g.agent_id)
                      FILTER (WHERE g.agent_id IS NOT NULL), ARRAY[]::uuid[]) AS agent_ids,
                    e.vault
             FROM world_environment e
             LEFT JOIN world_environment_agents g
               ON g.owner_user_id = e.owner_user_id AND g.name = e.name
             WHERE e.owner_user_id = $1
             GROUP BY e.owner_user_id, e.name, e.updated_at, e.vault
             ORDER BY e.name",
        )
        .bind(owner)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn save_environment(&self, owner: Uuid, name: &str, value: &str) -> Result<()> {
        if !valid_name(name) || !valid_value(value) {
            return Err(anyhow!("invalid environment entry"));
        }
        let key = master_key()?;
        let value = if is_vault_reference(value) { value.trim() } else { value };
        let (ciphertext, nonce) = encrypt(key, owner, name, value.as_bytes())?;
        sqlx::query(
            "INSERT INTO world_environment (owner_user_id, name, ciphertext, nonce, vault)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (owner_user_id, name) DO UPDATE
             SET ciphertext = EXCLUDED.ciphertext, nonce = EXCLUDED.nonce, vault = EXCLUDED.vault, updated_at = NOW()",
        )
        .bind(owner)
        .bind(name)
        .bind(ciphertext)
        .bind(nonce)
        .bind(is_vault_reference(value))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn save_environment_many(&self, owner: Uuid, entries: &[(String, String)]) -> Result<()> {
        if entries.iter().any(|(name, value)| !valid_name(name) || !valid_value(value)) {
            return Err(anyhow!("invalid environment entry"));
        }
        let key = master_key()?;
        let mut tx = self.pool().begin().await?;
        for (name, value) in entries {
            let value = if is_vault_reference(value) { value.trim() } else { value.as_str() };
            let (ciphertext, nonce) = encrypt(key, owner, name, value.as_bytes())?;
            sqlx::query(
                "INSERT INTO world_environment (owner_user_id, name, ciphertext, nonce, vault)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (owner_user_id, name) DO UPDATE
                 SET ciphertext = EXCLUDED.ciphertext, nonce = EXCLUDED.nonce, vault = EXCLUDED.vault, updated_at = NOW()",
            )
            .bind(owner)
            .bind(name)
            .bind(ciphertext)
            .bind(nonce)
            .bind(is_vault_reference(value))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_environment_agents(
        &self,
        owner: Uuid,
        name: &str,
        agent_ids: &[Uuid],
    ) -> Result<bool> {
        if !valid_name(name) {
            return Err(anyhow!("invalid environment entry"));
        }
        let agent_ids: Vec<Uuid> = agent_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tx = self.pool().begin().await?;
        let exists: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM world_environment
             WHERE owner_user_id = $1 AND name = $2
             FOR UPDATE",
        )
        .bind(owner)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Ok(false);
        }
        let owned: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM agent_profiles
             WHERE id = ANY($1) AND owner_user_id = $2
             FOR UPDATE",
        )
        .bind(&agent_ids)
        .bind(owner)
        .fetch_all(&mut *tx)
        .await?;
        if owned.len() != agent_ids.len() {
            return Err(InvalidEnvironmentGrant.into());
        }
        sqlx::query("DELETE FROM world_environment_agents WHERE owner_user_id = $1 AND name = $2")
            .bind(owner)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO world_environment_agents (owner_user_id, name, agent_id)
             SELECT $1, $2, agent_id FROM UNNEST($3::uuid[]) AS agent_id",
        )
        .bind(owner)
        .bind(name)
        .bind(&agent_ids)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn delete_environment(&self, owner: Uuid, name: &str) -> Result<bool> {
        if !valid_name(name) {
            return Err(anyhow!("invalid environment entry"));
        }
        Ok(
            sqlx::query("DELETE FROM world_environment WHERE owner_user_id = $1 AND name = $2")
                .bind(owner)
                .bind(name)
                .execute(self.pool())
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn environment_value(&self, owner: Uuid, name: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, EncryptedEntry>(
            "SELECT owner_user_id, name, ciphertext, nonce FROM world_environment WHERE owner_user_id = $1 AND name = $2 AND NOT vault",
        )
        .bind(owner)
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let plaintext = open(row.owner_user_id, &row.name, &row.ciphertext, &row.nonce)?;
        let value = String::from_utf8(plaintext).map_err(|_| anyhow!("environment decryption failed"))?;
        Ok(Some(value.trim().to_string()).filter(|v| !v.is_empty()))
    }

    pub async fn environment_for_agent(&self, agent_id: Uuid) -> Result<BTreeMap<String, String>> {
        Ok(self.keys_for_agent(agent_id).await?.values)
    }

    pub async fn keys_for_agent(&self, agent_id: Uuid) -> Result<AgentKeys> {
        let rows = sqlx::query_as::<_, GrantedEntry>(&format!(
            "SELECT e.owner_user_id,
                    CASE WHEN v.environment IS NULL THEN e.name ELSE e.name || '@' || v.environment END AS name,
                    COALESCE(v.ciphertext, e.ciphertext) AS ciphertext, COALESCE(v.nonce, e.nonce) AS nonce,
                    COALESCE(v.vault, e.vault) AS vault
             FROM agent_profiles a
             JOIN world_environment_agents g ON g.agent_id = a.id
             JOIN world_environment e
               ON e.owner_user_id = g.owner_user_id AND e.name = g.name
             LEFT JOIN world_environment_choices c
               ON c.agent_id = a.id AND c.owner_user_id = e.owner_user_id AND c.name = e.name AND {}
             LEFT JOIN world_environment_variants v
               ON v.owner_user_id = c.owner_user_id AND v.name = c.name AND v.environment = c.environment
             WHERE a.id = $1 AND a.owner_user_id = e.owner_user_id
             UNION ALL
             SELECT CASE WHEN s.org_id = $3 THEN $2::uuid ELSE s.org_id END, s.name, s.ciphertext, s.nonce, FALSE
             FROM world_shared_environment_agents g
             JOIN world_shared_environment s ON s.org_id = g.org_id AND s.name = g.name
             JOIN agent_profiles a ON a.id = g.agent_id AND a.org_id = g.org_id
             WHERE g.agent_id = $1",
            crate::key_environments::ACTIVE_CHOICE
        ))
        .bind(agent_id)
        .bind(SHARED_OWNER)
        .bind(crate::orgs::DEFAULT_ORG)
        .fetch_all(self.pool())
        .await?;
        let mut values = BTreeMap::new();
        let mut vault_refs = BTreeMap::new();
        if rows.is_empty() {
            return Ok(AgentKeys { values, vault_refs });
        }
        master_key()?;
        let agent_org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?;
        for row in rows {
            let shared = row.owner_user_id == SHARED_OWNER || Some(row.owner_user_id) == agent_org;
            let key = crate::key_environments::key_of(&row.name).to_string();
            if shared && (values.contains_key(&key) || vault_refs.contains_key(&key)) {
                continue;
            }
            let plaintext = open(
                row.owner_user_id,
                &row.name,
                &row.ciphertext,
                &row.nonce,
            )?;
            let value = String::from_utf8(plaintext)
                .map_err(|_| anyhow!("environment decryption failed"))?;
            if shared {
                values.entry(key).or_insert(value);
            } else if row.vault {
                values.remove(&key);
                vault_refs.insert(key, value.trim().to_string());
            } else {
                vault_refs.remove(&key);
                values.insert(key, value);
            }
        }
        Ok(AgentKeys { values, vault_refs })
    }

    pub async fn rotate_environment_key(&self) -> Result<Rotation> {
        let previous = PREVIOUS_KEYS.get().cloned().unwrap_or_default();
        self.rotate_environment_key_with(master_key()?, &previous).await
    }

    async fn rotate_environment_key_with(&self, key: &[u8; 32], previous: &[[u8; 32]]) -> Result<Rotation> {
        let rows = sqlx::query_as::<_, StoredSecret>(
            "SELECT 'user' AS table_key, owner_user_id AS aad_owner, name, ciphertext, nonce FROM world_environment
             UNION ALL
             SELECT 'variant', owner_user_id, name || '@' || environment, ciphertext, nonce FROM world_environment_variants
             UNION ALL
             SELECT org_id::text, CASE WHEN org_id = $1 THEN $2::uuid ELSE org_id END, name, ciphertext, nonce FROM world_shared_environment
             UNION ALL
             SELECT 'feed', id, $3, secret_ciphertext, secret_nonce FROM agent_feeds WHERE secret_ciphertext IS NOT NULL AND secret_nonce IS NOT NULL",
        )
        .bind(crate::orgs::DEFAULT_ORG)
        .bind(SHARED_OWNER)
        .bind(crate::feed_auth::rotation_name())
        .fetch_all(self.pool())
        .await?;
        let mut rotation = Rotation::default();
        for row in rows {
            match open_with_keys(key, previous, row.aad_owner, &row.name, &row.ciphertext, &row.nonce) {
                None => rotation.unreadable += 1,
                Some((_, false)) => rotation.current += 1,
                Some((plain, true)) => {
                    let (ciphertext, nonce) = encrypt(key, row.aad_owner, &row.name, &plain)?;
                    let updated = if row.table_key == "feed" {
                        sqlx::query("UPDATE agent_feeds SET secret_ciphertext = $2, secret_nonce = $3 WHERE id = $1 AND secret_ciphertext = $4")
                            .bind(row.aad_owner)
                            .bind(&ciphertext)
                            .bind(&nonce)
                            .bind(&row.ciphertext)
                            .execute(self.pool())
                            .await?
                    } else if row.table_key == "variant" {
                        let (name, environment) = row.name.split_once('@').ok_or_else(|| anyhow!("bad key environment row"))?;
                        sqlx::query("UPDATE world_environment_variants SET ciphertext = $4, nonce = $5 WHERE owner_user_id = $1 AND name = $2 AND environment = $3 AND ciphertext = $6")
                            .bind(row.aad_owner)
                            .bind(name)
                            .bind(environment)
                            .bind(&ciphertext)
                            .bind(&nonce)
                            .bind(&row.ciphertext)
                            .execute(self.pool())
                            .await?
                    } else if row.table_key == "user" {
                        sqlx::query("UPDATE world_environment SET ciphertext = $3, nonce = $4 WHERE owner_user_id = $1 AND name = $2 AND ciphertext = $5")
                            .bind(row.aad_owner)
                            .bind(&row.name)
                            .bind(&ciphertext)
                            .bind(&nonce)
                            .bind(&row.ciphertext)
                            .execute(self.pool())
                            .await?
                    } else {
                        sqlx::query("UPDATE world_shared_environment SET ciphertext = $3, nonce = $4 WHERE org_id = $1::uuid AND name = $2 AND ciphertext = $5")
                            .bind(&row.table_key)
                            .bind(&row.name)
                            .bind(&ciphertext)
                            .bind(&nonce)
                            .bind(&row.ciphertext)
                            .execute(self.pool())
                            .await?
                    };
                    rotation.rotated += updated.rows_affected() as usize;
                }
            }
        }
        Ok(rotation)
    }

    pub async fn key_version(&self, agent_id: Uuid) -> Result<String> {
        Ok(sqlx::query_scalar(
            "SELECT md5(COALESCE(string_agg(k.part, ',' ORDER BY k.part), '')) FROM (
               SELECT 'u:' || g.name || ':' || e.updated_at::text AS part
               FROM world_environment_agents g
               JOIN agent_profiles a ON a.id = g.agent_id
               JOIN world_environment e ON e.owner_user_id = g.owner_user_id AND e.name = g.name AND e.owner_user_id = a.owner_user_id
               WHERE g.agent_id = $1
               UNION ALL
               SELECT 'c:' || c.name || ':' || c.environment || ':' || v.updated_at::text
               FROM world_environment_choices c
               JOIN world_environment_variants v ON v.owner_user_id = c.owner_user_id AND v.name = c.name AND v.environment = c.environment
               WHERE c.agent_id = $1 AND (c.expires_at IS NULL OR c.expires_at > NOW())
               UNION ALL
               SELECT 's:' || g.name || ':' || s.updated_at::text
               FROM world_shared_environment_agents g
               JOIN world_shared_environment s ON s.org_id = g.org_id AND s.name = g.name
               WHERE g.agent_id = $1
               UNION ALL
               SELECT 'm:' || EXISTS (
                 SELECT 1 FROM agent_profiles a
                 JOIN org_members m ON m.org_id = a.org_id AND m.user_id = a.owner_user_id
                 JOIN users u ON u.id = a.owner_user_id AND u.disabled_at IS NULL
                 WHERE a.id = $1)::text
             ) k",
        )
        .bind(agent_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn environment_names_for_agent(&self, agent_id: Uuid) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT e.name
             FROM agent_profiles a
             JOIN world_environment_agents g ON g.agent_id = a.id
             JOIN world_environment e
               ON e.owner_user_id = g.owner_user_id AND e.name = g.name
             WHERE a.id = $1 AND a.owner_user_id = e.owner_user_id
             UNION
             SELECT g.name FROM world_shared_environment_agents g
             JOIN agent_profiles a ON a.id = g.agent_id AND a.org_id = g.org_id
             WHERE g.agent_id = $1
             ORDER BY 1",
        )
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_shared_keys(&self, org: Uuid) -> Result<Vec<SharedKey>> {
        Ok(sqlx::query_as::<_, SharedKey>(
            "SELECT s.name, s.note, s.updated_at,
                    COALESCE(array_agg(g.agent_id ORDER BY g.agent_id)
                      FILTER (WHERE g.agent_id IS NOT NULL), ARRAY[]::uuid[]) AS agent_ids
             FROM world_shared_environment s
             LEFT JOIN world_shared_environment_agents g ON g.org_id = s.org_id AND g.name = s.name
             WHERE s.org_id = $1
             GROUP BY s.name, s.note, s.updated_at
             ORDER BY s.name",
        )
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn save_shared_key(&self, org: Uuid, by: Uuid, name: &str, value: Option<&str>, note: Option<&str>) -> Result<bool> {
        if !valid_name(name) || value.is_some_and(|v| !valid_value(v) || is_vault_reference(v)) || note.is_some_and(|n| n.chars().count() > 500) {
            return Err(anyhow!("invalid environment entry"));
        }
        let Some(value) = value else {
            let note = note.unwrap_or_default();
            return Ok(sqlx::query("UPDATE world_shared_environment SET note = $2, updated_by = $3, updated_at = NOW() WHERE name = $1 AND org_id = $4")
                .bind(name)
                .bind(note)
                .bind(by)
                .bind(org)
                .execute(self.pool())
                .await?
                .rows_affected()
                > 0);
        };
        let aad = if org == crate::orgs::DEFAULT_ORG { SHARED_OWNER } else { org };
        let (ciphertext, nonce) = encrypt(master_key()?, aad, name, value.as_bytes())?;
        sqlx::query(
            "INSERT INTO world_shared_environment (name, ciphertext, nonce, note, updated_by, org_id)
             VALUES ($1, $2, $3, COALESCE($4, ''), $5, $6)
             ON CONFLICT (org_id, name) DO UPDATE
             SET ciphertext = EXCLUDED.ciphertext, nonce = EXCLUDED.nonce,
                 note = COALESCE($4, world_shared_environment.note),
                 updated_by = EXCLUDED.updated_by, updated_at = NOW()",
        )
        .bind(name)
        .bind(ciphertext)
        .bind(nonce)
        .bind(note)
        .bind(by)
        .bind(org)
        .execute(self.pool())
        .await?;
        Ok(true)
    }

    pub async fn set_shared_key_agents(&self, org: Uuid, name: &str, agent_ids: &[Uuid]) -> Result<bool> {
        if !valid_name(name) {
            return Err(anyhow!("invalid environment entry"));
        }
        let agent_ids: Vec<Uuid> = agent_ids.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
        let mut tx = self.pool().begin().await?;
        let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM world_shared_environment WHERE name = $1 AND org_id = $2 FOR UPDATE")
            .bind(name)
            .bind(org)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Ok(false);
        }
        let found: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM agent_profiles WHERE id = ANY($1) AND owner_user_id IS NOT NULL AND org_id = $2 FOR UPDATE")
            .bind(&agent_ids)
            .bind(org)
            .fetch_all(&mut *tx)
            .await?;
        if found.len() != agent_ids.len() {
            return Err(InvalidEnvironmentGrant.into());
        }
        sqlx::query("DELETE FROM world_shared_environment_agents WHERE name = $1 AND org_id = $2")
            .bind(name)
            .bind(org)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO world_shared_environment_agents (org_id, name, agent_id) SELECT $3, $1, agent_id FROM UNNEST($2::uuid[]) AS agent_id")
            .bind(name)
            .bind(&agent_ids)
            .bind(org)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn delete_shared_key(&self, org: Uuid, name: &str) -> Result<bool> {
        if !valid_name(name) {
            return Err(anyhow!("invalid environment entry"));
        }
        Ok(sqlx::query("DELETE FROM world_shared_environment WHERE name = $1 AND org_id = $2")
            .bind(name)
            .bind(org)
            .execute(self.pool())
            .await?
            .rows_affected()
            > 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferRefusal {
    NotFound,
    MainAgent,
    OnDevice,
    Busy,
    SameOwner,
    BadRecipient,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WantedKeys {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub keys: Vec<String>,
}

impl WorldStore {
    pub async fn transfer_agent(&self, agent_id: Uuid, to_user: Uuid) -> Result<std::result::Result<(Uuid, Vec<String>), TransferRefusal>> {
        let mut tx = self.pool().begin().await?;
        let row: Option<(Option<Uuid>, Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT owner_user_id, runner_id,
                    EXISTS (SELECT 1 FROM users u WHERE u.main_agent_id = a.id) OR EXISTS (SELECT 1 FROM org_members m WHERE m.main_agent_id = a.id)
             FROM agent_profiles a WHERE id = $1 FOR UPDATE",
        )
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((owner, runner, is_main)) = row else {
            return Ok(Err(TransferRefusal::NotFound));
        };
        if is_main {
            return Ok(Err(TransferRefusal::MainAgent));
        }
        if runner.is_some() {
            return Ok(Err(TransferRefusal::OnDevice));
        }
        if owner == Some(to_user) {
            return Ok(Err(TransferRefusal::SameOwner));
        }
        let recipient: Option<Option<String>> = sqlx::query_scalar(
            "SELECT world_ladder_role(m.role) FROM org_members m JOIN agent_profiles a ON a.org_id = m.org_id
             WHERE a.id = $2 AND m.user_id = $1",
        )
            .bind(to_user)
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?;
        if !matches!(recipient, Some(Some(ref r)) if r == "write" || r == "admin") {
            return Ok(Err(TransferRefusal::BadRecipient));
        }
        let busy: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM session_runs WHERE agent_id = $1 AND status IN ('queued', 'running'))
                 OR EXISTS (SELECT 1 FROM agent_profiles WHERE id = $1 AND inbox_claimed_by IS NOT NULL)",
        )
        .bind(agent_id)
        .fetch_one(&mut *tx)
        .await?;
        if busy {
            return Ok(Err(TransferRefusal::Busy));
        }
        let mut wanted: BTreeSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT name FROM world_environment_agents WHERE agent_id = $1",
        )
        .bind(agent_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let definitions: Vec<Value> = sqlx::query_scalar(
            "SELECT s.definition FROM agent_mcp_servers m JOIN world_mcp_servers s ON s.id = m.server_id WHERE m.agent_id = $1",
        )
        .bind(agent_id)
        .fetch_all(&mut *tx)
        .await?;
        for definition in &definitions {
            if let Ok(names) = secret_reference_names(definition) {
                wanted.extend(names);
            }
        }
        let wanted: Vec<String> = wanted.into_iter().collect();
        sqlx::query("DELETE FROM world_environment_agents WHERE agent_id = $1")
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE agent_profiles
             SET owner_user_id = $2, wanted_keys = $3, visibility = 'private', pending_move = NULL, updated_at = NOW()
             WHERE id = $1",
        )
        .bind(agent_id)
        .bind(to_user)
        .bind(&wanted)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM agent_subscriptions WHERE agent_id = $1")
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Ok((owner.unwrap_or(to_user), wanted)))
    }

    pub async fn wanted_keys(&self, owner: Uuid) -> Result<Vec<WantedKeys>> {
        Ok(sqlx::query_as::<_, WantedKeys>(
            "SELECT a.id AS agent_id, a.name AS agent_name,
                    ARRAY(SELECT k FROM unnest(a.wanted_keys) k
                          WHERE NOT EXISTS (SELECT 1 FROM world_environment_agents g
                                            WHERE g.agent_id = a.id AND g.owner_user_id = $1 AND g.name = k)
                          ORDER BY k) AS keys
             FROM agent_profiles a
             WHERE a.owner_user_id = $1 AND cardinality(a.wanted_keys) > 0
             ORDER BY a.name",
        )
        .bind(owner)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .filter(|w| !w.keys.is_empty())
        .collect())
    }

    pub async fn dismiss_wanted_key(&self, owner: Uuid, agent_id: Uuid, name: &str) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE agent_profiles SET wanted_keys = array_remove(wanted_keys, $3)
             WHERE id = $2 AND owner_user_id = $1 AND $3 = ANY(wanted_keys)",
        )
        .bind(owner)
        .bind(agent_id)
        .bind(name)
        .execute(self.pool())
        .await?
        .rows_affected()
            > 0)
    }
}

pub(crate) fn master_key() -> Result<&'static [u8; 32]> {
    MASTER_KEY
        .get()
        .and_then(Option::as_ref)
        .ok_or_else(|| anyhow!("environment encryption is not configured"))
}

fn master_key_from(value: Option<&str>) -> Result<[u8; 32]> {
    let decoded = value
        .and_then(|value| STANDARD.decode(value).ok())
        .ok_or_else(|| anyhow!("environment encryption is not configured"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("environment encryption is not configured"))
}

fn aad(owner: Uuid, name: &str) -> Vec<u8> {
    let mut value = Vec::with_capacity(AAD_PREFIX.len() + 20 + name.len());
    value.extend_from_slice(AAD_PREFIX);
    value.extend_from_slice(owner.as_bytes());
    value.extend_from_slice(&(name.len() as u32).to_be_bytes());
    value.extend_from_slice(name.as_bytes());
    value
}

fn encryption_key(key: &[u8; 32]) -> Result<LessSafeKey> {
    Ok(LessSafeKey::new(
        UnboundKey::new(&aead::AES_256_GCM, key)
            .map_err(|_| anyhow!("environment encryption failed"))?,
    ))
}

pub(crate) fn encrypt(key: &[u8; 32], owner: Uuid, name: &str, value: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow!("environment encryption failed"))?;
    let mut ciphertext = value.to_vec();
    encryption_key(key)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(owner, name)),
            &mut ciphertext,
        )
        .map_err(|_| anyhow!("environment encryption failed"))?;
    Ok((ciphertext, nonce.to_vec()))
}

fn decrypt(
    key: &[u8; 32],
    owner: Uuid,
    name: &str,
    ciphertext: &[u8],
    nonce: &[u8],
) -> Result<Vec<u8>> {
    let nonce: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| anyhow!("environment decryption failed"))?;
    let mut plaintext = ciphertext.to_vec();
    let opened = encryption_key(key)?
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(owner, name)),
            &mut plaintext,
        )
        .map_err(|_| anyhow!("environment decryption failed"))?;
    Ok(opened.to_vec())
}

#[cfg(test)]
mod tests {

    #[test]
    fn masked_values_and_the_runner_slack_key_never_reach_a_cli() {
        let definition = runnable_definition(serde_json::json!({
            "command": "npx",
            "env": {"MCP_REMOTE_CONFIG_DIR": "***", "TEAM": "T1"},
            "headers": {"Authorization": "***"},
        }));
        assert_eq!(definition["env"], serde_json::json!({"TEAM": "T1"}));
        assert_eq!(definition["headers"], serde_json::json!({}));

        let slack = runnable_definition(serde_json::json!({
            "command": "python3",
            "args": ["/app/slack_mcp.py"],
            "env": {"WORLD_SLACK_CONNECTION_KEY": "${secret:SLACK_MAC_CONNECTION_KEY}", "WORLD_SLACK_TEAM_ID": "T1"},
        }));
        assert!(is_mac_slack_server(&slack));
        assert_eq!(slack["env"], serde_json::json!({"WORLD_SLACK_TEAM_ID": "T1"}));
        assert!(secret_reference_names(&slack).unwrap().is_empty());

        let other = runnable_definition(serde_json::json!({"command": "npx", "env": {"WORLD_SLACK_CONNECTION_KEY": "kept"}}));
        assert_eq!(other["env"]["WORLD_SLACK_CONNECTION_KEY"], "kept");
    }

    #[test]
    fn vault_refs_an_mcp_server_needs_stay_out_of_the_cli_environment() {
        let definitions = [serde_json::json!({"command": "x", "env": {"TOKEN": "${secret:LINEAR_KEY}"}})];
        let vault_refs = BTreeMap::from([
            ("LINEAR_KEY".to_string(), "op://Private/Linear/key".to_string()),
            ("GITHUB_TOKEN".to_string(), "op://Private/GitHub/token".to_string()),
        ]);
        assert_eq!(
            without_secret_references(vault_refs, &definitions),
            BTreeMap::from([("GITHUB_TOKEN".to_string(), "op://Private/GitHub/token".to_string())])
        );
    }

    #[test]
    fn missing_secrets_names_only_ungranted_keys() {
        let definition = serde_json::json!({"command": "x", "env": {"A": "${secret:A}", "B": "${secret:B}"}});
        let granted = BTreeMap::from([("A".to_string(), "1".to_string())]);
        assert_eq!(missing_secrets(&definition, &granted).unwrap(), vec!["B".to_string()]);
    }
    use super::*;

    #[tokio::test]
    async fn transfer_moves_owner_and_lists_wanted_keys() -> Result<()> {
        let Ok(url) = std::env::var("TRANSFER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut users = Vec::new();
        for (who, role) in [("a", "write"), ("b", "write"), ("c", "view")] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, $2) RETURNING id")
                .bind(format!("{who}-{tag}@test.dev"))
                .bind(role)
                .fetch_one(&pool)
                .await?;
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, $3)")
                .bind(crate::orgs::DEFAULT_ORG)
                .bind(id)
                .bind(if role == "write" { "member" } else { "guest" })
                .execute(&pool)
                .await?;
            users.push(id);
        }
        let (a, b, c) = (users[0], users[1], users[2]);
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility)
             VALUES ($1, 'Kimmy', 'claude', 'sonnet', $2, 'public') RETURNING id",
        )
        .bind(format!("kimmy-{tag}"))
        .bind(a)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO world_environment (owner_user_id, name, ciphertext, nonce) VALUES ($1, 'LINEAR_API_KEY', '\\x00', '\\x000000000000000000000000')")
            .bind(a)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_environment_agents (owner_user_id, name, agent_id) VALUES ($1, 'LINEAR_API_KEY', $2)")
            .bind(a)
            .bind(agent)
            .execute(&pool)
            .await?;
        let server: Uuid = sqlx::query_scalar("INSERT INTO world_mcp_servers (name, definition) VALUES ($1, $2) RETURNING id")
            .bind(format!("slack-{tag}"))
            .bind(serde_json::json!({ "command": "slack", "env": { "TOKEN": "${secret:SLACK_TOKEN}" } }))
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_mcp_servers (agent_id, server_id) VALUES ($1, $2)")
            .bind(agent)
            .bind(server)
            .execute(&pool)
            .await?;

        assert_eq!(store.transfer_agent(agent, c).await?, Err(TransferRefusal::BadRecipient));
        assert_eq!(store.transfer_agent(agent, a).await?, Err(TransferRefusal::SameOwner));
        let (from, wanted) = store.transfer_agent(agent, b).await?.expect("transfer");
        assert_eq!(from, a);
        assert_eq!(wanted, vec!["LINEAR_API_KEY".to_string(), "SLACK_TOKEN".to_string()]);
        let (owner, visibility): (Uuid, String) = sqlx::query_as("SELECT owner_user_id, visibility FROM agent_profiles WHERE id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!((owner, visibility.as_str()), (b, "private"));
        let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_environment_agents WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        assert_eq!(grants, 0);
        assert!(store.wanted_keys(a).await?.is_empty());
        let listed = store.wanted_keys(b).await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].keys, vec!["LINEAR_API_KEY".to_string(), "SLACK_TOKEN".to_string()]);
        assert!(store.dismiss_wanted_key(b, agent, "LINEAR_API_KEY").await?);
        assert!(!store.dismiss_wanted_key(a, agent, "SLACK_TOKEN").await?);
        assert_eq!(store.wanted_keys(b).await?[0].keys, vec!["SLACK_TOKEN".to_string()]);

        sqlx::query("UPDATE users SET main_agent_id = $1 WHERE id = $2").bind(agent).bind(b).execute(&pool).await?;
        assert_eq!(store.transfer_agent(agent, a).await?, Err(TransferRefusal::MainAgent));
        sqlx::query("UPDATE users SET main_agent_id = NULL WHERE id = $1").bind(b).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM world_mcp_servers WHERE id = $1").bind(server).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(&users).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn validates_names_and_values() {
        assert!(valid_name("LINEAR_API_KEY"));
        assert!(valid_name("DATABASE_URL"));
        assert!(!valid_name("lowercase"));
        assert!(!valid_name("WORLD_TOKEN"));
        assert!(!valid_name("PATH"));
        assert!(!valid_name("REDIS_URL"));
        assert!(!valid_name("CONFIG_PATH"));
        assert!(valid_name("ANTHROPIC_API_KEY"));
        assert!(valid_name("OPENAI_API_KEY"));
        assert!(valid_name("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(valid_name("APP_OPENAI_API_KEY"));
        assert!(!valid_name("LD_PRELOAD"));
        assert!(valid_value("secret"));
        assert!(!valid_value(""));
        assert!(!valid_value("bad\0value"));
    }

    #[test]
    fn vault_references_are_checked_before_they_are_kept() {
        for good in [
            "op://Private/GitHub/token",
            "op://Work Vault/Stripe API/credentials/secret key",
            "op://dev/ssh.key/private key?ssh-format=openssh",
            "op://dev/db/password?attribute=otp",
        ] {
            assert!(is_vault_reference(good), "{good}");
            assert!(valid_vault_reference(good), "{good}");
            assert!(valid_value(good), "{good}");
        }
        for bad in [
            "op://",
            "op://vault",
            "op://vault/item",
            "op://vault/item/section/field/extra",
            "op://vault//field",
            "op://vault/../field",
            "op://vault/item/field?format=json",
            "op://vault/item/field?attribute=",
            "op://vault/item/$(whoami)",
            "op://vault/item/field;rm",
            "op://vault/item/fi\nld",
            "op://-vault/item/field",
            "op://vault/item/--account=evil",
        ] {
            assert!(is_vault_reference(bad), "{bad}");
            assert!(!valid_vault_reference(bad), "{bad}");
            assert!(!valid_value(bad), "{bad}");
        }
        assert!(!is_vault_reference("-op://vault/item/field"));
        assert!(!valid_vault_reference("-op://vault/item/field"));
        assert!(!valid_vault_reference("--account=x op://vault/item/field"));
        assert!(!is_vault_reference("sk-live-123"));
        assert!(valid_value("sk-live-123"));
        assert!(!valid_vault_reference(&format!("op://v/i/{}", "a".repeat(600))));
    }

    #[tokio::test]
    async fn bulk_save_marks_vault_references_on_insert_and_update() -> Result<()> {
        let Ok(url) = std::env::var("KEYS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("bulk-{}@test.dev", Uuid::new_v4().simple()))
            .fetch_one(&pool)
            .await?;
        let vault_flags = |entries: Vec<EnvironmentEntry>| {
            entries.into_iter().map(|e| (e.name, e.vault)).collect::<BTreeMap<_, _>>()
        };
        let first = [
            ("PLAIN_KEY".to_string(), "sk-live-123".to_string()),
            ("VAULT_KEY".to_string(), "op://Private/GitHub/token".to_string()),
        ];
        store.save_environment_many(owner, &first).await?;
        assert_eq!(
            vault_flags(store.list_environment(owner).await?),
            BTreeMap::from([("PLAIN_KEY".to_string(), false), ("VAULT_KEY".to_string(), true)])
        );
        assert_eq!(store.environment_value(owner, "VAULT_KEY").await?, None);
        let swapped = [
            ("PLAIN_KEY".to_string(), "op://Private/Linear/key".to_string()),
            ("VAULT_KEY".to_string(), "ghp_plain".to_string()),
        ];
        store.save_environment_many(owner, &swapped).await?;
        assert_eq!(
            vault_flags(store.list_environment(owner).await?),
            BTreeMap::from([("PLAIN_KEY".to_string(), true), ("VAULT_KEY".to_string(), false)])
        );
        assert_eq!(store.environment_value(owner, "VAULT_KEY").await?.as_deref(), Some("ghp_plain"));
        sqlx::query("DELETE FROM world_environment WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn vault_provider_keys_are_not_counted_as_the_owners_own() -> Result<()> {
        let Ok(url) = std::env::var("KEYS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("providers-{}@test.dev", Uuid::new_v4().simple()))
            .fetch_one(&pool)
            .await?;
        let keys = [
            ("OPENAI_API_KEY".to_string(), "sk-proj-plain-value-123456".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "op://Private/Anthropic/key".to_string()),
        ];
        store.save_environment_many(owner, &keys).await?;
        assert_eq!(store.own_provider_names(owner).await?, vec!["OPENAI_API_KEY".to_string()]);
        sqlx::query("DELETE FROM world_environment WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn validates_mcp_secret_reference_locations_and_names() {
        let definition = serde_json::json!({
            "command": "server",
            "env": { "TOKEN": "Bearer ${secret:LINEAR_API_KEY}" },
            "headers": { "X-Key": "${secret:DATABASE_URL}" }
        });
        assert_eq!(
            secret_reference_names(&definition).unwrap(),
            BTreeSet::from(["DATABASE_URL".to_string(), "LINEAR_API_KEY".to_string()])
        );
        assert!(secret_reference_names(&serde_json::json!({
            "command": "${secret:LINEAR_API_KEY}"
        }))
        .is_err());
        assert!(secret_reference_names(&serde_json::json!({
            "${secret:LINEAR_API_KEY}": "value"
        }))
        .is_err());
        assert!(secret_reference_names(&serde_json::json!({
            "command": "server",
            "env": { "TOKEN": "${secret:lowercase}" }
        }))
        .is_err());
        assert!(secret_reference_names(&serde_json::json!({
            "url": "https://example.com",
            "headers": { "Authorization": "${secret:LINEAR_API_KEY" }
        }))
        .is_err());
        assert!(secret_reference_names(&serde_json::json!({
            "command": "ordinary",
            "env": { "PORT": 3000 }
        }))
        .unwrap()
        .is_empty());
    }

    #[test]
    fn rejects_missing_and_malformed_keys() {
        assert!(master_key_from(None).is_err());
        assert!(master_key_from(Some("not base64")).is_err());
        assert!(master_key_from(Some(&STANDARD.encode([0u8; 31]))).is_err());
        assert!(master_key_from(Some(&STANDARD.encode([0u8; 32]))).is_ok());
    }

    #[test]
    fn initialization_captures_key_and_clears_environment() -> Result<()> {
        const MODE: &str = "WORLD_ENVIRONMENT_INIT_TEST";
        if let Ok(mode) = std::env::var(MODE) {
            initialize();
            assert!(std::env::var("WORLD_SECRETS_KEY").is_err());
            match mode.as_str() {
                "valid" => {
                    assert!(configured());
                    let owner = Uuid::new_v4();
                    let key = master_key()?;
                    let (ciphertext, nonce) = encrypt(key, owner, "TOKEN", b"value")?;
                    assert_eq!(decrypt(key, owner, "TOKEN", &ciphertext, &nonce)?, b"value");
                }
                "missing" | "malformed" => assert!(!configured()),
                _ => unreachable!(),
            }
            return Ok(());
        }

        let executable = std::env::current_exe()?;
        for mode in ["valid", "missing", "malformed"] {
            let mut command = std::process::Command::new(&executable);
            command
                .arg("--exact")
                .arg("environment::tests::initialization_captures_key_and_clears_environment")
                .env(MODE, mode)
                .env_remove("WORLD_SECRETS_KEY");
            match mode {
                "valid" => {
                    command.env("WORLD_SECRETS_KEY", STANDARD.encode([11u8; 32]));
                }
                "malformed" => {
                    command.env("WORLD_SECRETS_KEY", "not-base64");
                }
                _ => {}
            }
            assert!(command.status()?.success());
        }
        Ok(())
    }

    #[tokio::test]
    async fn kit_dedupe_key_versions_and_rotation() -> Result<()> {
        let Ok(url) = std::env::var("KIT_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("kit-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member')")
            .bind(crate::orgs::DEFAULT_ORG)
            .bind(user)
            .execute(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Kit', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("kit-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let context7 = serde_json::json!({ "command": "npx", "args": ["-y", "@upstash/context7-mcp"] });
        let mut shared_copy = context7.clone();
        shared_copy["shared"] = serde_json::json!(true);
        let mut ids = Vec::new();
        for (name, definition) in [
            (format!("a-ctx-{tag}"), context7.clone()),
            (format!("b-ctx-{tag}"), shared_copy),
            (format!("c-other-{tag}"), serde_json::json!({ "command": "other", "args": [tag.clone()] })),
        ] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO world_mcp_servers (name, definition, created_by) VALUES ($1, $2, $3) RETURNING id")
                .bind(&name)
                .bind(definition)
                .bind(user)
                .fetch_one(&pool)
                .await?;
            sqlx::query("INSERT INTO agent_mcp_servers (agent_id, server_id) VALUES ($1, $2)").bind(agent).bind(id).execute(&pool).await?;
            ids.push(id);
        }
        let kit: Vec<String> = store.mcp_servers_for(agent).await?.into_iter().map(|s| s.name).collect();
        assert_eq!(kit, vec![format!("a-ctx-{tag}"), format!("c-other-{tag}")], "the same server equipped twice reaches the agent once");
        let listed = store.list_mcp_servers(Some(crate::orgs::DEFAULT_ORG)).await?;
        let duplicate = |name: String| listed.iter().find(|s| s.name == name).and_then(|s| s.duplicate_of.clone());
        assert_eq!(duplicate(format!("b-ctx-{tag}")), Some(format!("a-ctx-{tag}")));
        assert_eq!(duplicate(format!("a-ctx-{tag}")), None);
        assert_eq!(duplicate(format!("c-other-{tag}")), None);

        let empty = store.key_version(agent).await?;
        let old = [5u8; 32];
        let new = [6u8; 32];
        let (ciphertext, nonce) = encrypt(&old, user, "LINEAR_API_KEY", b"lin-secret")?;
        sqlx::query("INSERT INTO world_environment (owner_user_id, name, ciphertext, nonce) VALUES ($1, 'LINEAR_API_KEY', $2, $3)")
            .bind(user)
            .bind(&ciphertext)
            .bind(&nonce)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_environment_agents (owner_user_id, name, agent_id) VALUES ($1, 'LINEAR_API_KEY', $2)")
            .bind(user)
            .bind(agent)
            .execute(&pool)
            .await?;
        let granted = store.key_version(agent).await?;
        assert_ne!(granted, empty, "a grant changes the version");
        assert_eq!(store.key_version(agent).await?, granted, "the version is stable");
        sqlx::query("UPDATE world_environment SET updated_at = NOW() + INTERVAL '1 second' WHERE owner_user_id = $1").bind(user).execute(&pool).await?;
        let changed = store.key_version(agent).await?;
        assert_ne!(changed, granted, "a new value changes the version");

        let feed_id: Uuid = sqlx::query_scalar("INSERT INTO agent_feeds (agent_id, owner_user_id, kind, name, token) VALUES ($1, $2, 'webhook', 'hook', $3) RETURNING id")
            .bind(agent)
            .bind(user)
            .bind(crate::feeds::new_token())
            .fetch_one(&pool)
            .await?;
        let (feed_ct, feed_nonce) = encrypt(&old, feed_id, crate::feed_auth::rotation_name(), b"lkwh_feed")?;
        sqlx::query("UPDATE agent_feeds SET secret_ciphertext = $2, secret_nonce = $3 WHERE id = $1")
            .bind(feed_id)
            .bind(&feed_ct)
            .bind(&feed_nonce)
            .execute(&pool)
            .await?;

        let rotation = store.rotate_environment_key_with(&new, &[old]).await?;
        assert!(rotation.rotated >= 2, "{rotation:?}");
        let (feed_stored, feed_stored_nonce): (Vec<u8>, Vec<u8>) = sqlx::query_as("SELECT secret_ciphertext, secret_nonce FROM agent_feeds WHERE id = $1")
            .bind(feed_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(decrypt(&new, feed_id, crate::feed_auth::rotation_name(), &feed_stored, &feed_stored_nonce)?, b"lkwh_feed", "webhook secrets rotate too");
        let (stored, stored_nonce): (Vec<u8>, Vec<u8>) =
            sqlx::query_as("SELECT ciphertext, nonce FROM world_environment WHERE owner_user_id = $1 AND name = 'LINEAR_API_KEY'")
                .bind(user)
                .fetch_one(&pool)
                .await?;
        assert_eq!(decrypt(&new, user, "LINEAR_API_KEY", &stored, &stored_nonce)?, b"lin-secret");
        assert!(decrypt(&old, user, "LINEAR_API_KEY", &stored, &stored_nonce).is_err());
        assert_eq!(store.rotate_environment_key_with(&new, &[old]).await?.rotated, 0, "rotation is idempotent");
        assert_eq!(store.key_version(agent).await?, changed, "rotation alone does not revoke running servers");

        sqlx::query("DELETE FROM org_members WHERE user_id = $1").bind(user).execute(&pool).await?;
        assert_ne!(store.key_version(agent).await?, changed, "leaving the organization changes the version");
        Ok(())
    }

    #[test]
    fn previous_keys_still_open_and_report_rotation() {
        let old = [3u8; 32];
        let new = [4u8; 32];
        let owner = Uuid::new_v4();
        let (ciphertext, nonce) = encrypt(&old, owner, "TOKEN", b"value").unwrap();
        assert!(open_with_keys(&new, &[], owner, "TOKEN", &ciphertext, &nonce).is_none());
        assert_eq!(open_with_keys(&new, &[old], owner, "TOKEN", &ciphertext, &nonce), Some((b"value".to_vec(), true)));
        let (fresh, fresh_nonce) = encrypt(&new, owner, "TOKEN", b"value").unwrap();
        assert_eq!(open_with_keys(&new, &[old], owner, "TOKEN", &fresh, &fresh_nonce), Some((b"value".to_vec(), false)));
        assert!(open_with_keys(&new, &[old], Uuid::new_v4(), "TOKEN", &ciphertext, &nonce).is_none());
        let keys = previous_keys_from(Some(&format!("{} ,{}\nbad", STANDARD.encode(old), STANDARD.encode(new))));
        assert_eq!(keys, vec![old, new]);
    }

    #[test]
    fn ciphertext_is_bound_and_uses_unique_nonces() {
        let key = [7u8; 32];
        let other_key = [8u8; 32];
        let owner = Uuid::new_v4();
        let other_owner = Uuid::new_v4();
        let (ciphertext, nonce) = encrypt(&key, owner, "TOKEN", b"secret").unwrap();
        let (_, next_nonce) = encrypt(&key, owner, "TOKEN", b"secret").unwrap();
        assert_ne!(nonce, next_nonce);
        assert_eq!(
            decrypt(&key, owner, "TOKEN", &ciphertext, &nonce).unwrap(),
            b"secret"
        );
        assert!(decrypt(&other_key, owner, "TOKEN", &ciphertext, &nonce).is_err());
        assert!(decrypt(&key, other_owner, "TOKEN", &ciphertext, &nonce).is_err());
        assert!(decrypt(&key, owner, "OTHER", &ciphertext, &nonce).is_err());
        let mut tampered_nonce = nonce.clone();
        tampered_nonce[0] ^= 1;
        assert!(decrypt(&key, owner, "TOKEN", &ciphertext, &tampered_nonce).is_err());
        let mut tampered = ciphertext;
        tampered[0] ^= 1;
        assert!(decrypt(&key, owner, "TOKEN", &tampered, &nonce).is_err());
    }

    #[tokio::test]
    async fn postgres_environment_lifecycle() -> Result<()> {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let Ok(database_url) = std::env::var("WORLD_ENVIRONMENT_TEST_DATABASE_URL") else {
            return Ok(());
        };
        const CHILD: &str = "WORLD_ENVIRONMENT_DATABASE_TEST_CHILD";
        if std::env::var(CHILD).is_err() {
            let status = std::process::Command::new(std::env::current_exe()?)
                .arg("--exact")
                .arg("environment::tests::postgres_environment_lifecycle")
                .env(CHILD, "1")
                .env("WORLD_ENVIRONMENT_TEST_DATABASE_URL", &database_url)
                .env("WORLD_SECRETS_KEY", STANDARD.encode([9u8; 32]))
                .status()?;
            assert!(status.success());
            return Ok(());
        }
        initialize();
        assert!(configured());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        let schema = format!("world_environment_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await?;
        let options =
            PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let result: Result<()> = async {
            sqlx::raw_sql(
                "CREATE TABLE users (id UUID PRIMARY KEY);
                 CREATE TABLE agent_profiles (
                   id UUID PRIMARY KEY,
                   owner_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
                   visibility TEXT NOT NULL
                 );
                 CREATE TABLE world_mcp_servers (
                   id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                   name TEXT NOT NULL UNIQUE,
                   definition JSONB NOT NULL,
                   scope TEXT NOT NULL,
                   enabled BOOLEAN NOT NULL DEFAULT TRUE,
                   created_by UUID REFERENCES users(id) ON DELETE SET NULL,
                   created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                 );
                 CREATE TABLE world_skills (
                   id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                   name TEXT NOT NULL UNIQUE,
                   description TEXT NOT NULL DEFAULT '',
                   body TEXT NOT NULL,
                   source_url TEXT,
                   scope TEXT NOT NULL,
                   enabled BOOLEAN NOT NULL DEFAULT TRUE,
                   created_by UUID REFERENCES users(id) ON DELETE SET NULL,
                   created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                   updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                 );",
            )
            .execute(&pool)
            .await?;
            sqlx::raw_sql(include_str!("../../migrations/040_world_environment.sql"))
                .execute(&pool)
                .await?;
            sqlx::raw_sql(include_str!("../../migrations/096_shared_keys.sql"))
                .execute(&pool)
                .await?;
            sqlx::raw_sql(crate::orgs::ORG_COLUMNS_FOR_TESTS).execute(&pool).await?;
            sqlx::raw_sql(include_str!("../../migrations/126_key_environments.sql"))
                .execute(&pool)
                .await?;

            let owner = Uuid::new_v4();
            let other_owner = Uuid::new_v4();
            let private_agent = Uuid::new_v4();
            let public_agent = Uuid::new_v4();
            let foreign_agent = Uuid::new_v4();
            sqlx::query("INSERT INTO users (id) VALUES ($1), ($2)")
                .bind(owner)
                .bind(other_owner)
                .execute(&pool)
                .await?;
            sqlx::query(
                "INSERT INTO agent_profiles (id, owner_user_id, visibility)
                 VALUES ($1, $4, 'private'), ($2, $4, 'public'), ($3, $5, 'private')",
            )
            .bind(private_agent)
            .bind(public_agent)
            .bind(foreign_agent)
            .bind(owner)
            .bind(other_owner)
            .execute(&pool)
            .await?;

            let store = WorldStore::new(pool.clone());
            let original_definition = serde_json::json!({"command": "original"});
            let replacement_definition = serde_json::json!({"command": "replacement"});
            store
                .upsert_mcp_server("owned-server", &original_definition, "agent", Some(owner), Some(crate::orgs::DEFAULT_ORG))
                .await?;
            store
                .upsert_mcp_server(
                    "owned-server",
                    &replacement_definition,
                    "agent",
                    Some(owner),
                    Some(crate::orgs::DEFAULT_ORG),
                )
                .await?;
            assert!(store
                .upsert_mcp_server(
                    "owned-server",
                    &serde_json::json!({"command": "foreign"}),
                    "agent",
                    Some(other_owner),
                    Some(crate::orgs::DEFAULT_ORG),
                )
                .await
                .is_err());
            assert_eq!(
                sqlx::query_scalar::<_, Value>(
                    "SELECT definition FROM world_mcp_servers WHERE name = 'owned-server'"
                )
                .fetch_one(&pool)
                .await?,
                replacement_definition
            );

            store
                .upsert_skill(
                    "owned-skill",
                    "original",
                    "original body",
                    None,
                    "agent",
                    Some(owner),
                    Some(crate::orgs::DEFAULT_ORG),
                )
                .await?;
            store
                .upsert_skill(
                    "owned-skill",
                    "replacement",
                    "replacement body",
                    None,
                    "agent",
                    Some(owner),
                    Some(crate::orgs::DEFAULT_ORG),
                )
                .await?;
            assert!(store
                .upsert_skill(
                    "owned-skill",
                    "foreign",
                    "foreign body",
                    None,
                    "agent",
                    Some(other_owner),
                    Some(crate::orgs::DEFAULT_ORG),
                )
                .await
                .is_err());
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT body FROM world_skills WHERE name = 'owned-skill'"
                )
                .fetch_one(&pool)
                .await?,
                "replacement body"
            );

            store
                .save_environment(owner, "LINEAR_API_KEY", "first-value")
                .await?;
            let entries = store.list_environment(owner).await?;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "LINEAR_API_KEY");
            assert!(entries[0].agent_ids.is_empty());
            assert!(store.set_environment_agents(owner, "LINEAR_API_KEY", &[public_agent]).await?);
            assert_eq!(
                store.environment_for_agent(public_agent).await?,
                BTreeMap::from([("LINEAR_API_KEY".to_string(), "first-value".to_string())])
            );
            assert!(store
                .set_environment_agents(owner, "LINEAR_API_KEY", &[foreign_agent])
                .await
                .unwrap_err()
                .is::<InvalidEnvironmentGrant>());

            assert!(
                store
                    .set_environment_agents(owner, "LINEAR_API_KEY", &[private_agent])
                    .await?
            );
            assert_eq!(
                store.environment_for_agent(private_agent).await?,
                BTreeMap::from([("LINEAR_API_KEY".to_string(), "first-value".to_string())])
            );
            assert_eq!(
                store.environment_names_for_agent(private_agent).await?,
                vec!["LINEAR_API_KEY"]
            );

            store
                .save_environment(owner, "LINEAR_API_KEY", "replacement-value")
                .await?;
            assert_eq!(
                store.list_environment(owner).await?[0].agent_ids,
                vec![private_agent]
            );
            assert_eq!(
                store.environment_for_agent(private_agent).await?,
                BTreeMap::from([(
                    "LINEAR_API_KEY".to_string(),
                    "replacement-value".to_string()
                )])
            );

            assert!(
                store
                    .set_environment_agents(owner, "LINEAR_API_KEY", &[])
                    .await?
            );
            assert!(store.environment_for_agent(private_agent).await?.is_empty());
            assert!(
                store
                    .set_environment_agents(owner, "LINEAR_API_KEY", &[private_agent])
                    .await?
            );
            sqlx::query("UPDATE agent_profiles SET visibility = 'public' WHERE id = $1")
                .bind(private_agent)
                .execute(&pool)
                .await?;
            assert_eq!(store.list_environment(owner).await?[0].agent_ids, vec![private_agent]);
            sqlx::query("UPDATE agent_profiles SET visibility = 'private' WHERE id = $1")
                .bind(private_agent)
                .execute(&pool)
                .await?;

            assert!(
                store
                    .set_environment_agents(owner, "LINEAR_API_KEY", &[private_agent])
                    .await?
            );
            sqlx::query("UPDATE agent_profiles SET owner_user_id = $2 WHERE id = $1")
                .bind(private_agent)
                .bind(other_owner)
                .execute(&pool)
                .await?;
            assert!(store.list_environment(owner).await?[0].agent_ids.is_empty());
            sqlx::query("UPDATE agent_profiles SET owner_user_id = $2 WHERE id = $1")
                .bind(private_agent)
                .bind(owner)
                .execute(&pool)
                .await?;
            assert!(store.environment_for_agent(private_agent).await?.is_empty());

            assert!(
                store
                    .set_environment_agents(owner, "LINEAR_API_KEY", &[private_agent])
                    .await?
            );
            assert!(store.environment_for_agent(public_agent).await?.is_empty());

            assert!(store.save_shared_key(crate::orgs::DEFAULT_ORG, owner, "TEAM_TOKEN", Some("team-value"), Some("CI bot")).await?);
            assert!(store.save_shared_key(crate::orgs::DEFAULT_ORG, owner, "LINEAR_API_KEY", Some("shared-linear"), None).await?);
            assert!(store.set_shared_key_agents(crate::orgs::DEFAULT_ORG, "TEAM_TOKEN", &[public_agent, foreign_agent]).await?);
            assert!(store.set_shared_key_agents(crate::orgs::DEFAULT_ORG, "LINEAR_API_KEY", &[private_agent]).await?);
            assert!(!store.set_shared_key_agents(crate::orgs::DEFAULT_ORG, "MISSING_KEY", &[public_agent]).await?);
            assert!(store.set_shared_key_agents(crate::orgs::DEFAULT_ORG, "TEAM_TOKEN", &[Uuid::new_v4()]).await.unwrap_err().is::<InvalidEnvironmentGrant>());
            assert_eq!(
                store.environment_for_agent(foreign_agent).await?,
                BTreeMap::from([("TEAM_TOKEN".to_string(), "team-value".to_string())])
            );
            assert_eq!(
                store.environment_for_agent(private_agent).await?,
                BTreeMap::from([("LINEAR_API_KEY".to_string(), "replacement-value".to_string())])
            );
            assert_eq!(store.environment_names_for_agent(public_agent).await?, vec!["TEAM_TOKEN"]);
            let shared = store.list_shared_keys(crate::orgs::DEFAULT_ORG).await?;
            assert_eq!(shared.iter().map(|k| (k.name.as_str(), k.note.as_str())).collect::<Vec<_>>(), vec![("LINEAR_API_KEY", ""), ("TEAM_TOKEN", "CI bot")]);
            assert!(store.save_shared_key(crate::orgs::DEFAULT_ORG, owner, "TEAM_TOKEN", None, Some("renamed")).await?);
            assert!(!store.save_shared_key(crate::orgs::DEFAULT_ORG, owner, "NOPE_KEY", None, Some("x")).await?);
            sqlx::query("UPDATE agent_profiles SET owner_user_id = $2 WHERE id = $1")
                .bind(public_agent)
                .bind(other_owner)
                .execute(&pool)
                .await?;
            assert!(store.environment_for_agent(public_agent).await?.is_empty());
            assert!(store.delete_shared_key(crate::orgs::DEFAULT_ORG, "TEAM_TOKEN").await?);
            assert!(store.environment_for_agent(foreign_agent).await?.is_empty());
            assert!(store.delete_shared_key(crate::orgs::DEFAULT_ORG, "LINEAR_API_KEY").await?);
            assert!(store.delete_environment(owner, "LINEAR_API_KEY").await?);
            assert!(store.list_environment(owner).await?.is_empty());
            Ok(())
        }
        .await;

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await?;
        result
    }
}
