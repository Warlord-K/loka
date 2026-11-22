use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{AgentMessage, AgentSession, AsideChoice, NewEvent, NewMessage, SessionRun, WorldAgent, WorldStore, NOTES_MAX_BYTES};

pub const ROLE: &str = "quick";
pub const TIMEOUT_SECS: i32 = 1800;
pub const MAX_TURNS: u32 = 80;
pub const KEEP: &str = "Remember";
pub const SKIP: &str = "Not now";
pub const MEMORY_LINE_MAX: usize = 300;
const MEMORY_SECTION: &str = "## Remembered from quick chats";
const THREAD_CONTEXT: i64 = 16;
const RUN_COLUMNS: &str = "id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                           timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                           created_at, started_at, finished_at";

pub fn thread_root(session: &AgentSession) -> Option<Uuid> {
    session.asked_by.as_ref()?.get("thread_root")?.as_str()?.parse().ok()
}

pub fn switched_cli(session: &AgentSession) -> bool {
    session.asked_by.as_ref().and_then(|a| a.get("switched")).and_then(Value::as_bool) == Some(true)
}

pub fn asker(session: &AgentSession) -> Option<Uuid> {
    session.asked_by.as_ref().and_then(crate::guests::asked_by_user)
}

pub fn asker_name(session: &AgentSession) -> String {
    session
        .asked_by
        .as_ref()
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Your person")
        .to_string()
}

pub fn memory_line(text: &str) -> Result<String> {
    let line = text.replace('\u{2014}', " - ").split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim().trim_start_matches(['-', '*', ' ']).trim().to_string();
    if line.is_empty() {
        bail!("say what to remember in one short line");
    }
    if line.chars().count() > MEMORY_LINE_MAX {
        bail!("keep it to one line under {MEMORY_LINE_MAX} characters");
    }
    Ok(line)
}

fn title_of(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > 60 { format!("{}...", text.chars().take(57).collect::<String>().trim_end()) } else { text.to_string() })
}

pub fn ground_rules(asker: &str) -> String {
    format!(
        "This is a side conversation from a copy of your memory: your main conversation goes on without you, and nothing here is kept. Answer everything {asker} asks. Your notes cannot be changed from here, so never call update_my_notes; when something is worth keeping for later, call offer_memory with one short line and {asker} decides with a button. Do small things like reminders or lists right away and say so; ask before anything bigger. Write your whole answer in your last message, after any tool calls: text written before a tool call is not shown."
    )
}

pub fn opening_prompt(asker: &str, text: &str, attached: &[String], memory: Option<&str>) -> String {
    let memory = memory.map(|m| format!("\n\n{m}")).unwrap_or_default();
    format!("[quick chat] {asker} opened a quick chat with you. {}{memory}\n\n{}", ground_rules(asker), next_prompt(asker, text, attached))
}

pub fn thread_prompt(asker: &str, transcript: &str, text: &str, attached: &[String], memory: Option<&str>) -> String {
    let memory = memory.map(|m| format!("\n\n{m}")).unwrap_or_default();
    format!(
        "[thread] {asker} replied in a thread in your chat. {}{memory}\n\nThe thread so far, starting with the message it hangs off:\n{transcript}\n\n{}",
        ground_rules(asker),
        next_prompt(asker, text, attached)
    )
}

pub fn with_quote(text: &str, quote: Option<&Value>) -> String {
    match quote {
        Some(quote) => format!("{}{text}", crate::quotes::render(&json!({ "quote": quote }))),
        None => text.to_string(),
    }
}

