use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::world::{NewEvent, WorldStore};

pub const DEFAULT_LIST: &str = "To do";
pub const PRIORITIES: &[&str] = &["urgent", "high", "normal", "low"];

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Todo {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub text: String,
    pub priority: i16,
    pub position: Option<f64>,
    pub added_by_agent: Option<Uuid>,
    pub added_by_user: Option<Uuid>,
    pub source: Option<Value>,
    pub due_at: Option<DateTime<Utc>>,
    pub nudge_at: Option<DateTime<Utc>>,
    pub nudge_count: i32,
    pub repeat_nudge: bool,
    pub last_nudged_at: Option<DateTime<Utc>>,
    pub status: String,
    pub done_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    pub proposals: Value,
    #[sqlx(default)]
    pub decision: Option<Value>,
}

pub const MAX_PROPOSALS: usize = 4;
pub const PROPOSAL_KINDS: &[&str] = &["reply", "message", "task", "other"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default)]
    pub body: String,
}

fn field(v: Option<&str>, max: usize) -> Option<String> {
    v.map(str::trim).filter(|t| !t.is_empty()).map(|t| t.chars().take(max).collect())
}

impl Proposal {
    pub fn cleaned(&self) -> Option<Proposal> {
        let kind = self.kind.trim().to_ascii_lowercase();
        let kind = if PROPOSAL_KINDS.contains(&kind.as_str()) { kind } else { "other".to_string() };
        let body: String = self.body.trim().chars().take(4000).collect();
        let title = field(self.title.as_deref(), 200);
        if body.is_empty() && title.is_none() {
            return None;
        }
        let label = field(Some(&self.label), 60).unwrap_or_else(|| match kind.as_str() {
            "reply" => "Send a reply".into(),
            "message" => "Send a message".into(),
            "task" => "Start a quest".into(),
            _ => "Do this".into(),
        });
        Some(Proposal {
            kind,
            label,
            to: field(self.to.as_deref(), 120),
            via: field(self.via.as_deref(), 40),
            title,
            agent: field(self.agent.as_deref(), 80),
            repo: field(self.repo.as_deref(), 120),
            body,
        })
    }

    pub fn instruction(&self, original: &Proposal) -> String {
        let edited = self != original;
        let target = self.to.as_deref().map(|t| format!(" to {t}")).unwrap_or_default();
        let via = self.via.as_deref().map(|v| format!(" on {v}")).unwrap_or_default();
        let exact = if edited { "with my edits, exactly this" } else { "exactly as you wrote it" };
        match self.kind.as_str() {
            "reply" | "message" => format!("Send it{target}{via}, {exact}:\n\n{}", self.body),
            "task" => format!(
                "Start the quest{} \"{}\"{}{}:\n\n{}",
                if edited { " with my edits" } else { "" },
                self.title.as_deref().unwrap_or("(untitled)"),
                self.agent.as_deref().map(|a| format!(" for {a}")).unwrap_or_default(),
                self.repo.as_deref().map(|r| format!(" on {r}")).unwrap_or_default(),
                self.body
            ),
            _ => format!("Do this, {exact}:\n\n{}", self.body),
        }
    }
}

pub fn clean_proposals(raw: &Value) -> Vec<Proposal> {
    serde_json::from_value::<Vec<Proposal>>(raw.clone())
        .unwrap_or_default()
        .iter()
        .filter_map(Proposal::cleaned)
        .take(MAX_PROPOSALS)
        .collect()
}

impl Todo {
    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }

    pub fn age(&self, now: DateTime<Utc>) -> String {
        age_label(now - self.added_at)
    }
}

pub fn age_label(d: Duration) -> String {
    if d.num_days() >= 1 {
        format!("{}d", d.num_days())
    } else if d.num_hours() >= 1 {
        format!("{}h", d.num_hours())
    } else {
        format!("{}m", d.num_minutes().max(0))
    }
}

pub fn priority_from(v: Option<&Value>) -> Option<i16> {
    match v? {
        Value::Number(n) => n.as_i64().map(|n| n.clamp(0, 3) as i16),
        Value::String(s) => PRIORITIES.iter().position(|p| p.eq_ignore_ascii_case(s.trim())).map(|i| i as i16),
        _ => None,
    }
}

