use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::access::Role;
use crate::runners::{random_token, token_hash};
use crate::world::WorldStore;

pub const TOKEN_PREFIX: &str = "wld_";
pub const MAX_ACTIVE_TOKENS: i64 = 25;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_AGENTS: usize = 50;
pub const DEFAULT_DAYS: i64 = 90;
pub const MAX_DAYS: i64 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
        }
    }

    pub fn cap(&self, role: Role) -> Role {
        match self {
            Scope::Read => role.min(Role::Read),
            Scope::Write => role,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ApiToken {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub scope: String,
    pub agent_ids: Option<Vec<Uuid>>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    pub token: ApiToken,
    pub secret: String,
}

type TokenRow = (Uuid, Uuid, String, Option<Vec<Uuid>>, Uuid);

#[derive(Debug, Clone)]
pub struct TokenGrant {
    pub token_id: Uuid,
    pub user_id: Uuid,
    pub scope: Scope,
    pub agent_ids: Option<Vec<Uuid>>,
    pub org_id: Uuid,
}

impl TokenGrant {
    pub fn allows_agent(&self, agent: Uuid) -> bool {
        self.agent_ids.as_ref().is_none_or(|ids| ids.contains(&agent))
    }
}

pub fn new_secret() -> Result<String> {
    Ok(format!("{TOKEN_PREFIX}{}", random_token()?))
}

pub fn looks_like_token(secret: &str) -> bool {
    secret.starts_with(TOKEN_PREFIX) && secret.len() > TOKEN_PREFIX.len() + 20 && secret.len() < 200
}

pub fn display_prefix(secret: &str) -> String {
    secret.chars().take(TOKEN_PREFIX.len() + 6).collect()
}

pub fn clean_name(name: &str) -> Result<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    if name.is_empty() {
        bail!("give the token a name");
    }
    Ok(name)
}

const COLUMNS: &str = "id, name, prefix, scope, agent_ids, created_at, expires_at, last_used_at, revoked_at";

impl WorldStore {
    pub async fn create_api_token(&self, user_id: Uuid, name: &str, scope: Scope, agent_ids: Option<Vec<Uuid>>, days: i64) -> Result<CreatedToken> {
        let name = clean_name(name)?;
        if !(1..=MAX_DAYS).contains(&days) {
            bail!("tokens last between 1 and {MAX_DAYS} days");
        }
        if agent_ids.as_ref().is_some_and(|ids| ids.is_empty() || ids.len() > MAX_AGENTS) {
            bail!("pick between 1 and {MAX_AGENTS} agents, or leave the list out for all of them");
        }
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > NOW()")
            .bind(user_id)
            .fetch_one(self.pool())
            .await?;
        if active >= MAX_ACTIVE_TOKENS {
            bail!("you already have {MAX_ACTIVE_TOKENS} tokens; revoke one first");
        }
        let secret = new_secret()?;
        let token = sqlx::query_as::<_, ApiToken>(&format!(
            "INSERT INTO api_tokens (user_id, name, token_hash, prefix, scope, agent_ids, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, NOW() + make_interval(days => $7)) RETURNING {COLUMNS}"
        ))
        .bind(user_id)
        .bind(&name)
        .bind(token_hash(&secret))
        .bind(display_prefix(&secret))
        .bind(scope.as_str())
        .bind(agent_ids)
        .bind(days as i32)
        .fetch_one(self.pool())
        .await?;
        Ok(CreatedToken { token, secret })
    }

    pub async fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>> {
        Ok(sqlx::query_as::<_, ApiToken>(&format!(
            "SELECT {COLUMNS} FROM api_tokens WHERE user_id = $1
             AND COALESCE(revoked_at, expires_at) > NOW() - INTERVAL '7 days'
             ORDER BY revoked_at IS NOT NULL OR expires_at <= NOW(), created_at DESC"
        ))
        .bind(user_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn revoke_api_token(&self, user_id: Uuid, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("UPDATE api_tokens SET revoked_at = NOW() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL")
            .bind(id)
            .bind(user_id)
            .execute(self.pool())
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn authenticate_api_token(&self, secret: &str) -> Result<Option<TokenGrant>> {
        if !looks_like_token(secret) {
            return Ok(None);
        }
        let row: Option<TokenRow> = sqlx::query_as(
            "UPDATE api_tokens SET last_used_at = NOW()
             WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > NOW()
             RETURNING id, user_id, scope, agent_ids, org_id",
        )
        .bind(token_hash(secret))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.and_then(|(token_id, user_id, scope, agent_ids, org_id)| {
            Some(TokenGrant { token_id, user_id, scope: Scope::parse(&scope)?, agent_ids, org_id })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_prefixed_and_unique() {
        let a = new_secret().unwrap();
        let b = new_secret().unwrap();
        assert!(a.starts_with(TOKEN_PREFIX));
        assert!(looks_like_token(&a));
        assert_ne!(a, b);
        assert_ne!(token_hash(&a), token_hash(&b));
        assert_eq!(token_hash(&a), token_hash(&a.clone()));
        assert_eq!(token_hash(&a).len(), 32);
        assert_eq!(display_prefix(&a).len(), TOKEN_PREFIX.len() + 6);
    }

    #[test]
    fn rejects_foreign_tokens() {
        assert!(!looks_like_token("wld_short"));
        assert!(!looks_like_token("eyJhbGciOiJkaXIiLCJlbmMiOiJBMjU2Q0JDLUhTNTEyIn0..abc.def.ghi"));
        assert!(!looks_like_token(""));
    }

    #[test]
    fn read_scope_caps_role() {
        assert_eq!(Scope::Read.cap(Role::Admin), Role::Read);
        assert_eq!(Scope::Read.cap(Role::Write), Role::Read);
        assert_eq!(Scope::Read.cap(Role::View), Role::View);
        assert_eq!(Scope::Write.cap(Role::Write), Role::Write);
        assert_eq!(Scope::Write.cap(Role::View), Role::View);
        assert_eq!(Scope::Write.cap(Role::Admin), Role::Admin);
        assert_eq!(Scope::parse("write"), Some(Scope::Write));
        assert_eq!(Scope::parse("admin"), None);
    }

    #[test]
    fn agent_allowlist() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let open = TokenGrant { token_id: a, user_id: a, scope: Scope::Read, agent_ids: None, org_id: a };
        let narrow = TokenGrant { agent_ids: Some(vec![a]), ..open.clone() };
        assert!(open.allows_agent(b));
        assert!(narrow.allows_agent(a));
        assert!(!narrow.allows_agent(b));
    }

    #[tokio::test]
    async fn tokens_expire() -> Result<()> {
        let Ok(url) = std::env::var("TOKENS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("tokens-{}@tokens.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, user, None).await?;
        assert!(store.create_api_token(user, "zero", Scope::Read, None, 0).await.is_err());
        assert!(store.create_api_token(user, "long", Scope::Read, None, MAX_DAYS + 1).await.is_err());
        let created = store.create_api_token(user, "laptop", Scope::Read, None, DEFAULT_DAYS).await?;
        let days = (created.token.expires_at - created.token.created_at).num_days();
        assert!((DEFAULT_DAYS - 1..=DEFAULT_DAYS).contains(&days), "{days}");
        assert!(store.authenticate_api_token(&created.secret).await?.is_some());
        sqlx::query("UPDATE api_tokens SET expires_at = NOW() - INTERVAL '1 minute' WHERE id = $1")
            .bind(created.token.id)
            .execute(&pool)
            .await?;
        assert!(store.authenticate_api_token(&created.secret).await?.is_none(), "expired tokens are refused");
        let listed = store.list_api_tokens(user).await?;
        assert!(listed.iter().any(|t| t.id == created.token.id), "recently expired tokens stay listed");
        Ok(())
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  laptop\n ").unwrap(), "laptop");
        assert!(clean_name("   ").is_err());
        assert_eq!(clean_name(&"x".repeat(200)).unwrap().len(), MAX_NAME_CHARS);
    }
}