pub fn next_prompt(asker: &str, text: &str, attached: &[String]) -> String {
    let files = if attached.is_empty() { String::new() } else { format!("\n[attached: {}]", attached.join(", ")) };
    format!("{asker}: {text}{files}")
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct QuickBrief {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub title: Option<String>,
    pub backend: String,
    pub model: String,
    pub status: String,
    pub thread_root: Option<String>,
    pub busy: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct Opening<'a> {
    pub agent_id: Uuid,
    pub user_id: Uuid,
    pub asker: &'a str,
    pub text: &'a str,
    pub choice: &'a AsideChoice,
    pub thread_root: Option<Uuid>,
}

pub enum MemoryAnswer {
    Kept(String),
    Skipped,
    AlreadyAnswered,
}

impl WorldStore {
    pub async fn open_quick(&self, o: Opening<'_>) -> Result<AgentSession> {
        let mut tx = self.pool().begin().await?;
        let (cli, model) = sqlx::query_as::<_, (String, String)>("SELECT cli, model FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(o.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        if let Some(root) = o.thread_root {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
                .bind(format!("quick-thread:{}:{root}", o.agent_id))
                .execute(&mut *tx)
                .await?;
            let existing = sqlx::query_as::<_, AgentSession>(
                "SELECT * FROM agent_sessions
                 WHERE agent_id = $1 AND role = 'quick' AND status <> 'archived' AND asked_by->>'thread_root' = $2",
            )
            .bind(o.agent_id)
            .bind(root.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(session) = existing {
                tx.commit().await?;
                return Ok(session);
            }
        }
        let main = sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions WHERE agent_id = $1 AND role = 'main' AND status = 'active' FOR SHARE",
        )
        .bind(o.agent_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("this agent has not started yet; message them once first"))?;
        let switches = o.choice.cli.as_deref().is_some_and(|c| c != main.backend);
        if switches && o.choice.model.is_none() {
            bail!("pick a model for {}", o.choice.cli.as_deref().unwrap_or_default());
        }
        let (backend, model, baseline) = if switches {
            (o.choice.cli.clone().unwrap_or(cli), o.choice.model.clone().unwrap_or(model), json!({}))
        } else {
            (main.backend.clone(), o.choice.model.clone().unwrap_or_else(|| main.model.clone()), main.usage_baseline.clone())
        };
        let mut asked_by = json!({ "kind": "user", "user_id": o.user_id, "name": o.asker, "quick": true });
        if switches {
            asked_by["switched"] = json!(true);
        }
        if let Some(root) = o.thread_root {
            asked_by["thread_root"] = json!(root.to_string());
        }
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions
               (agent_id, role, backend, model, parent_session_id, working_dir, usage_baseline, asked_by, title)
             VALUES ($1, 'quick', $2, $3, $4, $5, $6, $7, $8)
             RETURNING *",
        )
        .bind(o.agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(main.id)
        .bind(main.working_dir.as_deref())
        .bind(&baseline)
        .bind(&asked_by)
        .bind(title_of(o.text).unwrap_or_else(|| "Quick chat".to_string()))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    pub async fn quick_device_problem(&self, agent_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String, Option<Uuid>, Option<bool>, Option<bool>, Option<String>)>(
            "SELECT a.name, a.runner_id, w.owner_user_id = a.owner_user_id,
                    COALESCE((w.capabilities->>'quick')::boolean, FALSE), w.name
             FROM agent_profiles a LEFT JOIN world_runners w ON w.id = a.runner_id
             WHERE a.id = $1",
        )
        .bind(agent_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| anyhow::anyhow!("agent not found"))?;
        let (agent, runner, own, capable, device) = row;
        if runner.is_none() {
            let worker = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE application_name IN ($1, 'karna-world') AND datname = current_database())",
            )
            .bind(crate::runners::WORLD_WORKER_APP)
            .fetch_one(self.pool())
            .await?;
            return Ok((!worker).then(|| format!("{agent} has no device yet, so a quick chat has nowhere to run. Pick a device in Devices first.")));
        }
        if own != Some(true) {
            return Ok(Some(format!("{agent} runs on a shared device. Quick chats run only on your own devices.")));
        }
        if capable != Some(true) {
            let device = device.unwrap_or_else(|| "its device".to_string());
            return Ok(Some(format!("{device} runs an older Loka. Update the app there to use quick chats with {agent}.")));
        }
        Ok(None)
    }

    pub async fn quick_owner_live(&self, session_id: Uuid, agent_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM agent_sessions s JOIN agent_profiles a ON a.id = s.agent_id
               WHERE s.id = $1 AND s.agent_id = $2 AND s.role = 'quick' AND s.status <> 'archived'
                 AND s.asked_by->>'user_id' = a.owner_user_id::text
                 AND EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status = 'running'))",
        )
        .bind(session_id)
        .bind(agent_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn quick_thread_session(&self, agent_id: Uuid, root: Uuid) -> Result<Option<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "SELECT * FROM agent_sessions
             WHERE agent_id = $1 AND role = 'quick' AND status <> 'archived' AND asked_by->>'thread_root' = $2",
        )
        .bind(agent_id)
        .bind(root.to_string())
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn session_busy(&self, session_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM session_runs WHERE session_id = $1 AND status IN ('queued', 'running'))")
            .bind(session_id)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn quick_has_started(&self, session_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM session_runs WHERE session_id = $1)")
            .bind(session_id)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn quick_memory_note(&self, session: &AgentSession) -> Result<Option<String>> {
        if !switched_cli(session) {
            return Ok(None);
        }
        Ok(Some(self.handoff_memory(session.agent_id, "You are on another model here, so you only have these notes from your main mind, not your full memory.").await?))
    }

    pub async fn queue_quick(&self, session: &AgentSession, prompt: &str) -> Result<SessionRun> {
        let mut tx = self.pool().begin().await?;
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_sessions WHERE id = $1 AND role = 'quick' FOR UPDATE")
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow::anyhow!("quick chat not found"))?;
        if status == "archived" {
            bail!("this quick chat is closed; start a new one");
        }
        let run = sqlx::query_as::<_, SessionRun>(&format!(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, timeout_secs)
             VALUES ($1, $2, 'quick', 'queued', $3, $4)
             RETURNING {RUN_COLUMNS}"
        ))
        .bind(session.id)
        .bind(session.agent_id)
        .bind(prompt)
        .bind(TIMEOUT_SECS)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'active', updated_at = NOW() WHERE id = $1")
            .bind(session.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(run)
    }

    pub async fn quick_fork_source(&self, session: &AgentSession) -> Result<Option<(Uuid, String)>> {
        if session.role != ROLE || session.cli_session_id.is_some() || switched_cli(session) {
            return Ok(None);
        }
        let Some(parent) = session.parent_session_id else {
            return Ok(None);
        };
        Ok(sqlx::query_as::<_, (Uuid, Option<String>)>("SELECT id, cli_session_id FROM agent_sessions WHERE id = $1")
            .bind(parent)
            .fetch_optional(self.pool())
            .await?
            .and_then(|(id, cli)| cli.map(|cli| (id, cli))))
    }

    pub async fn quick_thread_transcript(&self, agent_id: Uuid, root: Uuid) -> Result<String> {
        self.agent_thread_transcript(agent_id, root, THREAD_CONTEXT).await
    }

    pub async fn post_quick_message(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        author_user: Option<Uuid>,
        body: &str,
        run_id: Option<Uuid>,
        meta: Value,
    ) -> Result<AgentMessage> {
        let author_kind = match author_user {
            Some(_) => "user",
            None if meta.get("system").and_then(Value::as_bool) == Some(true) => "system",
            None => "agent",
        };
        let mut meta = crate::guests::in_thread(meta, asker(session));
        meta["quick"] = json!(session.id);
        let root = thread_root(session);
        let msg = self
            .insert_reply(
                NewMessage {
                    agent_id: agent.id,
                    author_kind,
                    author_user_id: author_user,
                    author_agent_id: (author_kind == "agent").then_some(agent.id),
                    body,
                    session_id: Some(session.id),
                    run_id,
                    meta: meta.clone(),
                },
                root,
            )
            .await?;
        sqlx::query("UPDATE agent_sessions SET updated_at = NOW() WHERE id = $1")
            .bind(session.id)
            .execute(self.pool())
            .await?;
        self.emit(
            NewEvent::new("message.created")
                .agent(agent.id)
                .actor(author_user)
                .session(session.id)
                .payload(crate::guests::in_thread(
                    json!({
                        "message_id": msg.id,
                        "author": if author_kind == "agent" { agent.name.clone() } else { asker_name(session) },
                        "author_kind": author_kind,
                        "body": crate::links::excerpt(body, 400),
                        "reply_to": root,
                        "quick_session_id": session.id,
                        "quiet": true,
                    }),
                    asker(session),
                )),
            None,
        )
        .await?;
        Ok(msg)
    }

    pub async fn record_quick_reply(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        text: &str,
        failed: bool,
    ) -> Result<Option<AgentMessage>> {
        let posted = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_messages WHERE run_id = $1 AND author_kind <> 'user')",
        )
        .bind(run_id)
        .fetch_one(self.pool())
        .await?;
        let text = text.trim();
        if posted || text.is_empty() {
            return Ok(None);
        }
        let meta = if failed { json!({ "failed": true }) } else { json!({}) };
        let msg = self.post_quick_message(agent, session, None, text, Some(run_id), meta).await?;
        sqlx::query("UPDATE agent_sessions SET summary = $2 WHERE id = $1")
            .bind(session.id)
            .bind(crate::links::excerpt(text, 2000))
            .execute(self.pool())
            .await?;
        Ok(Some(msg))
    }

    pub async fn offer_memory(&self, agent: &WorldAgent, session_id: Uuid, line: &str) -> Result<AgentMessage> {
        let line = memory_line(line)?;
        let session = self
            .get_session(session_id)
            .await?
            .filter(|s| s.role == ROLE && s.agent_id == agent.id)
            .ok_or_else(|| anyhow::anyhow!("offer_memory works only in a quick chat or thread"))?;
        let open = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_messages
             WHERE session_id = $1 AND meta ? 'memory' AND NOT meta ? 'memory_answered'",
        )
        .bind(session.id)
        .fetch_one(self.pool())
        .await?;
        if open >= 3 {
            bail!("{} has not answered your last offers yet; wait for them", asker_name(&session));
        }
        self.post_quick_message(
            agent,
            &session,
            None,
            &line,
            None,
            json!({ "kind": "ask", "choices": [KEEP, SKIP], "memory": { "line": line } }),
        )
        .await
    }

    pub async fn answer_memory(&self, agent: &WorldAgent, owner: Uuid, card: Uuid, keep: bool) -> Result<MemoryAnswer> {
        let claimed = sqlx::query_scalar::<_, String>(
            "UPDATE agent_messages SET meta = meta || jsonb_build_object('memory_answered', $3::text)
             WHERE id = $1 AND agent_id = $2 AND meta ? 'memory' AND NOT meta ? 'memory_answered'
             RETURNING meta->'memory'->>'line'",
        )
        .bind(card)
        .bind(agent.id)
        .bind(if keep { "kept" } else { "skipped" })
        .fetch_optional(self.pool())
        .await?;
        let Some(line) = claimed else {
            return Ok(MemoryAnswer::AlreadyAnswered);
        };
        if !keep {
            return Ok(MemoryAnswer::Skipped);
        }
        if let Err(error) = self.remember_line(agent, owner, &line).await {
            sqlx::query("UPDATE agent_messages SET meta = meta - 'memory_answered' WHERE id = $1")
                .bind(card)
                .execute(self.pool())
                .await?;
            return Err(error);
        }
        Ok(MemoryAnswer::Kept(line))
    }

    pub async fn is_memory_card(&self, agent_id: Uuid, card: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_messages WHERE id = $1 AND agent_id = $2 AND meta ? 'memory')")
            .bind(card)
            .bind(agent_id)
            .fetch_one(self.pool())
            .await?)
    }

    async fn remember_line(&self, agent: &WorldAgent, owner: Uuid, line: &str) -> Result<()> {
        let entry = format!("- {line}");
        let (existing, name, body) = match self.own_notes_skill(agent.id, Some(owner)).await? {
            Some(skill) => {
                let body = with_memory(&skill.body, &entry);
                (Some(skill.id), skill.name, body)
            }
            None => {
                let name = format!("{}-notes", agent.slug);
                let description = format!("{}'s working notes, read before every job: role, people, open threads and lessons.", agent.name);
                let body = format!("---\nname: {name}\ndescription: {description}\n---\n# {}'s notes\n\n{MEMORY_SECTION}\n{entry}\n", agent.name);
                (None, name, body)
            }
        };
        if body.len() > NOTES_MAX_BYTES {
            bail!("{}'s notes are full, so nothing was saved; tidy them first", agent.name);
        }
        let description = crate::world::check_notes(&name, &body)?;
        let skill = self.save_own_notes(agent.id, Some(owner), existing, &name, &description, &body).await?;
        self.emit(
            NewEvent::new("skill.updated")
                .agent(agent.id)
                .payload(json!({ "skill": skill.name, "skill_id": skill.id, "created": existing.is_none(), "bytes": body.len(), "quiet": true })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn quick_sessions(&self, user: Uuid, org: Uuid, agent: Option<Uuid>, threads: bool, limit: i64) -> Result<Vec<QuickBrief>> {
        Ok(sqlx::query_as::<_, QuickBrief>(
            "SELECT s.id, s.agent_id, s.title, s.backend, s.model, s.status, s.asked_by->>'thread_root' AS thread_root,
                    EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')) AS busy,
                    s.created_at, s.updated_at
             FROM agent_sessions s
             JOIN agent_profiles a ON a.id = s.agent_id
             WHERE s.role = 'quick' AND s.status <> 'archived'
               AND s.asked_by->>'user_id' = $1::text AND a.org_id = $2 AND a.owner_user_id = $1
               AND ($3::uuid IS NULL OR s.agent_id = $3)
               AND (s.asked_by ? 'thread_root') = $4
             ORDER BY s.updated_at DESC LIMIT $5",
        )
        .bind(user)
        .bind(org)
        .bind(agent)
        .bind(threads)
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn quick_session_for(&self, user: Uuid, org: Uuid, session_id: Uuid) -> Result<Option<AgentSession>> {
        Ok(sqlx::query_as::<_, AgentSession>(
            "SELECT s.* FROM agent_sessions s JOIN agent_profiles a ON a.id = s.agent_id
             WHERE s.id = $1 AND s.role = 'quick' AND s.asked_by->>'user_id' = $2::text
               AND a.owner_user_id = $2 AND a.org_id = $3",
        )
        .bind(session_id)
        .bind(user)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn quick_messages(&self, session_id: Uuid, limit: i64) -> Result<Vec<AgentMessage>> {
        Ok(sqlx::query_as::<_, AgentMessage>(
            "SELECT * FROM (
               SELECT m.*, s.role AS session_role FROM agent_messages m
               LEFT JOIN agent_sessions s ON s.id = m.session_id
               WHERE m.session_id = $1
               ORDER BY m.created_at DESC LIMIT $2
             ) r ORDER BY r.created_at",
        )
        .bind(session_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn close_quick(&self, session_id: Uuid) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("UPDATE agent_sessions SET status = 'archived', updated_at = NOW() WHERE id = $1 AND role = 'quick'")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE session_runs SET status = 'cancelled', finished_at = NOW(), error = 'Quick chat closed'
             WHERE session_id = $1 AND status = 'queued'",
        )
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE session_runs SET stop_requested_by = 'Loka'
             WHERE session_id = $1 AND status = 'running' AND stop_requested_by IS NULL",
        )
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn with_memory(body: &str, entry: &str) -> String {
    let body = body.trim_end();
    match body.find(MEMORY_SECTION) {
        Some(start) => {
            let after = start + MEMORY_SECTION.len();
            let end = body[after..].find("\n## ").map(|i| after + i).unwrap_or(body.len());
            let section = body[..end].trim_end();
            let rest = &body[end..];
            if rest.is_empty() {
                format!("{section}\n{entry}\n")
            } else {
                format!("{section}\n{entry}\n{rest}\n")
            }
        }
        None => format!("{body}\n\n{MEMORY_SECTION}\n{entry}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_goes_under_its_own_heading() {
        let notes = "---\nname: a-notes\n---\n# A\n\n## People\n- Sam likes tea";
        let once = with_memory(notes, "- first");
        assert!(once.ends_with("## Remembered from quick chats\n- first\n"));
        let twice = with_memory(&once, "- second");
        assert!(twice.contains("- first\n- second\n"));
        let middle = "# A\n\n## Remembered from quick chats\n- first\n\n## People\n- Sam";
        let added = with_memory(middle, "- second");
        assert!(added.contains("- first\n- second\n\n## People\n- Sam"));
    }

    #[tokio::test]
    async fn quick_chats_fork_answer_and_remember_only_on_yes() -> Result<()> {
        let Ok(url) = std::env::var("QUICK_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(3).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Sam', 'write') RETURNING id")
            .bind(format!("quick-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Quill', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("quill-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, working_dir, status)
             VALUES ($1, 'main', 'claude', 'haiku', 'main-cli', $2, 'active') RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("agents/quill-{tag}"))
        .fetch_one(&pool)
        .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(agent_id).fetch_one(&pool).await?;
        let choice = AsideChoice::default();

        let session = store
            .open_quick(Opening { agent_id, user_id: owner, asker: "Sam", text: "Any good anime this season?", choice: &choice, thread_root: None })
            .await?;
        assert_eq!(session.role, ROLE);
        assert_eq!(session.parent_session_id, Some(main));
        assert_eq!(session.title.as_deref(), Some("Any good anime this season?"));
        assert_eq!(store.quick_fork_source(&session).await?, Some((main, "main-cli".to_string())));

        let asked = store.post_quick_message(&agent, &session, Some(owner), "Any good anime this season?", None, json!({})).await?;
        let run = store.queue_quick(&session, "[quick chat] Sam: Any good anime this season?").await?;
        assert_eq!((run.kind.as_str(), run.fork_from), ("quick", None));
        assert!(!store.quick_owner_live(session.id, agent_id).await?, "no run is working yet");
        sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1").bind(run.id).execute(&pool).await?;
        assert!(store.quick_owner_live(session.id, agent_id).await?);

        let main_chat = store.list_messages(agent_id, owner, 50).await?;
        assert!(main_chat.iter().all(|m| m.id != asked.id), "quick chats stay out of the main chat");

        let reply = store.record_quick_reply(&agent, &session, run.id, "Try Frieren.", false).await?;
        assert!(reply.is_some());
        assert!(store.record_quick_reply(&agent, &session, run.id, "Try Frieren.", false).await?.is_none(), "one reply per run");
        let messages = store.quick_messages(session.id, 50).await?;
        assert_eq!(messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), ["Any good anime this season?", "Try Frieren."]);

        let card = store.offer_memory(&agent, session.id, "Sam likes slow, cozy fantasy anime").await?;
        assert!(store.is_memory_card(agent_id, card.id).await?);
        assert!(store.own_notes_skill(agent_id, Some(owner)).await?.is_none(), "nothing is saved before the yes");
        assert!(matches!(store.answer_memory(&agent, owner, card.id, true).await?, MemoryAnswer::Kept(_)));
        assert!(matches!(store.answer_memory(&agent, owner, card.id, true).await?, MemoryAnswer::AlreadyAnswered));
        let notes = store.own_notes_skill(agent_id, Some(owner)).await?.expect("notes");
        assert!(notes.body.contains("## Remembered from quick chats\n- Sam likes slow, cozy fantasy anime"));
        let skipped = store.offer_memory(&agent, session.id, "Sam is tired today").await?;
        assert!(matches!(store.answer_memory(&agent, owner, skipped.id, false).await?, MemoryAnswer::Skipped));
        assert!(!store.own_notes_skill(agent_id, Some(owner)).await?.expect("notes").body.contains("tired"));

        let root = store
            .insert_message(NewMessage {
                agent_id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent_id),
                body: "Shipped the fix.",
                session_id: Some(main),
                run_id: None,
                meta: json!({}),
            })
            .await?;
        let thread = store
            .open_quick(Opening { agent_id, user_id: owner, asker: "Sam", text: "Which PR?", choice: &choice, thread_root: Some(root.id) })
            .await?;
        let again = store
            .open_quick(Opening { agent_id, user_id: owner, asker: "Sam", text: "And tests?", choice: &choice, thread_root: Some(root.id) })
            .await?;
        assert_eq!(thread.id, again.id, "one fork per thread");
        assert_eq!(thread_root(&thread), Some(root.id));
        let threaded = store.post_quick_message(&agent, &thread, Some(owner), "Which PR?", None, json!({})).await?;
        assert_eq!(threaded.reply_to, Some(root.id));
        assert!(store.quick_thread_transcript(agent_id, root.id).await?.contains("Which PR?"));

        assert_eq!(store.quick_sessions(owner, org, None, false, 10).await?.len(), 1);
        assert_eq!(store.quick_sessions(owner, org, None, true, 10).await?.len(), 1);
        assert!(store.quick_session_for(Uuid::new_v4(), org, session.id).await?.is_none(), "only the asker reads it");
        let colleague: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Kit', 'write') RETURNING id")
            .bind(format!("quick-kit-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, colleague, None).await?;
        assert!(store.quick_session_for(colleague, org, session.id).await?.is_none(), "a teammate in the same org cannot read it either");
        let quote = crate::quotes::clean(Some(&json!({ "text": "Frieren", "label": "Quill's message", "source": format!("msg:{}", reply.as_ref().expect("reply").id) }))).unwrap();
        let quoted = store.post_quick_message(&agent, &session, Some(owner), "More like it?", None, json!({ "quote": quote })).await?;
        assert_eq!(quoted.meta["quote"]["text"], "Frieren");
        assert!(store.quick_messages(session.id, 50).await?.iter().any(|m| m.id == quoted.id && m.meta["quote"]["label"] == "Quill's message"));

        let queued = store.queue_quick(&session, "Sam: more?").await?;
        store.close_quick(session.id).await?;
        let status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1").bind(queued.id).fetch_one(&pool).await?;
        assert_eq!(status, "cancelled");
        assert!(store.queue_quick(&session, "Sam: still there?").await.is_err());
        assert!(store.quick_sessions(owner, org, None, false, 10).await?.is_empty());
        Ok(())
    }

    #[test]
    fn a_quote_leads_the_prompt_text_and_plain_text_is_unchanged() {
        assert_eq!(with_quote("why?", None), "why?");
        let quote = crate::quotes::clean(Some(&json!({ "text": "cache misses doubled", "label": "Quill's message", "source": "msg:abcdef12" }))).unwrap();
        let text = with_quote("why?", Some(&quote));
        assert!(text.contains("from Quill's message (msg:abcdef12)") && text.contains("> cache misses doubled") && text.ends_with("\n\nwhy?"));
        let prompt = next_prompt("Sam", &text, &[]);
        assert!(prompt.starts_with("Sam: (they selected and quoted this") && prompt.ends_with("why?"));
    }

    #[test]
    fn memory_lines_stay_short_and_plain() {
        assert_eq!(memory_line("  - Prefers   short answers \u{2014} always ").unwrap(), "Prefers short answers - always");
        assert!(memory_line("   ").is_err());
        assert!(memory_line(&"x".repeat(MEMORY_LINE_MAX + 1)).is_err());
    }
}
