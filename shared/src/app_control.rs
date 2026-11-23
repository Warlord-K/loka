use anyhow::{bail, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{NewEvent, NewMessage, WorldAgent, WorldStore};

pub const ALLOW: &str = "Allow for this quest";
pub const DENY: &str = "Not now";
pub const HELP_DONE: &str = "Done";
pub const HELP_CANT: &str = "I can't right now";
pub const HELP_ASKED: &str = "Asked your owner. Leave the page open and keep working on anything else; their answer reaches you as a message in this session.";
pub const HELP_PENDING: &str = "You already asked and your owner has not answered yet. Leave the page open; their answer reaches you as a message.";
pub const HELP_URL_NEEDED: &str = "url must be the public http or https page that needs your owner: run get url in your browser first. Loka opens it again for them if your browser closed after this run";
const APPS_KEY: &str = "apps";
const HELP_KEY: &str = "browser_help";

pub fn is_allow(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.starts_with("allow") || t == "yes" || t == "ok" || t == "okay"
}

pub fn is_help_done(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.starts_with("done") || t.starts_with("signed in") || t == "yes" || t == "ok" || t == "okay"
}

fn help_url_ok(url: &str) -> bool {
    url.len() <= 2048 && url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

fn one_line(text: &str, max: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

pub struct BrowserHelp<'a> {
    pub site: &'a str,
    pub reason: &'a str,
    pub url: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestCard {
    Apps,
    BrowserHelp,
}

pub enum CardAnswer {
    Stale,
    Answered { yes: bool },
}

impl WorldStore {
    pub async fn apps_allowed_for_run(&self, run_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM session_runs r JOIN agent_tasks t ON t.id = r.task_id
                             WHERE r.id = $1 AND t.apps_allowed_at IS NOT NULL)",
        )
        .bind(run_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn request_app_control(&self, agent: &WorldAgent, task_id: Uuid, session_id: Uuid, reason: &str) -> Result<String> {
        let (owner, title, allowed) = self.quest_for_ask(agent, task_id, "apps_allowed_at IS NOT NULL").await?;
        if allowed {
            return Ok("Your owner already allowed app control for this quest. Go ahead.".into());
        }
        if self.quest_ask_pending(agent.id, owner, task_id, APPS_KEY).await? {
            return Ok("You already asked and your owner has not answered yet. Keep working on anything that does not need apps; their answer reaches you as a message.".into());
        }
        let reason = one_line(reason, 400);
        let text = if reason.is_empty() {
            format!("I want to use apps on your Mac for \"{title}\". Allow it for this quest?")
        } else {
            format!("I want to use apps on your Mac for \"{title}\": {reason}\n\nAllow it for this quest?")
        };
        let card = json!({ "task_id": task_id, "session_id": session_id });
        self.post_quest_ask(agent, task_id, session_id, &text, &[ALLOW, DENY], APPS_KEY, card).await?;
        Ok("Asked your owner. Keep working on anything that does not need apps; their answer reaches you as a message in this session.".into())
    }

    pub async fn request_browser_help(&self, agent: &WorldAgent, task_id: Uuid, session_id: Uuid, help: BrowserHelp<'_>) -> Result<String> {
        let site = one_line(help.site, 80);
        if site.is_empty() {
            bail!("say which site needs your owner, for example GitHub or the Stripe dashboard");
        }
        let url = help.url.trim();
        if !help_url_ok(url) {
            bail!("{HELP_URL_NEEDED}");
        }
        let (owner, title, _) = self.quest_for_ask(agent, task_id, "FALSE").await?;
        if self.quest_ask_pending(agent.id, owner, task_id, HELP_KEY).await? {
            return Ok(HELP_PENDING.into());
        }
        let reason = one_line(help.reason, 400);
        let text = if reason.is_empty() {
            format!("I need you in my browser on {site} for \"{title}\". Open it here, do that part, then press Done.")
        } else {
            format!("I need you in my browser on {site} for \"{title}\": {reason}\n\nOpen it here, do that part, then press Done.")
        };
        let card = json!({ "task_id": task_id, "session_id": session_id, "site": site, "url": url, "title": title });
        self.post_quest_ask(agent, task_id, session_id, &text, &[HELP_DONE, HELP_CANT], HELP_KEY, card).await?;
        Ok(HELP_ASKED.into())
    }

    async fn quest_for_ask(&self, agent: &WorldAgent, task_id: Uuid, flag: &str) -> Result<(Uuid, String, bool)> {
        let Some(owner) = agent.owner_user_id else {
            bail!("only agents that work for someone can ask their owner");
        };
        let row = sqlx::query_as::<_, (String, bool)>(&format!("SELECT title, {flag} FROM agent_tasks WHERE id = $1 AND assigned_agent_id = $2"))
            .bind(task_id)
            .bind(agent.id)
            .fetch_optional(self.pool())
            .await?;
        let Some((title, flag)) = row else {
            bail!("this quest is not yours");
        };
        Ok((owner, title, flag))
    }

    async fn quest_ask_pending(&self, agent_id: Uuid, owner: Uuid, task_id: Uuid, key: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_messages m
               WHERE m.agent_id = $1 AND m.meta->$4->>'task_id' = $2
                 AND NOT EXISTS (SELECT 1 FROM agent_messages a
                                  WHERE a.agent_id = m.agent_id AND a.author_user_id = $3 AND a.meta->>'answers' = m.id::text))",
        )
        .bind(agent_id)
        .bind(task_id.to_string())
        .bind(owner)
        .bind(key)
        .fetch_one(self.pool())
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_quest_ask(&self, agent: &WorldAgent, task_id: Uuid, session_id: Uuid, text: &str, choices: &[&str], key: &str, card: Value) -> Result<()> {
        let main = self.main_session(agent.id).await?;
        let mut meta = json!({ "kind": "ask", "choices": choices, "task_id": task_id });
        meta[key] = card;
        let message = self
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: text,
                session_id: main.as_ref().map(|m| m.id),
                run_id: None,
                meta,
            })
            .await?;
        self.emit(
            NewEvent::new("message.created")
                .agent(agent.id)
                .task(Some(task_id))
                .payload(json!({ "message_id": message.id, "author": agent.name, "author_kind": "agent", "body": text, "ask": true })),
            None,
        )
        .await?;
        self.emit(
            NewEvent::new("attention.requested")
                .agent(agent.id)
                .session(session_id)
                .task(Some(task_id))
                .payload(json!({ "text": text, "agent": agent.name, "message_id": message.id, "choices": choices })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn quest_card(&self, agent_id: Uuid, card: Uuid) -> Result<Option<QuestCard>> {
        let keys = sqlx::query_as::<_, (bool, bool)>("SELECT meta ? 'apps', meta ? 'browser_help' FROM agent_messages WHERE id = $1 AND agent_id = $2")
            .bind(card)
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(match keys {
            Some((true, _)) => Some(QuestCard::Apps),
            Some((_, true)) => Some(QuestCard::BrowserHelp),
            _ => None,
        })
    }

    async fn quest_card_meta(&self, agent_id: Uuid, card: Uuid, key: &str) -> Result<Option<(Value, Uuid, Uuid)>> {
        let meta = sqlx::query_scalar::<_, Option<Value>>("SELECT meta->$3 FROM agent_messages WHERE id = $1 AND agent_id = $2")
            .bind(card)
            .bind(agent_id)
            .bind(key)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        let Some(meta) = meta else {
            return Ok(None);
        };
        let id = |k: &str| meta.get(k).and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok());
        Ok(match (id("task_id"), id("session_id")) {
            (Some(task_id), Some(session_id)) => Some((meta, task_id, session_id)),
            _ => None,
        })
    }

    async fn steer_owner_answer(&self, agent_id: Uuid, session_id: Uuid, note: &str, by: &str) -> Result<bool> {
        let Some(session) = self.get_session(session_id).await?.filter(|s| s.agent_id == agent_id && s.status != "archived") else {
            return Ok(false);
        };
        Ok(self.start_followup(&session, note, by, false).await.is_ok())
    }

    pub async fn answer_quest_card(&self, agent_id: Uuid, card: Uuid, kind: QuestCard, text: &str, by: &str) -> Result<CardAnswer> {
        let answers = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND author_kind = 'user' AND meta->>'answers' = $2::text",
        )
        .bind(agent_id)
        .bind(card)
        .fetch_one(self.pool())
        .await?;
        if answers > 1 {
            return Ok(CardAnswer::Stale);
        }
        match kind {
            QuestCard::Apps => self.answer_app_control(agent_id, card, text, by).await,
            QuestCard::BrowserHelp => self.answer_browser_help(agent_id, card, text, by).await,
        }
    }

    pub async fn answer_app_control(&self, agent_id: Uuid, card: Uuid, text: &str, by: &str) -> Result<CardAnswer> {
        let Some((_, task_id, session_id)) = self.quest_card_meta(agent_id, card, APPS_KEY).await? else {
            return Ok(CardAnswer::Stale);
        };
        let allowed = is_allow(text);
        if allowed {
            sqlx::query("UPDATE agent_tasks SET apps_allowed_at = COALESCE(apps_allowed_at, NOW()) WHERE id = $1 AND assigned_agent_id = $2")
                .bind(task_id)
                .bind(agent_id)
                .execute(self.pool())
                .await?;
        }
        let note = if allowed {
            format!("[{by}, your owner, verified] allowed app control for this quest. Go ahead with the apps launcher.")
        } else {
            format!("[{by}, your owner, verified] said not now to app control. Do not use apps in this quest: finish another way, or say in your report what you would need.")
        };
        Ok(match self.steer_owner_answer(agent_id, session_id, &note, by).await? {
            true => CardAnswer::Answered { yes: allowed },
            false => CardAnswer::Stale,
        })
    }

    pub async fn answer_browser_help(&self, agent_id: Uuid, card: Uuid, text: &str, by: &str) -> Result<CardAnswer> {
        let Some((meta, _, session_id)) = self.quest_card_meta(agent_id, card, HELP_KEY).await? else {
            return Ok(CardAnswer::Stale);
        };
        let site = meta.get("site").and_then(Value::as_str).unwrap_or("the site");
        let done = is_help_done(text);
        let note = help_note(by, site, done, text);
        Ok(match self.steer_owner_answer(agent_id, session_id, &note, by).await? {
            true => CardAnswer::Answered { yes: done },
            false => CardAnswer::Stale,
        })
    }
}

fn help_note(by: &str, site: &str, done: bool, text: &str) -> String {
    let said = one_line(text, 300);
    let extra = if said.is_empty() || said == HELP_DONE || said == HELP_CANT { String::new() } else { format!(" They said: \"{said}\"") };
    if done {
        format!("[{by}, your owner, verified] finished in your browser on {site}.{extra} Check the page with a snapshot and carry on.")
    } else {
        format!("[{by}, your owner, verified] could not help in your browser on {site} right now.{extra} Finish another way, or say in your report what you would need.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_answers() {
        assert!(is_allow(ALLOW));
        assert!(is_allow("yes"));
        assert!(!is_allow(DENY));
        assert!(!is_allow("no thanks"));
    }

    #[tokio::test]
    async fn a_browser_help_card_asks_once_and_steers_one_answer_into_the_quest() -> Result<()> {
        let Ok(url) = std::env::var("BROWSER_HELP_TEST_DATABASE_URL") else { return Ok(()) };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
        let ann: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Ann') RETURNING id").bind(format!("ann-{tag}@help.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, ann, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let agent_id: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, $1, 'claude', 'sonnet', $2, $3) RETURNING id")
            .bind(format!("helper-{tag}"))
            .bind(ann)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (title, description, status, assigned_agent_id, user_id) VALUES ('Check billing', '', 'in_progress', $1, $2) RETURNING id")
            .bind(agent_id)
            .bind(ann)
            .fetch_one(&pool)
            .await?;
        let session: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id, cli_session_id) VALUES ($1, 'task', 'claude', 'sonnet', 'active', $2, 'cli-1') RETURNING id")
            .bind(agent_id)
            .bind(task)
            .fetch_one(&pool)
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let help = || BrowserHelp { site: " Stripe\n dashboard ", reason: "sign in with the team account", url: "https://dashboard.stripe.com/login" };

        assert!(store.request_browser_help(&agent, task, session, BrowserHelp { url: "file:///etc/passwd", ..help() }).await.is_err());
        assert!(store.request_browser_help(&agent, task, session, BrowserHelp { url: " ", ..help() }).await.is_err(), "the page is required so the owner can reach it after the run closed its tabs");
        assert_eq!(store.request_browser_help(&agent, task, session, help()).await?, HELP_ASKED);
        assert_eq!(store.request_browser_help(&agent, task, session, help()).await?, HELP_PENDING, "one open ask per quest");
        let (card, meta): (Uuid, Value) = sqlx::query_as("SELECT id, meta FROM agent_messages WHERE agent_id = $1 AND meta ? 'browser_help'").bind(agent_id).fetch_one(&pool).await?;
        assert_eq!(meta["browser_help"]["site"], "Stripe dashboard");
        assert_eq!(meta["browser_help"]["url"], "https://dashboard.stripe.com/login");
        assert_eq!(meta["browser_help"]["title"], "Check billing", "the watch view names the quest it opens");
        assert_eq!(meta["choices"], json!([HELP_DONE, HELP_CANT]));
        assert_eq!(store.quest_card(agent_id, card).await?, Some(QuestCard::BrowserHelp));

        let answer = |text: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta) VALUES ($1, 'user', $2, $3, jsonb_build_object('answers', $4::text))")
                    .bind(agent_id)
                    .bind(ann)
                    .bind(text)
                    .bind(card)
                    .execute(&pool)
                    .await
            }
        };
        answer(HELP_DONE).await?;
        assert!(matches!(store.answer_quest_card(agent_id, card, QuestCard::BrowserHelp, HELP_DONE, "Ann").await?, CardAnswer::Answered { yes: true }));
        let prompts: Vec<String> = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE session_id = $1 AND kind = 'followup'").bind(session).fetch_all(&pool).await?;
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("finished in your browser on Stripe dashboard"));
        let main_wakes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'message'").bind(agent_id).fetch_one(&pool).await?;
        assert_eq!(main_wakes, 0, "the answer goes to the quest, never the main inbox");

        answer(HELP_DONE).await?;
        assert!(matches!(store.answer_quest_card(agent_id, card, QuestCard::BrowserHelp, HELP_DONE, "Ann").await?, CardAnswer::Stale), "closing the view after Done does not steer twice");
        let followups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE session_id = $1 AND kind = 'followup'").bind(session).fetch_one(&pool).await?;
        assert_eq!(followups, 1);
        assert_eq!(store.request_browser_help(&agent, task, session, help()).await?, HELP_ASKED, "an answered ask lets the quest ask again");
        Ok(())
    }

    #[test]
    fn browser_help_answers_and_notes() {
        assert!(is_help_done(HELP_DONE) && is_help_done(" done, signed in") && is_help_done("Signed in"));
        assert!(!is_help_done(HELP_CANT) && !is_help_done("not done yet") && !is_help_done("no"));
        let done = help_note("Ana", "GitHub", true, HELP_DONE);
        assert!(done.starts_with("[Ana, your owner, verified] finished in your browser on GitHub.") && !done.contains("They said"));
        let cant = help_note("Ana", "GitHub", false, "my phone   is\nflat");
        assert!(cant.contains("could not help") && cant.contains("They said: \"my phone is flat\""));
        assert!(help_url_ok("https://github.com/login") && help_url_ok("http://example.com"));
        assert!(!help_url_ok("javascript:alert(1)") && !help_url_ok("file:///etc/passwd") && !help_url_ok("github.com") && !help_url_ok(&format!("https://a.com/{}", "x".repeat(2100))));
        assert_eq!(one_line("  Stripe\n dashboard ", 80), "Stripe dashboard");
    }
}
