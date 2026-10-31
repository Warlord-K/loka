use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::world::{WorldStore, CHANNEL_INBOX, CHANNEL_RUNS};

pub const ARCHIVED: &str = "Archived";
pub const MAX_DELETE_DAYS: i32 = 3650;

const KEEPS_ITS_PAUSE: &str = "(paused_reason LIKE 'Organization %' OR paused_reason LIKE 'Owner left%' OR paused_reason LIKE 'Owner account disabled%')";

pub fn delete_days_valid(days: i32) -> bool {
    (1..=MAX_DELETE_DAYS).contains(&days)
}

impl WorldStore {
    pub async fn archive_agent(&self, id: Uuid, delete_after_days: Option<i32>) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(&format!(
            "UPDATE agent_profiles
                SET archived_at = COALESCE(archived_at, NOW()),
                    archive_delete_at = CASE WHEN $2::INT IS NULL THEN NULL ELSE NOW() + make_interval(days => $2) END,
                    paused_reason = CASE WHEN {KEEPS_ITS_PAUSE} THEN paused_reason ELSE '{ARCHIVED}' END,
                    budget_paused_on = NULL,
                    updated_at = NOW()
              WHERE id = $1
              RETURNING archive_delete_at"
        ))
        .bind(id)
        .bind(delete_after_days)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    pub async fn unarchive_agent(&self, id: Uuid) -> Result<bool> {
        let changed = sqlx::query(&format!(
            "UPDATE agent_profiles
                SET archived_at = NULL, archive_delete_at = NULL,
                    paused_reason = CASE WHEN paused_reason = '{ARCHIVED}' THEN NULL ELSE paused_reason END,
                    updated_at = NOW()
              WHERE id = $1 AND archived_at IS NOT NULL"
        ))
        .bind(id)
        .execute(self.pool())
        .await?
        .rows_affected()
            > 0;
        if changed {
            sqlx::query("SELECT pg_notify($1, $2), pg_notify($3, $2)")
                .bind(CHANNEL_INBOX)
                .bind(id.to_string())
                .bind(CHANNEL_RUNS)
                .execute(self.pool())
                .await?;
        }
        Ok(changed)
    }

    pub async fn archived_due_for_deletion(&self) -> Result<Vec<(Uuid, String, Option<Uuid>)>> {
        Ok(sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
            "SELECT a.id, a.name, a.owner_user_id FROM agent_profiles a
              WHERE a.archived_at IS NOT NULL AND a.archive_delete_at <= NOW()
                AND NOT EXISTS (SELECT 1 FROM users u WHERE u.main_agent_id = a.id)
                AND NOT EXISTS (SELECT 1 FROM org_members m WHERE m.main_agent_id = a.id)
              ORDER BY a.archive_delete_at LIMIT 50",
        )
        .fetch_all(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archiving_pauses_hides_and_deletes_only_when_due() -> Result<()> {
        let Ok(url) = std::env::var("ARCHIVE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("arch-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let make = |slug: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Old', 'claude', 'haiku', $2) RETURNING id",
                )
                .bind(slug)
                .bind(owner)
                .fetch_one(&pool)
                .await
            }
        };
        let kept = make(format!("kept-{tag}")).await?;
        let doomed = make(format!("doomed-{tag}")).await?;
        let locked = make(format!("locked-{tag}")).await?;

        assert!(store.archive_agent(kept, None).await?.is_none());
        let agent = store.get_agent(kept).await?.expect("agent");
        assert!(agent.archived_at.is_some());
        assert_eq!(agent.paused_reason.as_deref(), Some(ARCHIVED));

        assert!(store.archive_agent(doomed, Some(7)).await?.is_some());
        assert!(store.archived_due_for_deletion().await?.iter().all(|(id, _, _)| *id != doomed), "not due yet");
        sqlx::query("UPDATE agent_profiles SET archive_delete_at = NOW() - INTERVAL '1 minute' WHERE id = $1").bind(doomed).execute(&pool).await?;
        assert!(store.archived_due_for_deletion().await?.iter().any(|(id, _, _)| *id == doomed));
        assert!(store.archived_due_for_deletion().await?.iter().all(|(id, _, _)| *id != kept), "no date means never");

        assert!(store.archive_agent(doomed, None).await?.is_none(), "archiving again with no days turns auto delete off");
        assert!(store.archived_due_for_deletion().await?.iter().all(|(id, _, _)| *id != doomed));

        store.set_paused(locked, Some("Organization suspended")).await?;
        store.archive_agent(locked, None).await?;
        assert!(store.unarchive_agent(locked).await?);
        let back = store.get_agent(locked).await?.expect("agent");
        assert_eq!(back.paused_reason.as_deref(), Some("Organization suspended"), "an organization pause outlives the archive");

        assert!(store.unarchive_agent(kept).await?);
        let back = store.get_agent(kept).await?.expect("agent");
        assert!(back.archived_at.is_none() && back.archive_delete_at.is_none() && back.paused_reason.is_none());
        assert!(!store.unarchive_agent(kept).await?);

        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![kept, doomed, locked]).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }
}
