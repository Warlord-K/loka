use std::time::Instant;

use anyhow::Result;
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};

pub const INDEXES: &[(&str, &str, &str)] = &[
    ("idx_search_agent_messages_body", "agent_messages", "body"),
    ("idx_search_channel_messages_body", "channel_messages", "body"),
    ("idx_search_reports_title", "world_reports", "title"),
    ("idx_search_reports_body", "world_reports", "body"),
    ("idx_search_tasks_title", "agent_tasks", "title"),
    ("idx_search_tasks_description", "agent_tasks", "description"),
];

pub fn enabled() -> bool {
    std::env::var("WORLD_SEARCH_INDEX").map_or(true, |value| value != "off")
}

pub async fn ensure(pool: &PgPool) -> Result<usize> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SET statement_timeout = 0").execute(&mut *conn).await?;
    let built = build(&mut conn).await;
    let _ = sqlx::query("RESET statement_timeout").execute(&mut *conn).await;
    built
}

async fn trigram_ops(conn: &mut PgConnection) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT quote_ident(n.nspname) || '.gin_trgm_ops'
         FROM pg_opclass c
         JOIN pg_namespace n ON n.oid = c.opcnamespace
         JOIN pg_am a ON a.oid = c.opcmethod
         WHERE c.opcname = 'gin_trgm_ops' AND a.amname = 'gin'
         LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?)
}

async fn build(conn: &mut PgConnection) -> Result<usize> {
    let mut ops = trigram_ops(conn).await?;
    if ops.is_none() {
        if let Err(error) = sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm").execute(&mut *conn).await {
            warn!(%error, "pg_trgm is not available, so search reads without trigram indexes");
            return Ok(0);
        }
        ops = trigram_ops(conn).await?;
    }
    let Some(ops) = ops else {
        warn!("pg_trgm has no gin operator class, so search reads without trigram indexes");
        return Ok(0);
    };
    let mut built = 0;
    for (name, table, column) in INDEXES {
        let (exists, valid, building) = sqlx::query_as::<_, (bool, bool, bool)>(
            "SELECT to_regclass($1) IS NOT NULL,
                    COALESCE((SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass($1)), false),
                    EXISTS (SELECT 1 FROM pg_stat_progress_create_index WHERE index_relid = to_regclass($1))",
        )
        .bind(name)
        .fetch_one(&mut *conn)
        .await?;
        if building || (exists && valid) {
            continue;
        }
        if exists {
            if let Err(error) = sqlx::query(&format!("DROP INDEX CONCURRENTLY IF EXISTS {name}")).execute(&mut *conn).await {
                warn!(%error, index = name, "Could not drop an unfinished search index");
                continue;
            }
        }
        let started = Instant::now();
        let sql = format!("CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {table} USING gin ({column} {ops})");
        match sqlx::query(&sql).execute(&mut *conn).await {
            Ok(_) => {
                built += 1;
                info!(index = name, ms = started.elapsed().as_millis() as u64, "Built search index");
            }
            Err(error) => warn!(%error, index = name, "Could not build search index"),
        }
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_once_and_replaces_broken_indexes() -> Result<()> {
        let Ok(url) = std::env::var("SEARCH_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        ensure(&pool).await?;
        assert_eq!(ensure(&pool).await?, 0);
        sqlx::query("UPDATE pg_index SET indisvalid = false WHERE indexrelid = to_regclass('idx_search_reports_title')")
            .execute(&pool)
            .await?;
        assert_eq!(ensure(&pool).await?, 1);
        let valid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_index WHERE indisvalid AND indexrelid IN (SELECT to_regclass(name) FROM unnest($1::text[]) name)",
        )
        .bind(INDEXES.iter().map(|(name, _, _)| name.to_string()).collect::<Vec<_>>())
        .fetch_one(&pool)
        .await?;
        assert_eq!(valid as usize, INDEXES.len());
        Ok(())
    }
}
