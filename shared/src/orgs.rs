use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::access::Role;
use crate::world::WorldStore;

pub const DEFAULT_ORG: Uuid = Uuid::from_u128(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    Guest,
    Member,
    Admin,
    Owner,
}

impl OrgRole {
    pub fn parse(s: &str) -> Option<OrgRole> {
        match s {
            "guest" => Some(OrgRole::Guest),
            "member" => Some(OrgRole::Member),
            "admin" => Some(OrgRole::Admin),
            "owner" => Some(OrgRole::Owner),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            OrgRole::Guest => "guest",
            OrgRole::Member => "member",
            OrgRole::Admin => "admin",
            OrgRole::Owner => "owner",
        }
    }

    pub fn ladder(&self) -> Role {
        match self {
            OrgRole::Owner | OrgRole::Admin => Role::Admin,
            OrgRole::Member => Role::Write,
            OrgRole::Guest => Role::Read,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Viewer {
    pub user: Uuid,
    pub org: Option<Uuid>,
    pub role: Role,
    pub org_role: Option<OrgRole>,
    pub instance_admin: bool,
}

impl Viewer {
    pub fn org(&self) -> Uuid {
        self.org.unwrap_or(Uuid::nil())
    }

    pub fn in_org(&self, org: Option<Uuid>) -> bool {
        self.org.is_some() && org == self.org
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Org {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub join_domains: Vec<String>,
    pub plan: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub(crate) async fn lock_org(tx: &mut Transaction<'_, Postgres>, org: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('world_org_members:' || $1::text))").bind(org).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn lock_org_parent(tx: &mut Transaction<'_, Postgres>, org: Uuid) -> Result<bool> {
    lock_org(tx, org).await?;
    Ok(sqlx::query_scalar::<_, Uuid>("SELECT id FROM orgs WHERE id = $1 FOR KEY SHARE")
        .bind(org)
        .fetch_optional(&mut **tx)
        .await?
        .is_some())
}

pub(crate) async fn audit_in(tx: &mut Transaction<'_, Postgres>, org: Uuid, actor: Option<Uuid>, action: &str, target: Option<String>, detail: Value) -> Result<()> {
    sqlx::query("INSERT INTO org_audit (org_id, actor_user_id, action, target, detail) VALUES ($1, $2, $3, $4, $5)")
        .bind(org)
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn add_member(tx: &mut Transaction<'_, Postgres>, org: Uuid, user: Uuid, role: OrgRole, invited_by: Option<Uuid>, how: &str) -> Result<bool> {
    let added = sqlx::query(
        "INSERT INTO org_members (org_id, user_id, role, invited_by) VALUES ($1, $2, $3, $4) ON CONFLICT (org_id, user_id) DO NOTHING",
    )
    .bind(org)
    .bind(user)
    .bind(role.as_str())
    .bind(invited_by)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;
    if added {
        sqlx::query(
            "INSERT INTO channel_members (channel_id, member_kind, member_id)
             SELECT id, 'user', $2 FROM channels WHERE org_id = $1 AND kind = 'channel' AND lower(name) = 'general'
             ON CONFLICT DO NOTHING",
        )
        .bind(org)
        .bind(user)
        .execute(&mut **tx)
        .await?;
        audit_in(tx, org, invited_by.or(Some(user)), "member.joined", Some(user.to_string()), json!({ "role": role.as_str(), "via": how })).await?;
    }
    Ok(added)
}

impl WorldStore {
    pub async fn resolve_viewer(&self, user: Uuid) -> Result<Viewer> {
        let Some((active, instance_admin)) =
            sqlx::query_as::<_, (Option<Uuid>, bool)>("SELECT active_org_id, instance_admin FROM users WHERE id = $1")
                .bind(user)
                .fetch_optional(self.pool())
                .await?
        else {
            return Ok(Viewer { user, org: None, role: Role::View, org_role: None, instance_admin: false });
        };
        let mut memberships = self.live_memberships(user).await?;
        if memberships.is_empty() {
            self.auto_join(user).await?;
            memberships = self.live_memberships(user).await?;
        }
        let instance_admin = instance_admin
            || sqlx::query_scalar::<_, bool>("SELECT instance_admin FROM users WHERE id = $1")
                .bind(user)
                .fetch_one(self.pool())
                .await?;
        let chosen = memberships
            .iter()
            .find(|(org, _)| Some(*org) == active)
            .or_else(|| memberships.first())
            .cloned();
        let Some((org, role)) = chosen else {
            return Ok(Viewer { user, org: None, role: Role::View, org_role: None, instance_admin });
        };
        if Some(org) != active {
            self.point_at_org(user, org).await?;
        }
        let org_role = OrgRole::parse(&role).unwrap_or(OrgRole::Guest);
        Ok(Viewer { user, org: Some(org), role: org_role.ladder(), org_role: Some(org_role), instance_admin })
    }

    pub async fn role_in_org(&self, user: Uuid, org: Option<Uuid>) -> Result<Option<OrgRole>> {
        let Some(org) = org else { return Ok(None) };
        let role: Option<String> = sqlx::query_scalar(
            "SELECT m.role FROM org_members m JOIN orgs o ON o.id = m.org_id AND o.suspended_at IS NULL
             WHERE m.org_id = $1 AND m.user_id = $2",
        )
        .bind(org)
        .bind(user)
        .fetch_optional(self.pool())
        .await?;
        Ok(role.as_deref().and_then(OrgRole::parse))
    }

    pub async fn is_org_member(&self, org: Uuid, user: Uuid) -> Result<bool> {
        Ok(self.role_in_org(user, Some(org)).await?.is_some())
    }

    async fn live_memberships(&self, user: Uuid) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as::<_, (Uuid, String)>(
            "SELECT m.org_id, m.role FROM org_members m JOIN orgs o ON o.id = m.org_id AND o.suspended_at IS NULL
             WHERE m.user_id = $1 ORDER BY m.joined_at, m.org_id",
        )
        .bind(user)
        .fetch_all(self.pool())
        .await?)
    }

    async fn point_at_org(&self, user: Uuid, org: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE users SET active_org_id = $2,
                main_agent_id = COALESCE((SELECT main_agent_id FROM org_members WHERE org_id = $2 AND user_id = $1), main_agent_id)
             WHERE id = $1",
        )
        .bind(user)
        .bind(org)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn auto_join(&self, user: Uuid) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('world_org_auto_join'))").execute(&mut *tx).await?;
        let already: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM org_members WHERE user_id = $1)")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
        if already {
            return Ok(());
        }
        let default_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM orgs WHERE id = $1 AND suspended_at IS NULL)")
            .bind(DEFAULT_ORG)
            .fetch_one(&mut *tx)
            .await?;
        if !default_exists {
            return Ok(());
        }
        let anyone: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM org_members)").fetch_one(&mut *tx).await?;
        let role = if anyone { OrgRole::Guest } else { OrgRole::Owner };
        add_member(&mut tx, DEFAULT_ORG, user, role, None, if anyone { "default" } else { "first" }).await?;
        if !anyone {
            sqlx::query("UPDATE users SET instance_admin = TRUE WHERE id = $1").bind(user).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE users SET active_org_id = $2 WHERE id = $1").bind(user).bind(DEFAULT_ORG).execute(&mut *tx).await?;
        tx.commit().await?;
        self.ensure_main_agent(user, DEFAULT_ORG).await?;
        Ok(())
    }

    pub async fn get_org(&self, id: Uuid) -> Result<Option<Org>> {
        Ok(sqlx::query_as::<_, Org>(
            "SELECT id, slug, name, join_domains, plan, suspended_at, created_at FROM orgs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn main_agent_in(&self, user: Uuid, org: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT COALESCE(m.main_agent_id,
                    (SELECT u.main_agent_id FROM users u JOIN agent_profiles a ON a.id = u.main_agent_id AND a.org_id = $2 WHERE u.id = $1))
             FROM org_members m WHERE m.org_id = $2 AND m.user_id = $1",
        )
        .bind(user)
        .bind(org)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    pub async fn ensure_main_agent(&self, user: Uuid, org: Uuid) -> Result<Option<Uuid>> {
        if let Some(main) = self.main_agent_in(user, org).await? {
            sqlx::query("UPDATE org_members SET main_agent_id = $3 WHERE org_id = $2 AND user_id = $1 AND main_agent_id IS NULL")
                .bind(user)
                .bind(org)
                .bind(main)
                .execute(self.pool())
                .await?;
            return Ok(Some(main));
        }
        let source: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT a.id, a.slug FROM agent_profiles a
             WHERE a.owner_user_id = $1 AND a.org_id <> $2
               AND (a.id = (SELECT main_agent_id FROM users WHERE id = $1)
                    OR a.id IN (SELECT main_agent_id FROM org_members WHERE user_id = $1))
             ORDER BY (a.id = (SELECT main_agent_id FROM users WHERE id = $1)) DESC, a.created_at
             LIMIT 1",
        )
        .bind(user)
        .bind(org)
        .fetch_optional(self.pool())
        .await?;
        let Some((source, slug)) = source else { return Ok(None) };
        let org_slug: String = sqlx::query_scalar("SELECT slug FROM orgs WHERE id = $1").bind(org).fetch_one(self.pool()).await?;
        let base: String = format!("{}-{}", slug.chars().take(24).collect::<String>(), org_slug.chars().take(16).collect::<String>());
        let mut handle = base.clone();
        let mut n = 2;
        while self.slug_taken(&handle).await? {
            handle = format!("{base}-{n}");
            n += 1;
        }
        let mut tx = self.pool().begin().await?;
        let created: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles
               (slug, name, avatar_emoji, cli, model, persona, role, look, gear, visibility, owner_user_id, org_id, tags, expertise, system_prompt_addendum)
             SELECT $2, name, avatar_emoji, cli, model, persona, role, look, gear, 'private', owner_user_id, $3, tags, expertise, system_prompt_addendum
             FROM agent_profiles WHERE id = $1
             RETURNING id",
        )
        .bind(source)
        .bind(&handle)
        .bind(org)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE org_members SET main_agent_id = $3 WHERE org_id = $2 AND user_id = $1")
            .bind(user)
            .bind(org)
            .bind(created)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if let Err(error) = crate::runners::RunnerStore::new(self.pool().clone()).assign_default_runner(created).await {
            tracing::warn!(%error, agent_id = %created, "could not pick a default device");
        }
        Ok(Some(created))
    }

