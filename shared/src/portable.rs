use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::environment::{is_vault_reference, open};
use crate::world::{looks_secret, mask_text, mask_url, WorldStore};

pub const FORMAT: &str = "loka.export";
pub const VERSION: i32 = 1;
const MESSAGES_PER_AGENT: i64 = 300;
const STROKES_PER_PRACTICE: i64 = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    pub user_id: Uuid,
    pub org_id: Uuid,
    #[serde(default)]
    pub org_name: Option<String>,
    #[serde(default)]
    pub main_agent_id: Option<Uuid>,
    #[serde(default)]
    pub app_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub version: i32,
    pub exported_at: DateTime<Utc>,
    pub source: Source,
    #[serde(default)]
    pub agents: Vec<Value>,
    #[serde(default)]
    pub memories: Vec<Value>,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub skills: Vec<Value>,
    #[serde(default)]
    pub agent_skills: Vec<Value>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
    #[serde(default)]
    pub agent_mcp_servers: Vec<Value>,
    #[serde(default)]
    pub subscriptions: Vec<Value>,
    #[serde(default)]
    pub omp_configs: Vec<Value>,
    #[serde(default)]
    pub schedules: Vec<Value>,
    #[serde(default)]
    pub feeds: Vec<Value>,
    #[serde(default)]
    pub rules: Vec<Value>,
    #[serde(default)]
    pub reports: Vec<Value>,
    #[serde(default)]
    pub regions: Vec<Value>,
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub todos: Vec<Value>,
    #[serde(default)]
    pub teams: Vec<Value>,
    #[serde(default)]
    pub team_members: Vec<Value>,
    #[serde(default)]
    pub practice_attempts: Vec<Value>,
    #[serde(default)]
    pub practice_coach_prefs: Vec<Value>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MovingKey {
    pub name: String,
    pub value: String,
    pub agents: Vec<Uuid>,
}

impl std::fmt::Debug for MovingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MovingKey").field("name", &self.name).field("value", &"***").field("agents", &self.agents).finish()
    }
}

#[derive(FromRow)]
struct StoredKey {
    owner_user_id: Uuid,
    name: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    vault: bool,
    agent_ids: Vec<Uuid>,
}

pub fn mask_definition(definition: &mut Value) -> bool {
    let mut masked = false;
    for key in ["env", "headers"] {
        if let Some(map) = definition.get_mut(key).and_then(Value::as_object_mut) {
            for (name, v) in map.iter_mut() {
                let keep = v.as_str().is_some_and(|text| text.contains("${secret:") || text.is_empty() || (key == "env" && plain_setting(name, text)));
                if !keep {
                    *v = Value::String("***".into());
                    masked = true;
                }
            }
        }
    }
    if let Some(args) = definition.get_mut("args").and_then(Value::as_array_mut) {
        for arg in args.iter_mut() {
            if let Some(text) = arg.as_str().filter(|text| looks_secret(text) && !text.contains("${secret:")) {
                *arg = Value::String(mask_text(text));
                masked = true;
            }
        }
    }
    if let Some(url) = definition.get_mut("url") {
        if let Some(text) = url.as_str() {
            let hidden = mask_url(text);
            if hidden != text {
                masked = true;
            }
            *url = Value::String(hidden);
        }
    }
    masked
}

pub fn plain_setting(name: &str, value: &str) -> bool {
    let name = name.to_ascii_uppercase();
    let secret_name = ["TOKEN", "KEY", "SECRET", "PASS", "AUTH", "CREDENTIAL", "COOKIE", "SESSION", "BEARER", "PRIVATE"].iter().any(|word| name.contains(word));
    !secret_name && !looks_secret(value) && value.len() <= 512
}

fn text_of<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn ids_of(rows: &[Value]) -> Vec<Uuid> {
    rows.iter().filter_map(|row| text_of(row, "id")).filter_map(|text| Uuid::parse_str(text).ok()).collect()
}

const EXPORTED_TABLES: &[&str] = &[
    "agent_profiles",
    "agent_sessions",
    "agent_messages",
    "world_skills",
    "agent_skills",
    "world_mcp_servers",
    "agent_mcp_servers",
    "agent_subscriptions",
    "agent_omp_configs",
    "schedules",
    "agent_feeds",
    "world_rules",
    "world_reports",
    "world_regions",
    "world_items",
    "world_todos",
    "agent_teams",
    "agent_team_members",
    "world_practice_attempts",
    "world_practice_coach_prefs",
];

