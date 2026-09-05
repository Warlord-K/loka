use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{emit_in, NewEvent, WorldEvent, WorldStore};

pub const SLACK_EVENT: &str = "slack.notification";
pub const WATCHED_EVENT: &str = "slack.watched";
pub const WATCH_PREFIX: &str = "slack.";
pub const WATCHED_KIND: &str = "watched";
pub const POST_WATCH_DAYS: i64 = 3;
const KINDS: &[&str] = &["dm", "mention", "highlight", "thread", WATCHED_KIND];
const WATCH_TARGETS_MAX: i64 = 200;
const MAX_TEXT: usize = 2000;

#[derive(Debug, Clone, Deserialize)]
pub struct SlackNotification {
    pub kind: String,
    pub team_id: String,
    #[serde(default)]
    pub team: String,
    pub channel_id: String,
    #[serde(default)]
    pub channel_name: String,
    pub ts: String,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub author_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub permalink: String,
}

pub fn slack_id(value: &str) -> bool {
    (2..=32).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

pub fn slack_ts(value: &str) -> bool {
    let mut parts = value.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), None) if !a.is_empty() && a.len() <= 12 && b.len() == 6 && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
    )
}

fn clip(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

impl SlackNotification {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !KINDS.contains(&self.kind.as_str()) {
            return Err("unknown kind");
        }
        if !slack_id(&self.team_id) || !slack_id(&self.channel_id) {
            return Err("invalid Slack id");
        }
        if !slack_ts(&self.ts) || self.thread_ts.as_deref().is_some_and(|t| !slack_ts(t)) {
            return Err("invalid Slack timestamp");
        }
        if !self.author_id.is_empty() && !slack_id(&self.author_id) {
            return Err("invalid Slack author");
        }
        if !self.permalink.is_empty() && !self.permalink.starts_with("https://") {
            return Err("invalid permalink");
        }
        Ok(())
    }

    pub fn thread_root(&self) -> &str {
        self.thread_ts.as_deref().filter(|t| !t.is_empty()).unwrap_or(&self.ts)
    }

    pub fn event_type(&self) -> &'static str {
        if self.kind == WATCHED_KIND {
            WATCHED_EVENT
        } else {
            SLACK_EVENT
        }
    }

    fn payload(&self, owner: Uuid) -> Value {
        json!({
            "owner_user_id": owner,
            "kind": self.kind,
            "team_id": self.team_id,
            "team": clip(&self.team, 120),
            "slack_channel_id": self.channel_id,
            "channel_name": clip(&self.channel_name, 120),
            "ts": self.ts,
            "thread_ts": self.thread_ts,
            "thread_root": self.thread_root(),
            "author": clip(&self.author, 120),
            "author_id": self.author_id,
            "text": clip(&self.text, MAX_TEXT),
            "permalink": clip(&self.permalink, 500),
        })
    }
}

pub fn hidden_from(event: &WorldEvent, viewer: Uuid) -> bool {
    let owner = event.payload.get("owner_user_id").and_then(Value::as_str);
    let foreign = owner != Some(viewer.to_string().as_str());
    ((event.event_type.starts_with(WATCH_PREFIX) || event.event_type.starts_with("todo.") || event.event_type.starts_with(crate::github_feed::PREFIX)) && foreign)
        || (event.event_type.starts_with("timer.") && owner.is_some() && foreign)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackSource {
    Desktop,
    App,
}

impl SlackSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SlackSource::Desktop => "desktop",
            SlackSource::App => "app",
        }
    }

    pub fn allowed_by(self, preference: &str) -> bool {
        preference == "both" || preference == self.as_str()
    }
}

pub const SOURCES: &[&str] = &["desktop", "app", "both"];

#[derive(Debug, Clone, Copy)]
pub struct SlackDelivery {
    pub source: SlackSource,
    pub addressed: bool,
    pub agent_id: Option<Uuid>,
}

impl SlackDelivery {
    pub fn desktop() -> Self {
        Self { source: SlackSource::Desktop, addressed: false, agent_id: None }
    }

    pub fn app() -> Self {
        Self { source: SlackSource::App, addressed: false, agent_id: None }
    }

    pub fn addressed(agent_id: Option<Uuid>) -> Self {
        Self { source: SlackSource::App, addressed: true, agent_id }
    }
}