pub fn backoff(count: i32) -> Duration {
    match count {
        ..=1 => Duration::hours(1),
        2 => Duration::hours(4),
        3 => Duration::days(1),
        4 => Duration::days(2),
        5 => Duration::days(4),
        _ => Duration::days(7),
    }
}

pub fn next_nudge(now: DateTime<Utc>, count: i32, due_at: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let next = now + backoff(count);
    match due_at {
        Some(due) if due > now && due < next => due,
        _ => next,
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewTodo {
    pub owner_user_id: Uuid,
    pub text: String,
    pub priority: Option<i16>,
    pub added_by_agent: Option<Uuid>,
    pub added_by_user: Option<Uuid>,
    pub source: Option<Value>,
    pub due_at: Option<DateTime<Utc>>,
    pub nudge_at: Option<DateTime<Utc>>,
    pub repeat_nudge: Option<bool>,
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TodoActor {
    pub agent: Option<Uuid>,
    pub user: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TodoPatch {
    pub text: Option<String>,
    pub priority: Option<i16>,
    #[serde(default, with = "double_option")]
    pub due_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, with = "double_option")]
    pub nudge_at: Option<Option<DateTime<Utc>>>,
    pub repeat_nudge: Option<bool>,
    pub started: Option<bool>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

const ORDER: &str = "ORDER BY (status = 'open') DESC, CASE WHEN status = 'open' THEN priority END, CASE WHEN status = 'open' THEN position END NULLS LAST, CASE WHEN status = 'open' THEN added_at END, done_at DESC NULLS LAST";

impl WorldStore {
    pub async fn add_todo(&self, new: NewTodo) -> Result<Todo> {
        let actor = TodoActor { agent: new.added_by_agent, user: new.added_by_user };
        let mut tx = self.pool().begin().await?;
        let todo = Self::add_todo_in(&mut tx, &new).await?;
        Self::todo_event_in(&mut tx, "todo.added", &todo, actor, None).await?;
        tx.commit().await?;
        Ok(todo)
    }

    pub(crate) async fn add_todo_in(tx: &mut Transaction<'_, Postgres>, new: &NewTodo) -> Result<Todo> {
        let text = new.text.trim();
        if text.is_empty() {
            bail!("a to-do needs text");
        }
        if text.chars().count() > 500 {
            bail!("keep a to-do under 500 characters");
        }
        let nudge_at = match (new.nudge_at, new.due_at) {
            (Some(n), _) => Some(n),
            (None, due) => due,
        };
        let todo = sqlx::query_as::<_, Todo>(
            "INSERT INTO world_todos (owner_user_id, text, priority, added_by_agent, added_by_user, source, due_at, nudge_at, repeat_nudge, proposals)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
        )
        .bind(new.owner_user_id)
        .bind(text)
        .bind(new.priority.unwrap_or(2).clamp(0, 3))
        .bind(new.added_by_agent)
        .bind(new.added_by_user)
        .bind(&new.source)
        .bind(new.due_at)
        .bind(nudge_at)
        .bind(new.repeat_nudge.unwrap_or(true))
        .bind(json!(&new.proposals))
        .fetch_one(&mut **tx)
        .await?;
        Ok(todo)
    }

    pub async fn set_todo_proposals(&self, id: Uuid, proposals: &[Proposal], by: TodoActor) -> Result<Option<Todo>> {
        let todo = sqlx::query_as::<_, Todo>(
            "UPDATE world_todos SET proposals = $2, decision = NULL, updated_at = NOW() WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(json!(proposals))
        .fetch_optional(self.pool())
        .await?;
        if let Some(t) = &todo {
            self.todo_event("todo.updated", t, by, None).await?;
        }
        Ok(todo)
    }

    pub async fn decide_todo(&self, id: Uuid, owner: Uuid, decision: Value) -> Result<Option<Todo>> {
        let todo = sqlx::query_as::<_, Todo>(
            "UPDATE world_todos SET decision = $3, started_at = COALESCE(started_at, NOW()), updated_at = NOW()
             WHERE id = $1 AND owner_user_id = $2 AND decision IS NULL AND status = 'open' RETURNING *",
        )
        .bind(id)
        .bind(owner)
        .bind(decision)
        .fetch_optional(self.pool())
        .await?;
        if let Some(t) = &todo {
            self.todo_event("todo.updated", t, TodoActor { agent: None, user: Some(owner) }, None).await?;
        }
        Ok(todo)
    }

    pub async fn clear_todo_decision(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE world_todos SET decision = NULL WHERE id = $1").bind(id).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn get_todo(&self, id: Uuid) -> Result<Option<Todo>> {
        Ok(sqlx::query_as::<_, Todo>("SELECT * FROM world_todos WHERE id = $1").bind(id).fetch_optional(self.pool()).await?)
    }

    pub async fn todos_for(&self, owner: Uuid, done_days: i64) -> Result<Vec<Todo>> {
        Ok(sqlx::query_as::<_, Todo>(&format!(
            "SELECT * FROM world_todos
             WHERE owner_user_id = $1
               AND (status = 'open' OR (status IN ('done', 'dismissed') AND done_at > NOW() - make_interval(days => $2::int)))
             {ORDER} LIMIT 300"
        ))
        .bind(owner)
        .bind(done_days as i32)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn find_todo(&self, owner: Uuid, key: &str) -> Result<Option<Todo>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        let open = self.todos_for(owner, 0).await?;
        if let Ok(n) = key.parse::<usize>() {
            if n >= 1 && n <= open.len() && key.len() < 4 {
                return Ok(open.into_iter().nth(n - 1));
            }
        }
        let k = key.to_lowercase();
        let by_id = sqlx::query_as::<_, Todo>(
            "SELECT * FROM world_todos WHERE owner_user_id = $1 AND id::text LIKE $2 || '%' ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(owner)
        .bind(&k)
        .fetch_optional(self.pool())
        .await?;
        if by_id.is_some() {
            return Ok(by_id);
        }
        Ok(open
            .iter()
            .find(|t| t.text.to_lowercase() == k)
            .or_else(|| open.iter().find(|t| t.text.to_lowercase().contains(&k)))
            .cloned())
    }

    pub async fn set_todo_status(&self, id: Uuid, status: &str, by: TodoActor) -> Result<Option<Todo>> {
        if !matches!(status, "open" | "done" | "dismissed") {
            bail!("unknown to-do status {status}");
        }
        let todo = sqlx::query_as::<_, Todo>(
            "UPDATE world_todos SET status = $2,
               done_at = CASE WHEN $2 = 'open' THEN NULL ELSE NOW() END,
               nudge_at = CASE WHEN $2 = 'open' THEN nudge_at ELSE NULL END,
               updated_at = NOW()
             WHERE id = $1 AND status <> $2 RETURNING *",
        )
        .bind(id)
        .bind(status)
        .fetch_optional(self.pool())
        .await?;
        let Some(t) = todo else {
            return self.get_todo(id).await;
        };
        let kind = match status {
            "done" => "todo.done",
            "dismissed" => "todo.dismissed",
            _ => "todo.reopened",
        };
        let suggestion_dropped = t.decision.is_none() && t.source.as_ref().and_then(|s| s.get("suggested")).and_then(Value::as_bool) == Some(true);
        let wake = match (status, t.added_by_agent) {
            ("done", Some(a)) if by.agent != Some(a) => Some(a),
            ("dismissed", Some(a)) if by.agent != Some(a) && suggestion_dropped => Some(a),
            _ => None,
        };
        self.todo_event(kind, &t, by, wake).await?;
        Ok(Some(t))
    }

    pub async fn update_todo(&self, id: Uuid, patch: &TodoPatch, by: TodoActor) -> Result<Option<Todo>> {
        if patch.text.as_deref().is_some_and(|t| t.trim().is_empty()) {
            bail!("a to-do needs text");
        }
        let todo = sqlx::query_as::<_, Todo>(
            "UPDATE world_todos SET
               text = COALESCE($2, text),
               priority = COALESCE($3, priority),
               due_at = CASE WHEN $4 THEN $5 ELSE due_at END,
               nudge_at = CASE WHEN $6 THEN $7 ELSE nudge_at END,
               repeat_nudge = COALESCE($8, repeat_nudge),
               started_at = CASE WHEN $9 IS NULL THEN started_at WHEN $9 THEN COALESCE(started_at, NOW()) ELSE NULL END,
               updated_at = NOW()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(patch.text.as_deref().map(str::trim))
        .bind(patch.priority.map(|p| p.clamp(0, 3)))
        .bind(patch.due_at.is_some())
        .bind(patch.due_at.flatten())
        .bind(patch.nudge_at.is_some())
        .bind(patch.nudge_at.flatten())
        .bind(patch.repeat_nudge)
        .bind(patch.started)
        .fetch_optional(self.pool())
        .await?;
        if let Some(t) = &todo {
            self.todo_event("todo.updated", t, by, None).await?;
        }
        Ok(todo)
    }

    pub async fn snooze_todo(&self, id: Uuid, until: DateTime<Utc>, by: TodoActor) -> Result<Option<Todo>> {
        self.update_todo(id, &TodoPatch { nudge_at: Some(Some(until)), ..Default::default() }, by).await
    }

    pub async fn reorder_todos(&self, owner: Uuid, ids: &[Uuid]) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE world_todos t SET position = o.ord, updated_at = NOW()
             FROM unnest($2::uuid[]) WITH ORDINALITY AS o(id, ord)
             WHERE t.id = o.id AND t.owner_user_id = $1",
        )
        .bind(owner)
        .bind(ids)
        .execute(self.pool())
        .await?
        .rows_affected();
        self.emit(NewEvent::new("todo.reordered").actor(Some(owner)).payload(json!({ "owner_user_id": owner })), None).await?;
        Ok(done)
    }

    pub async fn fire_due_nudges(&self) -> Result<Vec<Todo>> {
        let mut tx = self.pool().begin().await?;
        let due = sqlx::query_as::<_, Todo>(
            "SELECT * FROM world_todos WHERE status = 'open' AND nudge_at <= NOW()
             ORDER BY nudge_at LIMIT 100 FOR UPDATE SKIP LOCKED",
        )
        .fetch_all(&mut *tx)
        .await?;
        let now = Utc::now();
        let mut fired = Vec::with_capacity(due.len());
        for t in due {
            let count = t.nudge_count + 1;
            let next = t.repeat_nudge.then(|| next_nudge(now, count, t.due_at));
            let updated = sqlx::query_as::<_, Todo>(
                "UPDATE world_todos SET nudge_count = $2, nudge_at = $3, last_nudged_at = NOW(), updated_at = NOW()
                 WHERE id = $1 RETURNING *",
            )
            .bind(t.id)
            .bind(count)
            .bind(next)
            .fetch_one(&mut *tx)
            .await?;
            let added_by = match updated.added_by_agent {
                Some(a) => sqlx::query_scalar::<_, String>("SELECT name FROM agent_profiles WHERE id = $1")
                    .bind(a)
                    .fetch_optional(&mut *tx)
                    .await?,
                None => None,
            };
            let overdue = updated.due_at.is_some_and(|d| d <= now);
            let event = NewEvent::new("todo.nudge").payload(json!({
                "todo_id": updated.id,
                "text": updated.text,
                "owner_user_id": updated.owner_user_id,
                "added_by_agent": updated.added_by_agent,
                "added_by": added_by,
                "due_at": updated.due_at,
                "overdue": overdue,
                "nudge_count": count,
                "timer": !updated.repeat_nudge && count == 1,
                "age": updated.age(now),
                "source": updated.source,
            }));
            crate::world::emit_in(&mut tx, &event, None).await?;
            fired.push(updated);
        }
        tx.commit().await?;
        Ok(fired)
    }

    async fn todo_event(&self, kind: &str, todo: &Todo, by: TodoActor, wake: Option<Uuid>) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        Self::todo_event_in(&mut tx, kind, todo, by, wake).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn todo_event_in(
        tx: &mut Transaction<'_, Postgres>,
        kind: &str,
        todo: &Todo,
        by: TodoActor,
        wake: Option<Uuid>,
    ) -> Result<()> {
        let by_name = match by.user {
            Some(u) => sqlx::query_scalar::<_, Option<String>>("SELECT name FROM users WHERE id = $1")
                .bind(u)
                .fetch_optional(&mut **tx)
                .await?
                .flatten(),
            None => None,
        };
        let mut event = NewEvent::new(kind).actor(by.user).payload(json!({
            "todo_id": todo.id,
            "text": todo.text,
            "owner_user_id": todo.owner_user_id,
            "added_by_agent": todo.added_by_agent,
            "by_agent": by.agent,
            "by": by_name,
            "status": todo.status,
            "source": todo.source,
            "age": todo.age(Utc::now()),
        }));
        if let Some(a) = todo.added_by_agent {
            event = event.agent(a);
        }
        crate::world::emit_in(tx, &event, wake.map(|a| (a, kind))).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn proposals_clean_and_explain() {
        let raw = serde_json::json!([
            { "kind": "Reply", "to": "Maya", "via": "slack", "body": "  Flag is checked, rolling at 5.  " },
            { "kind": "task", "title": "Fix the flaky check", "agent": "Rocco", "body": "Make CI green" },
            { "kind": "nonsense", "body": "" },
            { "kind": "weird", "body": "Ping Sam" }
        ]);
        let p = clean_proposals(&raw);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].kind, "reply");
        assert_eq!(p[0].label, "Send a reply");
        assert_eq!(p[2].kind, "other");
        assert_eq!(p[0].instruction(&p[0]), "Send it to Maya on slack, exactly as you wrote it:\n\nFlag is checked, rolling at 5.");
        let edited = Proposal { body: "Rolling at 6 instead.".into(), ..p[0].clone() };
        assert!(edited.instruction(&p[0]).starts_with("Send it to Maya on slack, with my edits, exactly this:"));
        assert_eq!(p[1].instruction(&p[1]), "Start the quest \"Fix the flaky check\" for Rocco:\n\nMake CI green");
    }
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn backoff_grows_and_respects_due() {
        let now = Utc::now();
        assert_eq!(backoff(1), Duration::hours(1));
        assert!(backoff(3) > backoff(2));
        assert_eq!(backoff(40), Duration::days(7));
        let due = now + Duration::minutes(20);
        assert_eq!(next_nudge(now, 1, Some(due)), due);
        assert_eq!(next_nudge(now, 1, Some(now - Duration::hours(1))), now + Duration::hours(1));
        assert_eq!(priority_from(Some(&json!("High"))), Some(1));
        assert_eq!(priority_from(Some(&json!(9))), Some(3));
        assert_eq!(age_label(Duration::hours(80)), "3d");
    }

    #[tokio::test]
    async fn todo_nudges_fire_and_notify_adder() -> Result<()> {
        let Ok(url) = std::env::var("TODOS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("nudge-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, org_id) VALUES ($1, 'nudge', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("nudge-{tag}"))
            .bind(crate::orgs::DEFAULT_ORG)
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, user, None).await?;
        let todo = store
            .add_todo(NewTodo {
                owner_user_id: user,
                text: "nudge test item".into(),
                added_by_agent: Some(agent),
                nudge_at: Some(Utc::now() - Duration::seconds(1)),
                due_at: Some(Utc::now() + Duration::minutes(10)),
                ..Default::default()
            })
            .await?;
        store.fire_due_nudges().await?;
        let mine = store.get_todo(todo.id).await?.expect("todo");
        assert_eq!(mine.nudge_count, 1);
        assert_eq!(mine.nudge_at, todo.due_at);
        assert!(store.fire_due_nudges().await?.iter().all(|t| t.id != todo.id));
        let seen: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_notifications WHERE kind = 'reminder' AND ref = $1 AND user_id = $2",
        )
        .bind(todo.id.to_string())
        .bind(user)
        .fetch_one(&pool)
        .await?;
        assert_eq!(seen, 1);
        let done = store.set_todo_status(todo.id, "done", TodoActor { agent: None, user: Some(user) }).await?.expect("done");
        assert!(done.done_at.is_some() && done.nudge_at.is_none());
        let woken: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'todo.done' AND payload->>'todo_id' = $2",
        )
        .bind(agent)
        .bind(todo.id.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(woken, 1);
        sqlx::query("DELETE FROM agent_inbox WHERE kind = 'todo.done' AND payload->>'todo_id' = $1").bind(todo.id.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM world_todos WHERE id = $1").bind(todo.id).execute(&pool).await?;
        Ok(())
    }
}