impl WorldStore {
    async fn existing_tables(&self) -> Result<std::collections::HashSet<String>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables
             WHERE table_schema = current_schema() AND table_name = ANY($1)",
        )
        .bind(EXPORTED_TABLES)
        .fetch_all(self.pool())
        .await?;
        Ok(names.into_iter().collect())
    }

    async fn export_rows(&self, sql: &str, user: Uuid, org: Uuid, agents: &[Uuid]) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar::<_, Value>(sql)
            .bind(user)
            .bind(org)
            .bind(agents)
            .fetch_all(self.pool())
            .await?)
    }

    pub async fn export_bundle(&self, user: Uuid, org: Uuid, app_url: Option<String>) -> Result<Bundle> {
        let tables = self.existing_tables().await?;
        let has = |table: &str| tables.contains(table);
        let agents = if has("agent_profiles") {
            self.export_rows(
                "SELECT to_jsonb(a) FROM agent_profiles a
                 WHERE a.owner_user_id = $1 AND a.org_id = $2 AND cardinality($3::uuid[]) >= 0
                 ORDER BY a.created_at",
                user,
                org,
                &[],
            )
            .await?
        } else {
            Vec::new()
        };
        let ids = ids_of(&agents);
        let (org_name, main_agent_id): (Option<String>, Option<Uuid>) = sqlx::query_as(
            "SELECT o.name, m.main_agent_id FROM orgs o
             LEFT JOIN org_members m ON m.org_id = o.id AND m.user_id = $1
             WHERE o.id = $2",
        )
        .bind(user)
        .bind(org)
        .fetch_optional(self.pool())
        .await?
        .unwrap_or((None, None));
        let q = |table: &'static str, sql: &'static str| {
            let present = has(table);
            let ids = &ids;
            async move {
                if present {
                    self.export_rows(sql, user, org, ids).await
                } else {
                    Ok(Vec::new())
                }
            }
        };
        let memories = q(
            "agent_sessions",
            "SELECT jsonb_build_object('agent_id', s.agent_id, 'summary', s.summary) FROM agent_sessions s
             WHERE s.agent_id = ANY($3) AND s.role = 'main' AND s.status = 'active' AND s.summary IS NOT NULL
               AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL",
        )
        .await?;
        let messages = if has("agent_messages") {
            sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(m) - 'session_id' - 'run_id' - 'n' - 'reply_to' - 'reply_count' - 'last_reply_at' FROM (
                   SELECT m.*, row_number() OVER (PARTITION BY m.agent_id ORDER BY m.created_at DESC) AS n
                   FROM agent_messages m
                   WHERE m.agent_id = ANY($2)
                     AND COALESCE(m.meta->>'thread_user_id', m.author_user_id::text, $1::text) = $1::text
                 ) m WHERE n <= $3 ORDER BY created_at",
            )
            .bind(user)
            .bind(&ids)
            .bind(MESSAGES_PER_AGENT)
            .fetch_all(self.pool())
            .await?
        } else {
            Vec::new()
        };
        let skills = q("world_skills", "SELECT to_jsonb(s) FROM world_skills s WHERE s.created_by = $1 AND s.org_id = $2 AND cardinality($3::uuid[]) >= 0").await?;
        let agent_skills = q(
            "agent_skills",
            "SELECT jsonb_build_object('agent_id', x.agent_id, 'skill_id', x.skill_id, 'name', s.name)
             FROM agent_skills x JOIN world_skills s ON s.id = x.skill_id
             WHERE x.agent_id = ANY($3) AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL",
        )
        .await?;
        let mut mcp_servers = q("world_mcp_servers", "SELECT to_jsonb(s) FROM world_mcp_servers s WHERE s.created_by = $1 AND s.org_id = $2 AND cardinality($3::uuid[]) >= 0").await?;
        for server in &mut mcp_servers {
            if let Some(definition) = server.get_mut("definition") {
                mask_definition(definition);
            }
        }
        let agent_mcp_servers = q(
            "agent_mcp_servers",
            "SELECT jsonb_build_object('agent_id', x.agent_id, 'server_id', x.server_id, 'name', s.name)
             FROM agent_mcp_servers x JOIN world_mcp_servers s ON s.id = x.server_id
             WHERE x.agent_id = ANY($3) AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL",
        )
        .await?;
        let subscriptions = q(
            "agent_subscriptions",
            "SELECT to_jsonb(x) FROM agent_subscriptions x WHERE x.agent_id = ANY($3) AND x.task_id IS NULL AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL",
        )
        .await?;
        let mut omp_configs = q("agent_omp_configs", "SELECT to_jsonb(x) FROM agent_omp_configs x WHERE x.agent_id = ANY($3) AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL").await?;
        omp_configs.retain_mut(|row| {
            let Some(config) = text_of(row, "config").map(str::to_string) else { return false };
            match crate::omp_config::masked(&config) {
                Ok((text, _)) => {
                    row["config"] = Value::String(text);
                    true
                }
                Err(_) => false,
            }
        });
        let schedules = q("schedules", "SELECT to_jsonb(x) FROM schedules x WHERE x.agent_id = ANY($3) AND x.user_id = $1 AND $2::uuid IS NOT NULL").await?;
        let feeds = q(
            "agent_feeds",
            "SELECT to_jsonb(x) - 'secret_ciphertext' - 'secret_nonce' FROM agent_feeds x WHERE x.agent_id = ANY($3) AND $1::uuid IS NOT NULL AND $2::uuid IS NOT NULL",
        )
        .await?;
        let rules = q("world_rules", "SELECT to_jsonb(x) FROM world_rules x WHERE x.user_id = $1 AND (x.org_id IS NULL OR x.org_id = $2) AND cardinality($3::uuid[]) >= 0 ORDER BY x.position").await?;
        let reports = q("world_reports", "SELECT to_jsonb(x) FROM world_reports x WHERE x.owner_user_id = $1 AND x.org_id = $2 AND cardinality($3::uuid[]) >= 0 ORDER BY x.created_at").await?;
        let regions = q("world_regions", "SELECT to_jsonb(x) FROM world_regions x WHERE x.owner_user_id = $1 AND x.org_id = $2 AND cardinality($3::uuid[]) >= 0").await?;
        let items = q(
            "world_items",
            "SELECT to_jsonb(x) FROM world_items x WHERE x.owner_user_id = $1 AND x.org_id = $2
               AND x.status IS DISTINCT FROM 'archived' AND NOT (x.kind = 'timer' AND x.status = 'done')
               AND cardinality($3::uuid[]) >= 0",
        )
        .await?;
        let todos = q("world_todos", "SELECT to_jsonb(x) FROM world_todos x WHERE x.owner_user_id = $1 AND x.org_id = $2 AND x.status = 'open' AND cardinality($3::uuid[]) >= 0").await?;
        let teams = if has("agent_team_members") {
            q(
                "agent_teams",
                "SELECT to_jsonb(t) FROM agent_teams t WHERE t.owner_user_id = $1 AND $2::uuid IS NOT NULL
                   AND (t.lead_agent_id = ANY($3) OR EXISTS (SELECT 1 FROM agent_team_members m WHERE m.team_id = t.id AND m.member_kind = 'agent' AND m.member_id = ANY($3)))",
            )
            .await?
        } else {
            Vec::new()
        };
        let practice_attempts = if has("world_practice_attempts") && has("world_reports") {
            self.export_rows(
                &format!(
                    "SELECT CASE WHEN x.rank <= {STROKES_PER_PRACTICE} THEN x.row
                                 ELSE jsonb_set(x.row, '{{strokes}}', jsonb_build_object('width', x.row->'strokes'->'width', 'height', x.row->'strokes'->'height', 'strokes', '[]'::jsonb)) END
                     FROM (SELECT to_jsonb(p) - 'coach_message_id' AS row, p.report_id, p.n,
                                  row_number() OVER (PARTITION BY p.report_id ORDER BY p.n DESC) AS rank
                           FROM world_practice_attempts p JOIN world_reports r ON r.id = p.report_id
                           WHERE r.owner_user_id = $1 AND r.org_id = $2 AND p.user_id = $1 AND cardinality($3::uuid[]) >= 0) x
                     ORDER BY x.report_id, x.n"
                ),
                user,
                org,
                &ids,
            )
            .await?
        } else {
            Vec::new()
        };
        let practice_coach_prefs = q(
            "world_practice_coach_prefs",
            "SELECT to_jsonb(p) FROM world_practice_coach_prefs p WHERE p.user_id = $1 AND p.agent_id = ANY($3) AND $2::uuid IS NOT NULL",
        )
        .await?;
        let team_ids = ids_of(&teams);
        let team_members = if has("agent_team_members") {
            sqlx::query_scalar::<_, Value>("SELECT to_jsonb(m) FROM agent_team_members m WHERE m.team_id = ANY($1)")
                .bind(&team_ids)
                .fetch_all(self.pool())
                .await?
        } else {
            Vec::new()
        };
        Ok(Bundle {
            format: FORMAT.into(),
            version: VERSION,
            exported_at: Utc::now(),
            source: Source { user_id: user, org_id: org, org_name, main_agent_id, app_url },
            agents,
            memories,
            messages,
            skills,
            agent_skills,
            mcp_servers,
            agent_mcp_servers,
            subscriptions,
            omp_configs,
            schedules,
            feeds,
            rules,
            reports,
            regions,
            items,
            todos,
            teams,
            team_members,
            practice_attempts,
            practice_coach_prefs,
        })
    }

    pub async fn moving_keys(&self, owner: Uuid) -> Result<Vec<MovingKey>> {
        let rows = sqlx::query_as::<_, StoredKey>(
            "SELECT e.owner_user_id, e.name, e.ciphertext, e.nonce, e.vault,
                    COALESCE(array_agg(g.agent_id ORDER BY g.agent_id)
                      FILTER (WHERE g.agent_id IS NOT NULL), ARRAY[]::uuid[]) AS agent_ids
             FROM world_environment e
             LEFT JOIN world_environment_agents g
               ON g.owner_user_id = e.owner_user_id AND g.name = e.name
             WHERE e.owner_user_id = $1
             GROUP BY e.owner_user_id, e.name, e.ciphertext, e.nonce, e.vault
             ORDER BY e.name",
        )
        .bind(owner)
        .fetch_all(self.pool())
        .await?;
        let mut keys = Vec::new();
        for row in rows {
            if row.vault {
                continue;
            }
            let plaintext = open(row.owner_user_id, &row.name, &row.ciphertext, &row.nonce)?;
            let value = String::from_utf8(plaintext).map_err(|_| anyhow!("environment decryption failed"))?;
            if value.trim().is_empty() || is_vault_reference(&value) {
                continue;
            }
            keys.push(MovingKey { name: row.name, value, agents: row.agent_ids });
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_literal_values_but_keeps_secret_refs() {
        let mut definition = json!({
            "command": "npx",
            "env": {"TOKEN": "abc123", "KEY": "${secret:KEY}"},
        });
        assert!(mask_definition(&mut definition));
        assert_eq!(definition["env"]["TOKEN"], json!("***"));
        assert_eq!(definition["env"]["KEY"], json!("${secret:KEY}"));

        let mut remote = json!({
            "command": "npx",
            "args": ["-y", "mcp-remote", "https://mcp.notion.com/mcp"],
            "env": {"MCP_REMOTE_CONFIG_DIR": "/Users/me/.mcp-auth/work", "NOTION_TOKEN": "ntn_x", "API_SECRET": "short"},
        });
        assert!(mask_definition(&mut remote));
        assert_eq!(remote["env"]["MCP_REMOTE_CONFIG_DIR"], json!("/Users/me/.mcp-auth/work"));
        assert_eq!(remote["env"]["NOTION_TOKEN"], json!("***"));
        assert_eq!(remote["env"]["API_SECRET"], json!("***"));
        assert_eq!(remote["args"], json!(["-y", "mcp-remote", "https://mcp.notion.com/mcp"]));

        let mut headers = json!({"url": "https://example.com/mcp", "headers": {"X-Team": "", "Authorization": "Bearer abc"}});
        assert!(mask_definition(&mut headers));
        assert_eq!(headers["headers"]["Authorization"], json!("***"));
        assert_eq!(headers["headers"]["X-Team"], json!(""));

        let mut plain = json!({"command": "uvx", "args": ["server"], "env": {"WORKSPACE": "/Users/me/code"}});
        assert!(!mask_definition(&mut plain));
        assert_eq!(plain["env"]["WORKSPACE"], json!("/Users/me/code"));
    }

    #[test]
    fn plain_settings_are_short_and_never_secret_named() {
        assert!(plain_setting("WORKSPACE_DIR", "/Users/me/code"));
        assert!(plain_setting("LOG_LEVEL", "debug"));
        assert!(!plain_setting("GITHUB_TOKEN", "x"));
        assert!(!plain_setting("db_password", "hunter2"));
        assert!(!plain_setting("SESSION_NAME", "work"));
        assert!(!plain_setting("NOTES", &"a".repeat(513)));
        assert!(!plain_setting("NOTES", "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn moving_keys_never_print_their_value() {
        let key = MovingKey { name: "OPENAI_API_KEY".into(), value: "sk-very-secret".into(), agents: vec![] };
        assert!(!format!("{key:?}").contains("sk-very-secret"));
    }

    #[tokio::test]
    async fn export_bundle_and_keys_for_the_move() -> Result<()> {
        let Ok(url) = std::env::var("PORTABLE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        crate::environment::initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Mover') RETURNING id")
            .bind(format!("portable-{}@example.com", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Someone else') RETURNING id")
            .bind(format!("portable-{}@example.com", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let home = crate::orgs::DEFAULT_ORG;
        for user in [owner, other] {
            sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
                .bind(home)
                .bind(user)
                .execute(&pool)
                .await?;
        }
        let slug = format!("mover-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let agent = store
            .create_agent(crate::world::NewAgent {
                slug: &slug,
                name: "Mover",
                avatar_emoji: "",
                cli: "claude",
                model: "sonnet",
                persona: Some("Calm and careful."),
                role: "worker",
                look: json!({}),
                gear: json!({}),
                visibility: "private",
                owner_user_id: Some(owner),
                org_id: Some(home),
            })
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'Remember the blue door')")
            .bind(agent.id)
            .bind(owner)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_skills (name, body, created_by, org_id) VALUES ($1, 'Likes tea', $2, $3)")
            .bind(format!("{slug}-notes"))
            .bind(owner)
            .bind(home)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_mcp_servers (name, definition, created_by, org_id) VALUES ($1, $2, $3, $4)")
            .bind(format!("{slug}-server"))
            .bind(json!({"command": "npx", "env": {"API_TOKEN": "tok_live_value", "WORKSPACE": "/tmp/work"}}))
            .bind(owner)
            .bind(home)
            .execute(&pool)
            .await?;
        let report = |title: &str, body: &str| crate::reports::NewReport {
            owner_user_id: owner,
            by: crate::reports::ReportActor::User(owner),
            title: title.into(),
            body: body.into(),
            folder: None,
            tags: vec![],
            visibility: None,
            pinned: false,
            task_id: None,
        };
        store.create_report(report("Plan", "Body")).await?;
        store.create_report(report("Index", "See [[Plan]].")).await?;

        store.save_environment(owner, "OPENAI_API_KEY", "sk-test-value").await?;
        store.set_environment_agents(owner, "OPENAI_API_KEY", &[agent.id]).await?;
        store.save_environment(owner, "VAULTED_KEY", "op://Private/Item/credential").await?;
        store.save_environment(other, "OTHER_KEY", "not-yours").await?;

        let bundle = store.export_bundle(owner, home, Some("http://127.0.0.1:3000".into())).await?;
        assert_eq!((bundle.format.as_str(), bundle.version), (FORMAT, VERSION));
        assert_eq!(bundle.source.user_id, owner);
        assert_eq!(bundle.agents.len(), 1);
        assert_eq!(bundle.reports.len(), 2);
        assert_eq!(bundle.messages.len(), 1);
        assert_eq!(bundle.skills.len(), 1);
        assert_eq!(bundle.mcp_servers.len(), 1);
        assert_eq!(bundle.mcp_servers[0]["definition"]["env"]["API_TOKEN"], json!("***"));
        assert_eq!(bundle.mcp_servers[0]["definition"]["env"]["WORKSPACE"], json!("/tmp/work"));
        assert!(bundle.messages[0].get("session_id").is_none());
        let text = serde_json::to_string(&bundle)?;
        assert!(!text.contains("tok_live_value"));
        let back: Bundle = serde_json::from_str(&text)?;
        assert_eq!(back.agents.len(), 1);

        let theirs = store.export_bundle(other, home, None).await?;
        assert!(theirs.agents.is_empty() && theirs.reports.is_empty() && theirs.skills.is_empty());

        let keys = store.moving_keys(owner).await?;
        assert_eq!(keys, vec![MovingKey { name: "OPENAI_API_KEY".into(), value: "sk-test-value".into(), agents: vec![agent.id] }]);

        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE created_by = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_mcp_servers WHERE created_by = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, other]).execute(&pool).await?;
        Ok(())
    }
}
