use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{WorldAgent, WorldStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    View,
    Read,
    Write,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "view" => Some(Role::View),
            "read" => Some(Role::Read),
            "write" => Some(Role::Write),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::View => "view",
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Person {
    pub id: Uuid,
    pub name: Option<String>,
    pub email: Option<String>,
    pub world_role: Option<String>,
}

pub use crate::orgs::Viewer;

fn same_org(v: &Viewer, agent: &WorldAgent) -> bool {
    v.org.is_some() && agent.org_id == v.org
}

pub fn can_see(v: &Viewer, agent: &WorldAgent) -> bool {
    same_org(v, agent) && (v.role == Role::Admin || agent.visibility == "public" || agent.owner_user_id == Some(v.user))
}

pub fn can_use(v: &Viewer, agent: &WorldAgent) -> bool {
    same_org(v, agent)
        && (v.role == Role::Admin || (v.role >= Role::Write && (agent.visibility == "public" || agent.owner_user_id == Some(v.user))))
}

pub fn can_manage(v: &Viewer, agent: &WorldAgent) -> bool {
    same_org(v, agent) && (v.role == Role::Admin || (v.role >= Role::Write && agent.owner_user_id == Some(v.user)))
}

impl WorldStore {
    pub async fn people_in(&self, org: Uuid) -> Result<Vec<Person>> {
        Ok(sqlx::query_as::<_, Person>(
            "SELECT u.id, u.name, u.email, world_ladder_role(m.role) AS world_role
             FROM org_members m JOIN users u ON u.id = m.user_id
             WHERE m.org_id = $1
             ORDER BY u.name NULLS LAST, u.email",
        )
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn hidden_agents(&self, v: &Viewer) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM agent_profiles
             WHERE org_id = $3 AND $2 = false AND visibility = 'private' AND owner_user_id IS DISTINCT FROM $1",
        )
        .bind(v.user)
        .bind(v.role == Role::Admin)
        .bind(v.org)
        .fetch_all(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn roles_are_ordered() {
        assert!(Role::View < Role::Read && Role::Read < Role::Write && Role::Write < Role::Admin);
        assert_eq!(Role::parse("write"), Some(Role::Write));
        assert_eq!(Role::parse("owner"), None);
    }
}