impl WorldStore {
    pub async fn record_slack_notification(&self, owner: Uuid, notification: &SlackNotification) -> Result<Option<i64>> {
        self.record_slack(owner, notification, SlackDelivery::desktop()).await
    }

    pub async fn slack_source(&self, user: Uuid) -> Result<String> {
        Ok(sqlx::query_scalar::<_, String>("SELECT slack_source FROM users WHERE id = $1")
            .bind(user)
            .fetch_optional(self.pool())
            .await?
            .unwrap_or_else(|| "both".into()))
    }

    pub async fn set_slack_source(&self, user: Uuid, source: &str) -> Result<()> {
        if !SOURCES.contains(&source) {
            anyhow::bail!("unknown Slack source {source}");
        }
        sqlx::query("UPDATE users SET slack_source = $2 WHERE id = $1").bind(user).bind(source).execute(self.pool()).await?;
        Ok(())
    }

    async fn slack_author_trust(&self, owner: Uuid, notification: &SlackNotification) -> Result<&'static str> {
        if notification.author_id.is_empty() {
            return Ok("outside");
        }
        let trust = sqlx::query_scalar::<_, String>(
            "SELECT CASE WHEN i.user_id = $1 THEN 'owner'
                         WHEN EXISTS (SELECT 1 FROM world_trusted t WHERE t.owner_user_id = $1 AND t.user_id = i.user_id) THEN 'trusted'
                         ELSE 'person' END
             FROM slack_identities i
             WHERE i.team_id = $2 AND i.slack_user_id = $3 AND i.user_id IS NOT NULL",
        )
        .bind(owner)
        .bind(&notification.team_id)
        .bind(&notification.author_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(match trust.as_deref() {
            Some("owner") => "owner",
            Some("trusted") => "trusted",
            Some(_) => "person",
            None => "outside",
        })
    }

