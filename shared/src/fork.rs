use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::environment::secret_reference_names;
use crate::world::WorldStore;

const MEMORY_BUDGET: usize = 60_000;

pub struct Fork {
    pub agent_id: Uuid,
    pub wanted_keys: Vec<String>,
}

fn fork_memory(source: &str, by: &str, thread: &[(String, String)], quests: &[(String, String, Option<String>)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for (who, body) in thread.iter().rev() {
        let line = format!("{who}: {}", body.trim());
        used += line.len();
        if used > MEMORY_BUDGET && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    lines.reverse();
    let quests = quests
        .iter()
        .map(|(title, status, result)| match result.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => format!("- {title} ({status}): {}", r.chars().take(400).collect::<String>()),
            None => format!("- {title} ({status})"),
        })
        .collect::<Vec<_>>();
    format!(
        "You are a fork of {source}: {by} copied {source} so they have their own private version. You keep {source}'s personality, skills and tools; the original still lives on separately and is not you. Environment keys did not come along, so ask {by} to add any you need. What you remember from {source}'s conversations, oldest first:\n\n{}\n\n{source}'s recent quests:\n{}",
        if lines.is_empty() { "(nothing yet)".to_string() } else { lines.join("\n") },
        if quests.is_empty() { "(none)".to_string() } else { quests.join("\n") }
    )
}

impl WorldStore {
    pub async fn fork_agent(&self, source: Uuid, owner: Uuid, slug: &str, name: &str, by: &str) -> Result<Option<Fork>> {
        let mut tx = self.pool().begin().await?;
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT name, cli, model FROM agent_profiles WHERE id = $1",
        )
        .bind(source)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((source_name, cli, model)) = row else {
            return Ok(None);
        };
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles
               (slug, name, avatar_emoji, cli, model, persona, role, look, gear, visibility, owner_user_id, tags, expertise, revoked_powers)
             SELECT $2, $3, avatar_emoji, cli, model, persona, 'worker', look, gear, 'private', $4, tags, expertise, revoked_powers
             FROM agent_profiles WHERE id = $1
             RETURNING id",
        )
        .bind(source)
        .bind(slug)
        .bind(name)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        let servers: Vec<(Uuid, String, Option<Uuid>, Value)> = sqlx::query_as(
            "SELECT s.id, s.scope, s.created_by, s.definition
             FROM agent_mcp_servers m JOIN world_mcp_servers s ON s.id = m.server_id WHERE m.agent_id = $1",
        )
        .bind(source)
        .fetch_all(&mut *tx)
        .await?;
        let copied: Vec<Uuid> = servers
            .into_iter()
            .filter(|(_, scope, created_by, definition)| {
                scope == "global" || *created_by == Some(owner) || crate::world::keyless_definition(definition)
            })
            .map(|(id, ..)| id)
            .collect();
        sqlx::query("INSERT INTO agent_mcp_servers (agent_id, server_id) SELECT $1, UNNEST($2::uuid[])")
            .bind(agent_id)
            .bind(&copied)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO agent_skills (agent_id, skill_id) SELECT $2, a.skill_id FROM agent_skills a JOIN world_skills s ON s.id = a.skill_id
             WHERE a.agent_id = $1 AND NOT (s.scope = 'agent' AND s.name LIKE '%-notes')")
            .bind(source)
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        let definitions: Vec<Value> = sqlx::query_scalar(
            "SELECT s.definition FROM agent_mcp_servers m JOIN world_mcp_servers s ON s.id = m.server_id WHERE m.agent_id = $1",
        )
        .bind(agent_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut wanted = BTreeSet::new();
        for definition in &definitions {
            if let Ok(names) = secret_reference_names(definition) {
                wanted.extend(names);
            }
        }
        let wanted: Vec<String> = wanted.into_iter().collect();
        sqlx::query("UPDATE agent_profiles SET wanted_keys = $2 WHERE id = $1")
            .bind(agent_id)
            .bind(&wanted)
            .execute(&mut *tx)
            .await?;
        let thread: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(&format!(
            "SELECT CASE WHEN m.author_kind = 'agent' AND (m.author_agent_id IS NULL OR m.author_agent_id = m.agent_id) THEN $2
                         WHEN m.author_kind = 'agent' THEN COALESCE(o.name, 'another agent')
                         ELSE COALESCE(u.name, 'someone') END,
                    m.body, m.created_at
             FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id
             LEFT JOIN users u ON u.id = m.author_user_id
             LEFT JOIN agent_profiles o ON o.id = m.author_agent_id
             WHERE m.agent_id = $1 AND COALESCE(s.role, 'main') <> 'aside' AND m.body <> '' AND {}
               AND (EXISTS (SELECT 1 FROM agent_profiles p WHERE p.id = $1 AND p.owner_user_id = $3) OR (
                       COALESCE(m.meta->>'kind', '') <> 'shell'
                       AND COALESCE(m.meta->>'shell_command', 'false') <> 'true'
                       AND COALESCE(m.meta->>'shell_share', 'false') <> 'true'))
             ORDER BY m.created_at DESC LIMIT 200",
            crate::guests::thread_filter("$1", "$3")
        ))
        .bind(source)
        .bind(format!("you ({source_name})"))
        .bind(owner)
        .fetch_all(&mut *tx)
        .await?;
        let thread: Vec<(String, String)> = thread.into_iter().rev().map(|(who, body, _)| (who, body)).collect();
        let quests: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT title, status, result_content FROM agent_tasks WHERE assigned_agent_id = $1 ORDER BY updated_at DESC LIMIT 15",
        )
        .bind(source)
        .fetch_all(&mut *tx)
        .await?;
        let memory = fork_memory(&source_name, by, &thread, &quests);
        sqlx::query(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, summary, usage_baseline)
             VALUES ($1, 'main', $2, $3, 'active', $4, $5)",
        )
        .bind(agent_id)
        .bind(&cli)
        .bind(&model)
        .bind(&memory)
        .bind(json!({}))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(Fork { agent_id, wanted_keys: wanted }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fork_copies_profile_kit_and_memory() -> Result<()> {
        let Ok(url) = std::env::var("TRANSFER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let ana: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("ana-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let source: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, persona, visibility, tags)
             VALUES ($1, 'Kimmy', 'codex', 'gpt-5.4', 'A nosy magpie', 'public', ARRAY['slack']) RETURNING id",
        )
        .bind(format!("kimmy-{tag}"))
        .fetch_one(&pool)
        .await?;
        let server: Uuid = sqlx::query_scalar("INSERT INTO world_mcp_servers (name, definition) VALUES ($1, $2) RETURNING id")
            .bind(format!("slack-{tag}"))
            .bind(json!({ "command": "slack", "env": { "TOKEN": "${secret:SLACK_TOKEN}" } }))
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_mcp_servers (agent_id, server_id) VALUES ($1, $2)").bind(source).bind(server).execute(&pool).await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'watch my slack')")
            .bind(source)
            .bind(ana)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, body, meta) VALUES ($1, 'agent', 'On it', $2)")
            .bind(source)
            .bind(json!({ "thread_user_id": ana }))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, body) VALUES ($1, 'agent', 'someone else thread')").bind(source).execute(&pool).await?;

        let fork = store.fork_agent(source, ana, &format!("kimmy-jr-{tag}"), "Kimmy Jr", "Ana").await?.expect("fork");
        assert_eq!(fork.wanted_keys, vec!["SLACK_TOKEN".to_string()]);
        let (owner, visibility, cli, persona, tags): (Uuid, String, String, Option<String>, Vec<String>) = sqlx::query_as(
            "SELECT owner_user_id, visibility, cli, persona, tags FROM agent_profiles WHERE id = $1",
        )
        .bind(fork.agent_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!((owner, visibility.as_str(), cli.as_str()), (ana, "private", "codex"));
        assert_eq!(persona.as_deref(), Some("A nosy magpie"));
        assert_eq!(tags, vec!["slack".to_string()]);
        let servers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_mcp_servers WHERE agent_id = $1").bind(fork.agent_id).fetch_one(&pool).await?;
        assert_eq!(servers, 1);
        let summary: Option<String> = sqlx::query_scalar("SELECT summary FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active'")
            .bind(fork.agent_id)
            .fetch_one(&pool)
            .await?;
        let summary = summary.unwrap_or_default();
        assert!(summary.contains("Ana: watch my slack"));
        assert!(summary.contains("you (Kimmy): On it"));
        assert!(!summary.contains("someone else thread"), "a fork only copies the forker's own thread");
        assert_eq!(store.wanted_keys(ana).await?[0].keys, vec!["SLACK_TOKEN".to_string()]);

        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![source, fork.agent_id]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_mcp_servers WHERE id = $1").bind(server).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(ana).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn memory_keeps_newest_lines_within_budget() {
        let thread: Vec<(String, String)> = (0..3000).map(|i| ("Ana".to_string(), format!("message {i} {}", "x".repeat(40)))).collect();
        let memory = fork_memory("Kimmy", "Ana", &thread, &[("Fix login".into(), "done".into(), Some("Merged".into()))]);
        assert!(memory.contains("message 2999"));
        assert!(!memory.contains("message 0 "));
        assert!(memory.contains("- Fix login (done): Merged"));
        assert!(memory.len() < MEMORY_BUDGET + 2_000);
    }
}
