use anyhow::{anyhow, bail, Result};
use loka_shared::quest_questions::{self as questions, Ask, Asked, Outcome, Settle, Who};
use serde_json::{json, Value};
use uuid::Uuid;

use super::Server;

pub(super) const QUEST_TOOLS: &[&str] = &["ask_agent"];
pub(super) const ROOM_TOOLS: &[&str] = &["answer_question", "pass_up", "room_members"];
pub(super) const NO_ANSWER_YET: &str = "No answer yet. Keep going with your stated assumption; the answer is steered into your work when it comes, so check it then.";

fn names(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect()
}

fn refs(args: &Value) -> Vec<String> {
    questions::clean_refs(&names(args, "refs"))
}

impl Server {
    pub(super) fn room_tools(&self, tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
        let s = |d: &str| json!({ "type": "string", "description": d });
        let mut tools = Vec::new();
        if self.read_only && self.quest.is_some() && self.session.is_some() && self.asking {
            tools.push(tool(
                "ask_agent",
                "Ask your agent when you are unsure about something it would know: which system to extend, where something lives, what an earlier quest decided, which of two reasonable approaches to take. It answers from its memory, usually within a minute, and only passes taste, credentials, money or irreversible steps to your person. Ask instead of guessing or rereading at length, at most 5 times per run. Always state what you will assume if nobody knows; if no answer comes in time, go on with that and the answer is steered in later. Pass agent to ask another agent of your person instead.",
                json!({
                    "question": s("One clear question with the context it needs"),
                    "about": { "type": "string", "enum": questions::ABOUTS, "description": "What it is about: fact, choice, taste, credentials, money or irreversible" },
                    "assumption": s("What you will do if nobody knows"),
                    "agent": s("Another agent of your person to ask, optional; defaults to the agent that owns this quest")
                }),
                &["question", "assumption"],
            ));
        }
        if !self.read_only && !self.aside && self.quick.is_none() {
            tools.push(tool(
                "answer_question",
                "Decide a question one of your quest helpers asked you (my_sessions lists open ones). The answer goes straight into its work and shows in the quest room as your decision. Cite what you relied on in refs. Your decision never counts as your person's approval.",
                json!({ "question": s("Question id or its first 8 characters"), "answer": s("Your decision, short and concrete"), "refs": { "type": "array", "items": { "type": "string" }, "description": "msg:, report:, quest: or todo: refs you relied on" } }),
                &["question", "answer"],
            ));
            tools.push(tool(
                "pass_up",
                "Send a helper's question to your person when it is truly theirs: taste their notes and rules do not settle, credentials, money or an irreversible step. They get a Needs you card with your suggestion; their answer goes into the helper's work.",
                json!({ "question": s("Question id or its first 8 characters"), "why": { "type": "string", "enum": questions::PERSON_KINDS, "description": "Why it is theirs" }, "suggestion": s("Your pick for them, optional") }),
                &["question", "why"],
            ));
            tools.push(tool(
                "room_members",
                "List, add or remove members of a quest room. The quest's owner, its agent and its sessions are always members. Add another agent of your person to ask it things in the room, or a person your person trusts to let them follow and post there.",
                json!({
                    "quest": s("Quest id or its first 8 characters"),
                    "add": { "type": "array", "items": { "type": "string" }, "description": "Agents by name, or people as person:<email or name> (only your person and people they trust)" },
                    "remove": { "type": "array", "items": { "type": "string" }, "description": "Members to remove, same form as add" }
                }),
                &["quest"],
            ));
        }
        tools
    }

    pub(super) async fn question_led(&self) -> Result<bool> {
        if !self.aside {
            return Ok(false);
        }
        let Some(id) = self.aside_session else { return Ok(false) };
        Ok(self.store.get_session(id).await?.is_some_and(|s| questions::question_led(s.asked_by.as_ref())))
    }