    pub async fn org_members_of(&self, org: Uuid) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar("SELECT user_id FROM org_members WHERE org_id = $1")
            .bind(org)
            .fetch_all(self.pool())
            .await?)
    }

    pub async fn shares_org(&self, a: Uuid, b: Uuid, org: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) = 2 FROM org_members WHERE org_id = $1 AND user_id IN ($2, $3)",
        )
        .bind(org)
        .bind(a)
        .bind(b)
        .fetch_one(self.pool())
        .await?)
    }
}

#[cfg(test)]
pub(crate) const ORG_COLUMNS_FOR_TESTS: &str = "
ALTER TABLE IF EXISTS agent_profiles ADD COLUMN IF NOT EXISTS org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE IF EXISTS world_mcp_servers ADD COLUMN IF NOT EXISTS org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE IF EXISTS world_skills ADD COLUMN IF NOT EXISTS org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE world_shared_environment_agents DROP CONSTRAINT IF EXISTS world_shared_environment_agents_name_fkey;
ALTER TABLE world_shared_environment ADD COLUMN IF NOT EXISTS org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE world_shared_environment DROP CONSTRAINT IF EXISTS world_shared_environment_pkey;
ALTER TABLE world_shared_environment ADD PRIMARY KEY (org_id, name);
ALTER TABLE world_shared_environment_agents ADD COLUMN IF NOT EXISTS org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
ALTER TABLE world_shared_environment_agents DROP CONSTRAINT IF EXISTS world_shared_environment_agents_pkey;
ALTER TABLE world_shared_environment_agents ADD PRIMARY KEY (org_id, name, agent_id);
ALTER TABLE world_shared_environment_agents ADD FOREIGN KEY (org_id, name) REFERENCES world_shared_environment(org_id, name) ON DELETE CASCADE;
";

#[cfg(test)]
pub(crate) async fn join_default_for_tests(pool: &sqlx::PgPool, user: Uuid, main: Option<Uuid>) -> Result<()> {
    sqlx::query(
        "INSERT INTO org_members (org_id, user_id, role, main_agent_id) VALUES ($1, $2, 'member', $3)
         ON CONFLICT (org_id, user_id) DO UPDATE SET main_agent_id = COALESCE(EXCLUDED.main_agent_id, org_members.main_agent_id)",
    )
    .bind(DEFAULT_ORG)
    .bind(user)
    .bind(main)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE users SET active_org_id = $2 WHERE id = $1").bind(user).bind(DEFAULT_ORG).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_roles_map_to_the_ladder() {
        assert_eq!(OrgRole::Owner.ladder(), Role::Admin);
        assert_eq!(OrgRole::Admin.ladder(), Role::Admin);
        assert_eq!(OrgRole::Member.ladder(), Role::Write);
        assert_eq!(OrgRole::Guest.ladder(), Role::Read);
        assert!(OrgRole::Guest < OrgRole::Member && OrgRole::Admin < OrgRole::Owner);
    }
}
