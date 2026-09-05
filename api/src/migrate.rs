use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, PgPool};
use tracing::info;

static MIGRATOR: Migrator = sqlx::migrate!("../migrations");
const ADOPT_LOCK: i64 = 0x6b61_726e_615f_6d67;

fn file_name(version: i64, description: &str) -> String {
    format!("{version:03}_{}.sql", description.replace(' ', "_"))
}

pub fn enabled() -> bool {
    std::env::var("AUTO_MIGRATE").map(|v| !matches!(v.trim(), "false" | "0" | "no")).unwrap_or(true)
}

pub async fn run(pool: &PgPool) -> Result<()> {
    adopt_legacy_ledger(pool).await.context("could not adopt schema_migrations")?;
    MIGRATOR.run(pool).await.context("migrations failed")?;
    mirror_legacy_ledger(pool).await.context("could not update schema_migrations")?;
    info!(count = MIGRATOR.iter().count(), "database migrations up to date");
    Ok(())
}

async fn table_exists(conn: &mut sqlx::PgConnection, name: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL").bind(name).fetch_one(conn).await?)
}

async fn adopt_legacy_ledger(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(ADOPT_LOCK).execute(&mut *tx).await?;
    if table_exists(&mut tx, "_sqlx_migrations").await? || !table_exists(&mut tx, "schema_migrations").await? {
        return Ok(());
    }
    let applied: Vec<String> = sqlx::query_scalar("SELECT version FROM schema_migrations").fetch_all(&mut *tx).await?;
    if applied.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
           version BIGINT PRIMARY KEY,
           description TEXT NOT NULL,
           installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
           success BOOLEAN NOT NULL,
           checksum BYTEA NOT NULL,
           execution_time BIGINT NOT NULL
         )",
    )
    .execute(&mut *tx)
    .await?;
    let mut adopted = 0;
    for m in MIGRATOR.iter().filter(|m| applied.contains(&file_name(m.version, &m.description))) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES ($1, $2, TRUE, $3, 0)",
        )
        .bind(m.version)
        .bind(m.description.as_ref())
        .bind(m.checksum.as_ref())
        .execute(&mut *tx)
        .await?;
        adopted += 1;
    }
    tx.commit().await?;
    info!(adopted, "adopted migrations already recorded in schema_migrations");
    Ok(())
}

async fn mirror_legacy_ledger(pool: &PgPool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    if !table_exists(&mut conn, "schema_migrations").await? {
        return Ok(());
    }
    let names: Vec<String> = MIGRATOR.iter().map(|m| file_name(m.version, &m.description)).collect();
    sqlx::query("INSERT INTO schema_migrations (version) SELECT unnest($1::text[]) ON CONFLICT DO NOTHING")
        .bind(&names)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