    pub async fn record_slack(&self, owner: Uuid, notification: &SlackNotification, delivery: SlackDelivery) -> Result<Option<i64>> {
        if !delivery.addressed && !delivery.source.allowed_by(&self.slack_source(owner).await?) {
            return Ok(None);
        }
        let trust = self.slack_author_trust(owner, notification).await?;
        let mut tx = self.pool().begin().await?;
        let fresh = sqlx::query(
            "INSERT INTO slack_seen (user_id, team_id, channel_id, ts) VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(owner)
        .bind(&notification.team_id)
        .bind(&notification.channel_id)
        .bind(&notification.ts)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !fresh {
            tx.rollback().await?;
            return Ok(None);
        }
        let mut event = NewEvent::new(notification.event_type());
        event.actor_user_id = Some(owner);
        event.payload = notification.payload(owner);
        if let Some(fields) = event.payload.as_object_mut() {
            fields.insert("via".into(), json!(delivery.source.as_str()));
            fields.insert("author_trust".into(), json!(trust));
        }
        let seq = emit_in(&mut tx, &event, None).await?;
        sqlx::query("UPDATE slack_seen SET event_seq = $5 WHERE user_id = $1 AND team_id = $2 AND channel_id = $3 AND ts = $4")
            .bind(owner)
            .bind(&notification.team_id)
            .bind(&notification.channel_id)
            .bind(&notification.ts)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
        if delivery.addressed {
            let target = match delivery.agent_id {
                Some(agent) => sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_profiles WHERE id = $1 AND owner_user_id = $2")
                    .bind(agent)
                    .bind(owner)
                    .fetch_optional(&mut *tx)
                    .await?,
                None => None,
            };
            let target = match target {
                Some(agent) => Some(agent),
                None => sqlx::query_scalar::<_, Option<Uuid>>(
                    "SELECT u.main_agent_id FROM users u JOIN agent_profiles p ON p.id = u.main_agent_id AND p.owner_user_id = u.id WHERE u.id = $1",
                )
                .bind(owner)
                .fetch_optional(&mut *tx)
                .await?
                .flatten(),
            };
            if let Some(agent) = target {
                let addressed = json!({ "addressed": true });
                let updated = sqlx::query("UPDATE agent_inbox SET payload = payload || $3 WHERE agent_id = $1 AND event_seq = $2")
                    .bind(agent)
                    .bind(seq)
                    .bind(&addressed)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                if updated == 0 {
                    let mut payload = event.payload.clone();
                    if let Some(fields) = payload.as_object_mut() {
                        fields.insert("addressed".into(), Value::Bool(true));
                    }
                    sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload) VALUES ($1, $2, $3, $4)")
                        .bind(agent)
                        .bind(SLACK_EVENT)
                        .bind(seq)
                        .bind(&payload)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        sqlx::query("DELETE FROM slack_seen WHERE user_id = $1 AND created_at < NOW() - INTERVAL '30 days'")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(seq))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlackRef {
    Channel(String),
    Name(String),
    Message { channel: String, ts: String, thread_ts: Option<String>, link: String },
}

impl SlackRef {
    pub fn channel(&self) -> Option<&str> {
        match self {
            SlackRef::Channel(c) | SlackRef::Message { channel: c, .. } => Some(c),
            SlackRef::Name(_) => None,
        }
    }

    pub fn thread_root(&self) -> Option<&str> {
        match self {
            SlackRef::Message { ts, thread_ts, .. } => Some(thread_ts.as_deref().unwrap_or(ts)),
            _ => None,
        }
    }
}

fn conversation_id(value: &str) -> bool {
    slack_id(value) && matches!(value.as_bytes()[0], b'C' | b'G' | b'D')
}

fn ts_from_p(raw: &str) -> Option<String> {
    let digits = raw.strip_prefix('p')?;
    if digits.len() < 7 || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (secs, micros) = digits.split_at(digits.len() - 6);
    let ts = format!("{secs}.{micros}");
    slack_ts(&ts).then_some(ts)
}

fn parse_permalink(raw: &str) -> Option<SlackRef> {
    let rest = raw.strip_prefix("https://").or_else(|| raw.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if host != "slack.com" && !host.ends_with(".slack.com") {
        return None;
    }
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let mut parts = path.trim_end_matches('/').split('/');
    if parts.next()? != "archives" {
        return None;
    }
    let channel = parts.next()?.to_string();
    if !conversation_id(&channel) {
        return None;
    }
    let Some(message) = parts.next() else {
        return Some(SlackRef::Channel(channel));
    };
    let ts = ts_from_p(message)?;
    let thread_ts = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "thread_ts")
        .map(|(_, v)| v.to_string())
        .filter(|t| slack_ts(t) && *t != ts);
    Some(SlackRef::Message { channel, ts, thread_ts, link: raw.to_string() })
}

pub fn parse_slack_ref(raw: &str) -> Option<SlackRef> {
    let raw = raw.trim().trim_matches(|c| c == '<' || c == '>');
    if raw.is_empty() {
        return None;
    }
    if raw.contains("://") {
        return parse_permalink(raw);
    }
    if conversation_id(raw) {
        return Some(SlackRef::Channel(raw.to_string()));
    }
    let name = raw.strip_prefix('#').unwrap_or(raw).to_lowercase();
    let valid = (1..=80).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'));
    valid.then_some(SlackRef::Name(name))
}

pub fn watch_match(channel: &str, thread_root: Option<&str>) -> Value {
    match thread_root {
        Some(root) => json!({ "slack_channel_id": channel, "thread_root": root }),
        None => json!({ "slack_channel_id": channel }),
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SlackWatchTargets {
    pub channels: Vec<WatchedChannel>,
    pub threads: Vec<WatchedThread>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WatchedChannel {
    pub channel_id: String,
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WatchedThread {
    pub channel_id: String,
    pub thread_ts: String,
    pub team_id: Option<String>,
    pub since: String,
}

const SLACK_WATCH_SQL: &str = "s.task_id IS NULL AND s.match ? 'slack_channel_id'
    AND s.event_type IN ('slack.', 'slack.watched')
    AND (s.expires_at IS NULL OR s.expires_at > NOW())";

impl WorldStore {
    pub async fn slack_watch_targets(&self, owner: Uuid) -> Result<SlackWatchTargets> {
        let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, f64)>(&format!(
            "SELECT s.match->>'slack_channel_id', s.match->>'thread_root', s.match->>'team_id',
                    EXTRACT(EPOCH FROM MIN(s.created_at))::float8
             FROM agent_subscriptions s JOIN agent_profiles p ON p.id = s.agent_id
             WHERE p.owner_user_id = $1 AND p.paused_reason IS NULL AND {SLACK_WATCH_SQL}
             GROUP BY 1, 2, 3
             ORDER BY 4 DESC
             LIMIT $2"
        ))
        .bind(owner)
        .bind(WATCH_TARGETS_MAX)
        .fetch_all(self.pool())
        .await?;
        let mut targets = SlackWatchTargets::default();
        for (channel_id, thread, team_id, since) in rows {
            if !conversation_id(&channel_id) || team_id.as_deref().is_some_and(|t| !slack_id(t)) {
                continue;
            }
            match thread.filter(|t| slack_ts(t)) {
                Some(thread_ts) => targets.threads.push(WatchedThread { channel_id, thread_ts, team_id, since: format!("{since:.6}") }),
                None => targets.channels.push(WatchedChannel { channel_id, team_id }),
            }
        }
        Ok(targets)
    }

    pub async fn slack_watchers(&self, team: &str, channel: &str, thread_root: &str) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT DISTINCT p.owner_user_id
             FROM agent_subscriptions s JOIN agent_profiles p ON p.id = s.agent_id
             WHERE (s.match->>'slack_channel_id') = $2 AND {SLACK_WATCH_SQL}
               AND (NOT s.match ? 'thread_root' OR s.match->>'thread_root' = $3)
               AND (NOT s.match ? 'team_id' OR s.match->>'team_id' = $1)
               AND p.owner_user_id IS NOT NULL AND p.paused_reason IS NULL"
        ))
        .bind(team)
        .bind(channel)
        .bind(thread_root)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn slack_channel_named(&self, owner: Uuid, name: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT payload->>'slack_channel_id' FROM world_events
             WHERE starts_with(type, 'slack.') AND payload->>'owner_user_id' = $1::text
               AND lower(payload->>'channel_name') = $2 AND created_at > NOW() - INTERVAL '30 days'
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(owner)
        .bind(name.trim_start_matches('#').to_lowercase())
        .fetch_optional(self.pool())
        .await?
        .filter(|c| conversation_id(c)))
    }

    pub async fn mark_slack_seen(&self, owner: Uuid, team: &str, channel: &str, ts: &str) -> Result<()> {
        sqlx::query("INSERT INTO slack_seen (user_id, team_id, channel_id, ts) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(owner)
            .bind(team)
            .bind(channel)
            .bind(ts)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SlackNotification {
        SlackNotification {
            kind: "dm".into(),
            team_id: "T0123ABC".into(),
            team: "fal".into(),
            channel_id: "D0456DEF".into(),
            channel_name: "Ana".into(),
            ts: "1760800000.123456".into(),
            thread_ts: None,
            author: "Ana".into(),
            author_id: "U0789GHI".into(),
            text: "hello".into(),
            permalink: "https://fal.slack.com/archives/D0456DEF/p1760800000123456".into(),
        }
    }

    #[test]
    fn accepts_a_valid_notification() {
        assert!(sample().validate().is_ok());
    }

    #[test]
    fn rejects_bad_ids_and_timestamps() {
        let mut bad = sample();
        bad.channel_id = "d0456".into();
        assert!(bad.validate().is_err());
        let mut bad = sample();
        bad.ts = "1760800000".into();
        assert!(bad.validate().is_err());
        let mut bad = sample();
        bad.kind = "reaction".into();
        assert!(bad.validate().is_err());
        let mut bad = sample();
        bad.permalink = "javascript:alert(1)".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn payload_clips_long_text_and_carries_owner() {
        let mut long = sample();
        long.text = "x".repeat(5000);
        let owner = Uuid::new_v4();
        let payload = long.payload(owner);
        assert_eq!(payload["text"].as_str().unwrap().chars().count(), MAX_TEXT);
        assert_eq!(payload["owner_user_id"], json!(owner));
    }

    #[test]
    fn only_the_owner_sees_slack_events() {
        let owner = Uuid::new_v4();
        let event = WorldEvent {
            seq: 1,
            org_id: None,
            event_type: SLACK_EVENT.into(),
            agent_id: None,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: Some(owner),
            payload: sample().payload(owner),
            created_at: chrono::Utc::now(),
        };
        assert!(!hidden_from(&event, owner));
        assert!(hidden_from(&event, Uuid::new_v4()));
    }

    #[tokio::test]
    async fn subscribed_agents_get_grouped_slack() -> Result<()> {
        let Ok(url) = std::env::var("SLACK_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut users = Vec::new();
        for who in ["a", "b"] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
                .bind(format!("{who}-{tag}@test.dev"))
                .fetch_one(&pool)
                .await?;
            users.push(id);
        }
        let mut agents = Vec::new();
        for (name, owner) in [("exact", users[0]), ("prefix", users[0]), ("quiet", users[0]), ("stranger", users[1])] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id",
            )
            .bind(format!("{name}-{tag}"))
            .bind(name)
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            agents.push(id);
        }
        for u in &users {
            crate::orgs::join_default_for_tests(&pool, *u, None).await?;
        }
        let slack = vec!["slack.notification".to_string()];
        store.set_subscriptions(agents[0], &slack, &[]).await?;
        store.set_subscriptions(agents[1], &["slack.".to_string()], &[]).await?;
        store.set_subscriptions(agents[2], &slack, &[]).await?;
        store.set_subscriptions(agents[2], &[], &slack).await?;
        store.set_subscriptions(agents[3], &slack, &[]).await?;
        assert_eq!(store.subscriptions(agents[0]).await?, slack);

        for n in 0..3 {
            let mut note = sample();
            note.ts = format!("176080000{n}.12345{n}");
            note.channel_id = format!("D{}", tag[..8].to_uppercase());
            assert!(store.record_slack_notification(users[0], &note).await?.is_some());
        }
        let count = |agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'slack.notification' AND (payload->>'subscribed')::boolean",
                )
                .bind(agent)
                .fetch_one(&pool)
                .await
            }
        };
        assert_eq!(count(agents[0]).await?, 3);
        assert_eq!(count(agents[1]).await?, 3);
        assert_eq!(count(agents[2]).await?, 0);
        assert_eq!(count(agents[3]).await?, 0);

        let ready = |agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, bool>(&format!(
                    "SELECT {} FROM agent_profiles a WHERE a.id = $1",
                    crate::world::inbox_ready_sql("a")
                ))
                .bind(agent)
                .fetch_one(&pool)
                .await
            }
        };
        assert!(!ready(agents[0]).await?);
        let wait = store.inbox_settle_secs(Some(agents[0]), false).await?.expect("settling");
        assert!(wait > 0.0 && wait <= crate::world::SLACK_SETTLE_SECS as f64);
        sqlx::query("UPDATE agent_inbox SET created_at = created_at - INTERVAL '10 seconds' WHERE agent_id = $1")
            .bind(agents[0])
            .execute(&pool)
            .await?;
        assert!(ready(agents[0]).await?);
        assert!(store.inbox_settle_secs(Some(agents[0]), false).await?.is_none());
        Ok(())
    }