    pub(super) async fn ask_agent(&self, args: &Value) -> Result<String> {
        let (Some(task), Some(session_id)) = (self.quest, self.session) else {
            bail!("ask_agent only works inside a quest session");
        };
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let session = self.store.get_session(session_id).await?.ok_or_else(|| anyhow!("this session is gone"))?;
        questions::may_ask(&session.role, self.caller.owner_user_id.is_some(), self.on_shared_device).map_err(|e| anyhow!(e))?;
        let room = self.store.room_asking(task).await?.ok_or_else(|| anyhow!("this quest is gone"))?;
        let to = match arg("agent") {
            Some(name) => {
                let agent = self.agent(name).await?;
                if agent.owner_user_id != self.caller.owner_user_id {
                    bail!("{} works for someone else; you can only ask agents of your person", agent.name);
                }
                agent
            }
            None => match room.lead {
                Some(lead) if lead != self.caller.id => self.store.get_agent(lead).await?.ok_or_else(|| anyhow!("the quest's agent is gone"))?,
                _ => self.caller.clone(),
            },
        };
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM session_runs WHERE session_id = $1 AND status = 'running' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session.id)
        .fetch_optional(self.store.pool())
        .await?;
        let asker = format!("{} (quest session {})", self.caller.name, &session.id.to_string()[..8]);
        let asked = self
            .store
            .ask_question(Ask {
                room: &room,
                helper: Some(&session),
                person: None,
                asker: &asker,
                to: &to,
                question: arg("question").ok_or_else(|| anyhow!("question is required"))?,
                about: arg("about").unwrap_or("fact"),
                assumption: arg("assumption").unwrap_or(""),
                run_id,
                post_id: None,
            })
            .await?;
        Ok(match asked {
            Asked::Settled(settled) => settled.for_helper(),
            Asked::Waiting(id) => match self.store.await_answer(id, questions::WAIT).await? {
                Some(settled) => settled.for_helper(),
                None => NO_ANSWER_YET.to_string(),
            },
        })
    }

    pub(super) async fn answer_question(&self, key: &str, answer: &str, refs: Vec<String>) -> Result<String> {
        let id = self.store.find_question_for(&self.caller, key).await?;
        let settle = Settle { refs, ..Settle::new(Outcome::Answered, answer, Who::agent(&self.caller)) };
        match self.store.settle_question(id, settle).await? {
            Some(_) => Ok("Decided. It reaches the helper and shows in the quest room as your decision.".into()),
            None => bail!("that question is already settled; the quest room shows how"),
        }
    }

    pub(super) async fn room_call(&self, name: &str, args: &Value) -> Result<String> {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let need = |k: &str| arg(k).ok_or_else(|| anyhow!("{k} is required"));
        match name {
            "ask_agent" => self.ask_agent(args).await,
            "answer_question" => self.answer_question(need("question")?, need("answer")?, refs(args)).await,
            "pass_up" => {
                let id = self.store.find_question_for(&self.caller, need("question")?).await?;
                let why = arg("why").map(str::to_ascii_lowercase).filter(|w| questions::PERSON_KINDS.contains(&w.as_str()));
                let Some(why) = why else {
                    bail!("why must be taste, credentials, money or irreversible; decide everything else yourself with answer_question");
                };
                let settle = Settle { why: Some(why), ..Settle::new(Outcome::PassedUp, arg("suggestion").unwrap_or(""), Who::agent(&self.caller)) };
                match self.store.settle_question(id, settle).await? {
                    Some(_) => Ok("Passed to your person as a Needs you card. Their answer goes into the helper's work.".into()),
                    None => bail!("that question is already settled; the quest room shows how"),
                }
            }
            "room_members" => self.room_members(args).await,
            _ => bail!("unknown tool {name}"),
        }
    }

    async fn room_members(&self, args: &Value) -> Result<String> {
        let key = args.get("quest").and_then(Value::as_str).unwrap_or("");
        let task = self.reachable_quest(key, "change the room of").await?;
        let owner = self.owner()?;
        let mut notes = Vec::new();
        for entry in names(args, "add") {
            let (kind, id) = self.member_target(&entry, owner).await?;
            match self.store.add_room_member(task.id, kind, id, None, ("agent", Some(self.caller.id))).await {
                Ok(member) => notes.push(format!("added {}", member.name.unwrap_or(entry))),
                Err(error) => notes.push(format!("{entry}: {error}")),
            }
        }
        for entry in names(args, "remove") {
            let (kind, id) = self.member_target(&entry, owner).await?;
            match self.store.remove_room_member(task.id, kind, id).await {
                Ok(true) => notes.push(format!("removed {entry}")),
                Ok(false) => notes.push(format!("{entry} was not a member")),
                Err(error) => notes.push(format!("{entry}: {error}")),
            }
        }
        let members = self.store.room_members(task.id).await?;
        let list = members
            .iter()
            .map(|m| format!("- {} ({} {}{})", m.name.as_deref().unwrap_or("?"), m.member_kind, m.role, if m.default { "" } else { ", added" }))
            .collect::<Vec<_>>()
            .join("\n");
        let head = if notes.is_empty() { String::new() } else { format!("{}.\n", notes.join("; ")) };
        Ok(format!("{head}Room of \"{}\":\n{list}", task.title))
    }

    async fn member_target(&self, entry: &str, owner: Uuid) -> Result<(&'static str, Uuid)> {
        if let Some(person) = entry.strip_prefix("person:") {
            let id = sqlx::query_scalar::<_, Uuid>(
                "SELECT u.id FROM users u
                 WHERE (lower(u.email) = lower($2) OR lower(u.name) = lower($2))
                   AND (u.id = $1 OR EXISTS (SELECT 1 FROM world_trusted t WHERE t.owner_user_id = $1 AND t.user_id = u.id))
                 LIMIT 1",
            )
            .bind(owner)
            .bind(person.trim())
            .fetch_optional(self.store.pool())
            .await?
            .ok_or_else(|| anyhow!(loka_shared::rooms::NOT_TRUSTED))?;
            return Ok(("person", id));
        }
        Ok(("agent", self.agent(entry).await?.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loka_shared::db::Database;
    use loka_shared::world::{WorldAgent, WorldStore};

    async fn setup() -> Option<(sqlx::PgPool, WorldStore, WorldAgent, Uuid, Uuid)> {
        let url = std::env::var("QUESTIONS_TEST_DATABASE_URL").ok()?;
        let max = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(2);
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(max).connect(&url).await.expect("test database");
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('Ana', $1) RETURNING id")
            .bind(format!("{tag}@rooms.test"))
            .fetch_one(&pool)
            .await
            .unwrap();
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility, asking)
             VALUES ($1, 'Abhi', 'a', 'claude', 'opus', $2, 'private', 'ask_agent') RETURNING id",
        )
        .bind(format!("r-{}", &tag[..12]))
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, assigned_agent_id, status) VALUES ($1, 'Rooms', $2, 'in_progress') RETURNING id")
            .bind(owner)
            .bind(agent)
            .fetch_one(&pool)
            .await
            .unwrap();
        let session: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id) VALUES ($1, 'task', 'claude', 'opus', 'active', $2) RETURNING id")
            .bind(agent)
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap();
        let caller = store.get_agent(agent).await.unwrap().unwrap();
        Some((pool, store, caller, task, session))
    }

    #[tokio::test]
    async fn a_question_led_aside_cannot_change_setup_or_pass_as_the_owner() {
        let Some((pool, store, caller, _task, _session)) = setup().await else { return };
        let aside: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, status, asked_by) VALUES ($1, 'aside', 'claude', 'opus', 'active', $2) RETURNING id")
            .bind(caller.id)
            .bind(json!({ "kind": questions::KIND, "question_id": Uuid::new_v4() }))
            .fetch_one(&pool)
            .await
            .unwrap();
        let server = Server::new(store.clone(), Database::from_pool(pool.clone()), caller.clone()).in_aside(true, Some(aside));
        assert!(server.question_led().await.unwrap());
        let refused = server.call("update_agent", &json!({ "agent": caller.slug, "asking": "crew" })).await.unwrap_err().to_string();
        assert!(refused.contains("quest question"), "setup tools are refused in a question-led turn: {refused}");
        let report = server.call("write_report", &json!({ "title": "Private", "body": "text" })).await.unwrap_err().to_string();
        assert!(report.contains("quest question"), "every write tool is refused in a question-led turn: {report}");
        let owner_check = server.require_owner_asked().await.unwrap_err().to_string();
        assert!(owner_check.contains("quest question"), "a helper's question never counts as the owner asking");
        let owner_aside: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, status, asked_by) VALUES ($1, 'aside', 'claude', 'opus', 'active', $2) RETURNING id")
            .bind(caller.id)
            .bind(json!({ "kind": "user", "user_id": caller.owner_user_id }))
            .fetch_one(&pool)
            .await
            .unwrap();
        let plain = Server::new(store, Database::from_pool(pool), caller).in_aside(true, Some(owner_aside));
        assert!(!plain.question_led().await.unwrap(), "other asides keep their usual rules");
    }

    #[tokio::test]
    async fn guest_and_shared_device_sessions_cannot_ask() {
        let Some((pool, store, caller, task, session)) = setup().await else { return };
        let shared = Server::new(store.clone(), Database::from_pool(pool.clone()), caller.clone()).in_quest(Some(task), Some(session)).read_only().asking(true).on_shared_device();
        let refused = shared.call("ask_agent", &json!({ "question": "Which store?", "assumption": "the old one" })).await.unwrap_err().to_string();
        assert_eq!(refused, questions::GUEST_REFUSAL);
        let asked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE kind = 'question' AND asked_by_session = $1").bind(session).fetch_one(&pool).await.unwrap();
        assert_eq!(asked, 0, "nothing is asked from a shared device");
        let quiet = Server::new(store, Database::from_pool(pool), caller).in_quest(Some(task), Some(session)).read_only();
        assert!(!quiet.listed_tools().iter().any(|t| t["name"] == "ask_agent"), "with asking off the tool is not listed");
    }
}
