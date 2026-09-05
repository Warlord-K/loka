use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::WorldStore;

pub const MAX_LINKS: usize = 8;
const EXCERPT_CHARS: usize = 280;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRef {
    Thread(String),
    Channel(i64),
}

pub fn parse_ref(raw: &str) -> Option<MessageRef> {
    let raw = raw.trim();
    if let Some(n) = raw.get(..2).filter(|h| h.eq_ignore_ascii_case("ch")).map(|_| &raw[2..]) {
        if !n.is_empty() && n.len() <= 18 && n.chars().all(|c| c.is_ascii_digit()) {
            return n.parse().ok().map(MessageRef::Channel);
        }
    }
    let hex: String = raw.chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
    if (6..=32).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(MessageRef::Thread(hex));
    }
    None
}

pub fn short_ref(id: Uuid) -> String {
    id.simple().to_string()[..8].to_string()
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn scan(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let head = &bytes[i..i + 4];
        let lead_ok = i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric();
        if (head.eq_ignore_ascii_case(b"msg:") || head.eq_ignore_ascii_case(b"msg=")) && lead_ok {
            let start = i + 4;
            let end = text[start..].find(|c: char| !token_char(c)).map(|n| start + n).unwrap_or(text.len());
            let token = text[start..end].trim_end_matches('-');
            if parse_ref(token).is_some() {
                out.push((start, start + token.len()));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

pub fn refs_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (s, e) in scan(text) {
        let token = text[s..e].to_string();
        if !out.contains(&token) {
            out.push(token);
        }
        if out.len() >= MAX_LINKS {
            break;
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedMessage {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    pub agent_id: Option<Uuid>,
    pub channel_id: Option<Uuid>,
    pub channel_name: Option<String>,
    pub channel_kind: Option<String>,
    pub author: String,
    pub author_kind: String,
    pub author_emoji: Option<String>,
    pub excerpt: String,
    pub created_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct ThreadRow {
    id: Uuid,
    agent_id: Uuid,
    author_kind: String,
    author: String,
    author_emoji: Option<String>,
    body: String,
    created_at: DateTime<Utc>,
    scoped: bool,
}

#[derive(FromRow)]
struct ChannelRow {
    id: i64,
    channel_id: Uuid,
    channel_name: String,
    channel_kind: String,
    author_kind: String,
    author: String,
    author_emoji: Option<String>,
    body: String,
    created_at: DateTime<Utc>,
}

pub fn excerpt(body: &str, max: usize) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}...", cut.trim_end())
}

fn bounds(hex: &str) -> Option<(Uuid, Uuid)> {
    let pad = 32 - hex.len();
    let low = Uuid::parse_str(&format!("{hex}{}", "0".repeat(pad))).ok()?;
    let high = Uuid::parse_str(&format!("{hex}{}", "f".repeat(pad))).ok()?;
    Some((low, high))
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FoundMessage {
    pub reference: String,
    pub place: String,
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

const AUTHOR: &str = "COALESCE(a.name, u.name, split_part(u.email, '@', 1), 'system')";

impl WorldStore {
    pub async fn resolve_message(&self, raw: &str, scope: Option<Uuid>) -> Result<Option<LinkedMessage>> {
        match parse_ref(raw) {
            Some(MessageRef::Channel(id)) => {
                let row = sqlx::query_as::<_, ChannelRow>(&format!(
                    "SELECT m.id, m.channel_id, c.name AS channel_name, c.kind AS channel_kind, m.author_kind,
                            {AUTHOR} AS author, a.avatar_emoji AS author_emoji, m.body, m.created_at
                     FROM channel_messages m
                     JOIN channels c ON c.id = m.channel_id
                     LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
                     LEFT JOIN users u ON u.id = m.author_user_id
                     WHERE m.id = $1"
                ))
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
                Ok(row.map(|r| LinkedMessage {
                    reference: format!("ch{}", r.id),
                    kind: "channel".into(),
                    agent_id: None,
                    channel_id: Some(r.channel_id),
                    channel_name: Some(r.channel_name),
                    channel_kind: Some(r.channel_kind),
                    author: r.author,
                    author_kind: r.author_kind,
                    author_emoji: r.author_emoji,
                    excerpt: excerpt(&r.body, EXCERPT_CHARS),
                    created_at: r.created_at,
                }))
            }
            Some(MessageRef::Thread(hex)) => {
                let Some((low, high)) = bounds(&hex) else { return Ok(None) };
                let rows = sqlx::query_as::<_, ThreadRow>(&format!(
                    "SELECT m.id, m.agent_id, m.author_kind, {AUTHOR} AS author, a.avatar_emoji AS author_emoji,
                            m.body, m.created_at,
                            COALESCE($3::uuid IS NOT NULL AND (m.agent_id = $3 OR m.author_agent_id = $3), false) AS scoped
                     FROM agent_messages m
                     LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
                     LEFT JOIN users u ON u.id = m.author_user_id
                     WHERE m.id BETWEEN $1 AND $2
                     ORDER BY scoped DESC, m.created_at DESC
                     LIMIT 2"
                ))
                .bind(low)
                .bind(high)
                .bind(scope)
                .fetch_all(self.pool())
                .await?;
                let pick = match rows.as_slice() {
                    [only] => Some(only),
                    [first, second] if first.scoped && !second.scoped => Some(first),
                    _ => None,
                };
                Ok(pick.map(|r| LinkedMessage {
                    reference: r.id.to_string(),
                    kind: "agent".into(),
                    agent_id: Some(r.agent_id),
                    channel_id: None,
                    channel_name: None,
                    channel_kind: None,
                    author: r.author.clone(),
                    author_kind: r.author_kind.clone(),
                    author_emoji: r.author_emoji.clone(),
                    excerpt: excerpt(&r.body, EXCERPT_CHARS),
                    created_at: r.created_at,
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn expand_message_links(&self, text: &str, scope: Uuid) -> String {
        let spans = scan(text);
        if spans.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len() + 32 * spans.len());
        let mut at = 0;
        for (s, e) in spans {
            out.push_str(&text[at..s]);
            let token = &text[s..e];
            let full = match parse_ref(token) {
                Some(MessageRef::Thread(hex)) if hex.len() < 32 => match self.resolve_message(token, Some(scope)).await {
                    Ok(Some(found)) => found.reference,
                    _ => token.to_string(),
                },
                _ => token.to_string(),
            };
            out.push_str(&full);
            at = e;
        }
        out.push_str(&text[at..]);
        out
    }

    pub async fn linked_messages(&self, text: &str, scope: Option<Uuid>) -> Result<Vec<LinkedMessage>> {
        let mut out = Vec::new();
        for token in refs_in(text) {
            if let Some(found) = self.resolve_message(&token, scope).await? {
                if !out.iter().any(|l: &LinkedMessage| l.reference == found.reference) {
                    out.push(found);
                }
            }
        }
        Ok(out)
    }

    pub async fn sees_message(&self, message_id: Uuid, viewer: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM agent_messages m WHERE m.id = $1 AND {})",
            crate::guests::thread_filter("m.agent_id", "$2")
        ))
        .bind(message_id)
        .bind(viewer)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn last_turn_replies(&self, agent_id: Uuid, session_id: Uuid) -> Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, body FROM agent_messages
             WHERE agent_id = $1 AND author_kind = 'agent' AND author_agent_id = $1
               AND run_id = (SELECT id FROM session_runs WHERE session_id = $2 AND kind = 'turn' AND status = 'completed'
                             ORDER BY created_at DESC LIMIT 1)
             ORDER BY created_at",
        )
        .bind(agent_id)
        .bind(session_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn find_messages(&self, agent_id: Uuid, query: &str, limit: i64) -> Result<Vec<FoundMessage>> {
        let pattern = format!("%{}%", query.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        Ok(sqlx::query_as::<_, FoundMessage>(&format!(
            "SELECT * FROM (
               SELECT left(replace(m.id::text, '-', ''), 8) AS reference,
                      CASE WHEN m.agent_id = $1 THEN 'your thread' ELSE 'to ' || COALESCE(t.name, 'another agent') END AS place,
                      {AUTHOR} AS author, m.body, m.created_at
               FROM agent_messages m
               LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
               LEFT JOIN users u ON u.id = m.author_user_id
               LEFT JOIN agent_profiles t ON t.id = m.agent_id
               WHERE (m.agent_id = $1 OR (m.author_kind = 'agent' AND m.author_agent_id = $1))
                 AND {thread}
                 AND m.body ILIKE $2
               UNION ALL
               SELECT 'ch' || m.id::text AS reference,
                      CASE WHEN c.kind = 'dm' THEN 'a direct message' ELSE '#' || c.name END AS place,
                      {AUTHOR} AS author, m.body, m.created_at
               FROM channel_messages m
               JOIN channels c ON c.id = m.channel_id
               JOIN channel_members cm ON cm.channel_id = m.channel_id AND cm.member_kind = 'agent' AND cm.member_id = $1
               LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
               LEFT JOIN users u ON u.id = m.author_user_id
               WHERE m.body ILIKE $2
             ) found ORDER BY created_at DESC LIMIT $3",
            thread = crate::guests::thread_filter("$1", "(SELECT owner_user_id FROM agent_profiles WHERE id = $1)")
        ))
        .bind(agent_id)
        .bind(pattern)
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct SlackEmbed {
    pub author: Option<String>,
    pub channel_name: Option<String>,
    pub team: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl WorldStore {
    pub async fn slack_embed(&self, owner: Uuid, url: &str) -> Result<Option<SlackEmbed>> {
        let base = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
        Ok(sqlx::query_as::<_, SlackEmbed>(
            "SELECT payload->>'author' AS author, payload->>'channel_name' AS channel_name, payload->>'team' AS team,
                    payload->>'kind' AS kind, left(payload->>'text', 400) AS text, created_at
             FROM world_events
             WHERE type = 'slack.notification' AND payload->>'owner_user_id' = $1
               AND rtrim(split_part(split_part(payload->>'permalink', '?', 1), '#', 1), '/') = $2
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(owner.to_string())
        .bind(base)
        .fetch_optional(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refs() {
        assert_eq!(parse_ref("ch42"), Some(MessageRef::Channel(42)));
        assert_eq!(parse_ref("c1234567"), Some(MessageRef::Thread("c1234567".into())));
        assert_eq!(parse_ref("3F2A91C0"), Some(MessageRef::Thread("3f2a91c0".into())));
        assert_eq!(
            parse_ref("3f2a91c0-1111-2222-3333-444455556666"),
            Some(MessageRef::Thread("3f2a91c0111122223333444455556666".into()))
        );
        assert_eq!(parse_ref("abc"), None);
        assert_eq!(parse_ref("hello123"), None);
    }

    #[test]
    fn finds_refs_in_text() {
        let text = "as I said [here](msg:3f2a91c0), see msg:ch12 and https://loka.ygpt.dev/world?msg=abcdef12. not xmsg:abcdef12 or msg:zz";
        assert_eq!(refs_in(text), vec!["3f2a91c0", "ch12", "abcdef12"]);
    }

    #[tokio::test]
    async fn expands_and_finds_links() -> Result<()> {
        let Ok(url) = std::env::var("LINKS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("links-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'links', 'claude', 'sonnet', $2) RETURNING id")
            .bind(format!("links-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body) VALUES ($1, 'agent', $1, 'zebra crossing plan') RETURNING id",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        let short = short_ref(id);
        let text = format!("see [this](msg:{short}) and msg:{short}x and msg:ch999999999");
        let expanded = store.expand_message_links(&text, agent).await;
        assert_eq!(expanded, format!("see [this](msg:{id}) and msg:{short}x and msg:ch999999999"));
        let found = store.find_messages(agent, "zebra crossing", 5).await?;
        assert!(found.iter().any(|m| m.reference == short));
        let linked = store.linked_messages(&expanded, None).await?;
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].excerpt, "zebra crossing plan");
        sqlx::query("DELETE FROM agent_messages WHERE id = $1").bind(id).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn bounds_cover_prefix() {
        let (low, high) = bounds("3f2a91c0").unwrap();
        assert_eq!(low.to_string(), "3f2a91c0-0000-0000-0000-000000000000");
        assert_eq!(high.to_string(), "3f2a91c0-ffff-ffff-ffff-ffffffffffff");
    }
}