    #[test]
    fn slack_refs_parse() {
        assert_eq!(parse_slack_ref("C0123ABCD"), Some(SlackRef::Channel("C0123ABCD".into())));
        assert_eq!(parse_slack_ref("#Incidents"), Some(SlackRef::Name("incidents".into())));
        assert_eq!(parse_slack_ref("eng-alerts"), Some(SlackRef::Name("eng-alerts".into())));
        assert_eq!(parse_slack_ref("https://acme.slack.com/archives/C0123ABCD"), Some(SlackRef::Channel("C0123ABCD".into())));
        let root = parse_slack_ref("https://acme.slack.com/archives/C0123ABCD/p1760800000123456").unwrap();
        assert_eq!(root.channel(), Some("C0123ABCD"));
        assert_eq!(root.thread_root(), Some("1760800000.123456"));
        let reply = parse_slack_ref("<https://acme.slack.com/archives/C0123ABCD/p1760800500000001?thread_ts=1760800000.123456&cid=C0123ABCD>").unwrap();
        assert_eq!(reply.thread_root(), Some("1760800000.123456"));
        let same = parse_slack_ref("https://acme.slack.com/archives/C0123ABCD/p1760800000123456?thread_ts=1760800000.123456").unwrap();
        assert_eq!(same.thread_root(), Some("1760800000.123456"));
        assert_eq!(parse_slack_ref("https://evil.example/archives/C0123ABCD/p1760800000123456"), None);
        assert_eq!(parse_slack_ref("https://acme.slack.com/archives/x/p1"), None);
        assert_eq!(parse_slack_ref("https://acme.slack.com/archives/C0123ABCD/pnope"), None);
        assert_eq!(parse_slack_ref("#bad name!"), None);
        assert_eq!(parse_slack_ref(""), None);
    }

    #[test]
    fn thread_root_matches_root_and_replies() {
        let watch = watch_match("C0123ABCD", Some("1760800000.123456"));
        let mut root = sample();
        root.channel_id = "C0123ABCD".into();
        root.ts = "1760800000.123456".into();
        let mut reply = root.clone();
        reply.ts = "1760800100.000001".into();
        reply.thread_ts = Some("1760800000.123456".into());
        let mut other = root.clone();
        other.ts = "1760800200.000001".into();
        let contains = |payload: &Value| watch.as_object().unwrap().iter().all(|(k, v)| payload.get(k) == Some(v));
        let owner = Uuid::new_v4();
        assert!(contains(&root.payload(owner)));
        assert!(contains(&reply.payload(owner)));
        assert!(!contains(&other.payload(owner)));
        reply.kind = WATCHED_KIND.into();
        assert!(reply.validate().is_ok());
        assert_eq!(reply.event_type(), WATCHED_EVENT);
        assert_eq!(root.event_type(), SLACK_EVENT);
    }

    #[test]
    fn watched_slack_stays_with_its_owner() {
        let owner = Uuid::new_v4();
        let mut note = sample();
        note.kind = WATCHED_KIND.into();
        let event = WorldEvent {
            seq: 1,
            org_id: None,
            event_type: WATCHED_EVENT.into(),
            agent_id: None,
            session_id: None,
            run_id: None,
            task_id: None,
            actor_user_id: Some(owner),
            payload: note.payload(owner),
            created_at: chrono::Utc::now(),
        };
        assert!(!hidden_from(&event, owner));
        assert!(hidden_from(&event, Uuid::new_v4()));
    }

    #[tokio::test]
    async fn watched_slack_reaches_only_scoped_watches() -> Result<()> {
        let Ok(url) = std::env::var("SLACK_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let up = tag[..8].to_uppercase();
        let chan = |n: &str| format!("C{n}{up}");
        let mut users = Vec::new();
        for who in ["a", "b"] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
                .bind(format!("watch-{who}-{tag}@test.dev"))
                .fetch_one(&pool)
                .await?;
            users.push(id);
        }
        let mut agents = Vec::new();
        for (name, owner) in [("blanket", users[0]), ("channel", users[0]), ("thread", users[0]), ("poster", users[0]), ("lapsed", users[0]), ("stranger", users[1])] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id",
            )
            .bind(format!("w{name}-{tag}"))
            .bind(name)
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            agents.push(id);
        }
        for u in &users {
            crate::orgs::join_default_for_tests(&pool, *u, None).await?;
        }
        let (blanket, channel, thread, poster, lapsed, stranger) = (agents[0], agents[1], agents[2], agents[3], agents[4], agents[5]);
        store.set_subscriptions(blanket, &[WATCH_PREFIX.to_string(), SLACK_EVENT.to_string()], &[]).await?;
        store.add_watch(channel, WATCH_PREFIX, watch_match(&chan("1"), None), Some(7), Some("#ops")).await?;
        store.add_watch(thread, WATCH_PREFIX, watch_match(&chan("2"), Some("1760800000.000001")), Some(7), None).await?;
        store.add_watch(lapsed, WATCH_PREFIX, watch_match(&chan("9"), None), Some(7), None).await?;
        sqlx::query("UPDATE agent_subscriptions SET expires_at = NOW() - INTERVAL '1 minute' WHERE agent_id = $1").bind(lapsed).execute(&pool).await?;
        store.add_watch(stranger, WATCH_PREFIX, watch_match(&chan("1"), None), Some(7), None).await?;

        let first = store.extend_watch(poster, WATCH_PREFIX, watch_match(&chan("3"), Some("1760800000.000009")), POST_WATCH_DAYS, None).await?;
        let soon = first.expires_at.expect("auto watches expire");
        assert!(soon < chrono::Utc::now() + chrono::Duration::days(POST_WATCH_DAYS) + chrono::Duration::minutes(1));
        sqlx::query("UPDATE agent_subscriptions SET expires_at = NOW() + INTERVAL '20 days' WHERE id = $1").bind(first.id).execute(&pool).await?;
        let again = store.extend_watch(poster, WATCH_PREFIX, watch_match(&chan("3"), Some("1760800000.000009")), POST_WATCH_DAYS, None).await?;
        assert_eq!(again.id, first.id);
        assert!(again.expires_at.unwrap() > chrono::Utc::now() + chrono::Duration::days(19));
        let longer = store.add_watch(thread, WATCH_PREFIX, watch_match(&chan("2"), Some("1760800000.000001")), Some(30), None).await?;
        let shorter = store.add_watch(thread, WATCH_PREFIX, watch_match(&chan("2"), Some("1760800000.000001")), Some(1), None).await?;
        assert_eq!(longer.id, shorter.id);
        assert!(shorter.expires_at.unwrap() < longer.expires_at.unwrap() - chrono::Duration::days(28));
        assert_eq!(store.watches(thread).await?.len(), 1);
        let label = store.watches(channel).await?.remove(0).label();
        assert!(label.starts_with("Slack channel #ops ("), "{label}");

        let note = |kind: &str, channel_id: String, ts: &str, thread: Option<&str>| SlackNotification {
            kind: kind.into(),
            team_id: "T0123ABC".into(),
            team: "acme".into(),
            channel_id,
            channel_name: "ops".into(),
            ts: ts.into(),
            thread_ts: thread.map(String::from),
            author: "Ana".into(),
            author_id: "U0789GHI".into(),
            text: "hello".into(),
            permalink: String::new(),
        };
        let owner = users[0];
        let app = SlackDelivery::app();
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("1"), "1760800001.000001", None), app).await?.is_some());
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("2"), "1760800000.000001", None), app).await?.is_some());
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("2"), "1760800002.000001", Some("1760800000.000001")), app).await?.is_some());
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("2"), "1760800003.000001", None), app).await?.is_some());
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("9"), "1760800004.000001", None), app).await?.is_some());
        assert!(store.record_slack(owner, &note("mention", chan("1"), "1760800005.000001", None), app).await?.is_some());
        store.mark_slack_seen(owner, "T0123ABC", &chan("3"), "1760800006.000001").await?;
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("3"), "1760800006.000001", Some("1760800000.000009")), app).await?.is_none());
        assert!(store.record_slack(owner, &note(WATCHED_KIND, chan("3"), "1760800007.000001", Some("1760800000.000009")), app).await?.is_some());

        let items = |agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, String)>("SELECT kind, payload->>'ts' FROM agent_inbox WHERE agent_id = $1 ORDER BY created_at, event_seq")
                    .bind(agent)
                    .fetch_all(&pool)
                    .await
            }
        };
        let ts_of = |rows: Vec<(String, String)>| rows.into_iter().map(|(_, ts)| ts).collect::<Vec<_>>();
        assert_eq!(items(blanket).await?, vec![(SLACK_EVENT.to_string(), "1760800005.000001".to_string())]);
        assert_eq!(
            items(channel).await?,
            vec![(WATCHED_EVENT.to_string(), "1760800001.000001".to_string()), (SLACK_EVENT.to_string(), "1760800005.000001".to_string())]
        );
        assert_eq!(ts_of(items(thread).await?), vec!["1760800000.000001".to_string(), "1760800002.000001".to_string()]);
        assert_eq!(ts_of(items(poster).await?), vec!["1760800007.000001".to_string()]);
        assert!(items(lapsed).await?.is_empty());
        assert!(items(stranger).await?.is_empty());
        let watched: Value = sqlx::query_scalar("SELECT payload->'watch' FROM agent_inbox WHERE agent_id = $1 ORDER BY created_at, event_seq LIMIT 1")
            .bind(thread)
            .fetch_one(&pool)
            .await?;
        assert_eq!(watched["thread_root"], json!("1760800000.000001"));

        let targets = store.slack_watch_targets(owner).await?;
        let channels: Vec<String> = targets.channels.iter().map(|c| c.channel_id.clone()).collect();
        assert_eq!(channels, vec![chan("1")]);
        let mut threads: Vec<(String, String)> = targets.threads.iter().map(|t| (t.channel_id.clone(), t.thread_ts.clone())).collect();
        threads.sort();
        assert_eq!(threads, vec![(chan("2"), "1760800000.000001".to_string()), (chan("3"), "1760800000.000009".to_string())]);
        assert!(targets.threads.iter().all(|t| t.since.parse::<f64>().is_ok()));

        let mut watchers = store.slack_watchers("T0123ABC", &chan("1"), "1760800010.000001").await?;
        watchers.sort();
        let mut both = users.clone();
        both.sort();
        assert_eq!(watchers, both);
        assert_eq!(store.slack_watchers("T0123ABC", &chan("2"), "1760800000.000001").await?, vec![owner]);
        assert!(store.slack_watchers("T0123ABC", &chan("2"), "1760800099.000001").await?.is_empty());
        assert!(store.slack_watchers("T0123ABC", &chan("9"), "1760800010.000001").await?.is_empty());
        Ok(())
    }
}
