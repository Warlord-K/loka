use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::world::{emit_in, NewEvent, WorldStore};

const AGENT_MENTION_LIMIT: i64 = 6;
const CONTEXT_MESSAGES: i64 = 12;
const BROADCAST_AGENT_LIMIT: usize = 24;
pub const THREAD_LIMIT: i64 = 500;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Channel {
    pub id: Uuid,
    pub name: String,
    pub topic: Option<String>,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ChannelSummary {
    pub id: Uuid,
    pub name: String,
    pub topic: Option<String>,
    pub kind: String,
    pub member: bool,
    pub unread: i64,
    pub last_message_at: Option<DateTime<Utc>>,
    pub peers: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ChannelMember {
    pub member_kind: String,
    pub member_id: Uuid,
    pub name: String,
    pub emoji: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: i64,
    pub channel_id: Uuid,
    pub author_kind: String,
    pub author_user_id: Option<Uuid>,
    pub author_agent_id: Option<Uuid>,
    pub author_name: Option<String>,
    pub author_emoji: Option<String>,
    pub body: String,
    pub mentions: Value,
    pub created_at: DateTime<Utc>,
    pub meta: Value,
    pub edited_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_to: Option<i64>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_count: i32,
    #[sqlx(default)]
    #[serde(default)]
    pub last_reply_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub reply_authors: Option<Value>,
    #[sqlx(default)]
    #[serde(default)]
    pub seen_by: Value,
}

type MentionUser = (Uuid, Option<String>, Option<String>, Option<Uuid>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    NotFound,
    HasReplies,
}

#[derive(Debug, Clone, Copy)]
pub enum Author {
    User(Uuid),
    Agent(Uuid),
}

const MESSAGE_SELECT: &str = "SELECT m.id, m.channel_id, m.author_kind, m.author_user_id, m.author_agent_id,
        COALESCE(a.name, u.name, u.email, 'system') AS author_name, a.avatar_emoji AS author_emoji,
        m.body, m.mentions, m.created_at, m.meta, m.edited_at, m.reply_to, m.reply_count, m.last_reply_at,
        CASE WHEN m.reply_count > 0 THEN (
          SELECT jsonb_agg(jsonb_build_object('kind', p.author_kind, 'user_id', p.author_user_id, 'agent_id', p.author_agent_id, 'name', p.name) ORDER BY p.at DESC)
          FROM (
            SELECT r.author_kind, r.author_user_id, r.author_agent_id,
                   COALESCE(ra.name, ru.name, split_part(ru.email, '@', 1)) AS name, MAX(r.created_at) AS at
            FROM channel_messages r
            LEFT JOIN agent_profiles ra ON ra.id = r.author_agent_id
            LEFT JOIN users ru ON ru.id = r.author_user_id
            WHERE r.reply_to = m.id
            GROUP BY 1, 2, 3, 4 ORDER BY at DESC LIMIT 4
          ) p
        ) END AS reply_authors,
        CASE WHEN m.author_kind = 'user' THEN COALESCE((SELECT jsonb_agg(s.agent_id ORDER BY s.seen_at) FROM channel_message_seen s WHERE s.message_id = m.id), '[]'::jsonb) ELSE '[]'::jsonb END AS seen_by
     FROM channel_messages m
     LEFT JOIN agent_profiles a ON a.id = m.author_agent_id
     LEFT JOIN users u ON u.id = m.author_user_id";

fn handle(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

pub fn mention_tokens(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '-' | '.')) {
                end += 1;
            }
            if end > start {
                let token: String = chars[start..end].iter().collect();
                out.push(handle(token.trim_end_matches('.')));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn broadcast_reaches(broadcast: &str, agent: &crate::world::WorldAgent) -> bool {
    agent.paused_reason.is_none() && (broadcast == "channel" || !matches!(agent.status.as_str(), "away" | "error"))
}

pub fn broadcast_token(tokens: &[String]) -> Option<&'static str> {
    if tokens.iter().any(|t| t == "channel") {
        Some("channel")
    } else if tokens.iter().any(|t| t == "here") {
        Some("here")
    } else {
        None
    }
}

pub(crate) async fn agent_reaches_channel_with(
    conn: &mut PgConnection,
    channel_id: Uuid,
    agent: Uuid,
    public_too: bool,
) -> std::result::Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT ($3 AND c.kind = 'channel')
             OR EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id AND m.member_kind = 'agent' AND m.member_id = $2)
             OR EXISTS (SELECT 1 FROM agent_teams t WHERE t.channel_id = c.id AND (
                    EXISTS (SELECT 1 FROM agent_team_members tm WHERE tm.team_id = t.id AND tm.member_kind = 'agent' AND tm.member_id = $2)
                 OR EXISTS (SELECT 1 FROM org_members om WHERE om.user_id = t.owner_user_id AND om.org_id = c.org_id AND om.main_agent_id = $2)))
         FROM channels c WHERE c.id = $1",
    )
    .bind(channel_id)
    .bind(agent)
    .bind(public_too)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(false))
}

impl WorldStore {
    pub async fn channels_for_user(&self, user_id: Uuid, org: Uuid) -> Result<Vec<ChannelSummary>> {
        Ok(sqlx::query_as::<_, ChannelSummary>(
            "SELECT c.id, c.name, c.topic, c.kind,
                    EXISTS (SELECT 1 FROM channel_members cm WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $1) AS member,
                    (SELECT COUNT(*) FROM channel_messages m WHERE m.channel_id = c.id
                       AND m.reply_to IS NULL
                       AND m.id > COALESCE((SELECT last_read_id FROM channel_reads r WHERE r.user_id = $1 AND r.channel_id = c.id), 0)
                       AND (m.author_user_id IS DISTINCT FROM $1)) AS unread,
                    (SELECT MAX(created_at) FROM channel_messages m WHERE m.channel_id = c.id) AS last_message_at,
                    CASE WHEN c.kind = 'dm' THEN
                      (SELECT string_agg(COALESCE(u.name, split_part(u.email, '@', 1)), ', ' ORDER BY u.name)
                       FROM channel_members cm JOIN users u ON u.id = cm.member_id
                       WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id <> $1)
                    END AS peers
             FROM channels c
             WHERE c.org_id = $2 AND (c.kind = 'channel'
                OR EXISTS (SELECT 1 FROM channel_members cm WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $1))
             ORDER BY c.kind, c.name",
        )
        .bind(user_id)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn channels_for_agent(&self, agent_id: Uuid) -> Result<Vec<Channel>> {
        Ok(sqlx::query_as::<_, Channel>(
            "SELECT c.id, c.name, c.topic, c.kind, c.created_at, c.org_id FROM channels c
             JOIN channel_members cm ON cm.channel_id = c.id AND cm.member_kind = 'agent' AND cm.member_id = $1
             WHERE c.org_id = (SELECT p.org_id FROM agent_profiles p WHERE p.id = $1)
             ORDER BY c.name",
        )
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn find_channel(&self, org: Option<Uuid>, name_or_id: &str) -> Result<Option<Channel>> {
        let key = name_or_id.trim().trim_start_matches('#');
        Ok(sqlx::query_as::<_, Channel>(
            "SELECT id, name, topic, kind, created_at, org_id FROM channels
             WHERE org_id = $2 AND (id::text = $1 OR (kind IN ('channel', 'team') AND lower(name) = lower($1)))
             ORDER BY (kind = 'team') DESC LIMIT 1",
        )
        .bind(key)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn get_channel(&self, id: Uuid) -> Result<Option<Channel>> {
        Ok(sqlx::query_as::<_, Channel>("SELECT id, name, topic, kind, created_at, org_id FROM channels WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn create_channel(&self, org: Option<Uuid>, name: &str, topic: Option<&str>, created_by: Uuid) -> Result<Channel> {
        let name = name.trim().trim_start_matches('#').to_lowercase().replace(' ', "-");
        if name.is_empty() {
            bail!("channel needs a name");
        }
        let channel = sqlx::query_as::<_, Channel>(
            "INSERT INTO channels (name, topic, created_by, org_id) VALUES ($1, $2, $3, $4)
             RETURNING id, name, topic, kind, created_at, org_id",
        )
        .bind(&name)
        .bind(topic)
        .bind(created_by)
        .bind(org)
        .fetch_one(self.pool())
        .await?;
        self.add_member(channel.id, "user", created_by).await?;
        Ok(channel)
    }

    pub async fn open_dm(&self, org: Uuid, a: Uuid, b: Uuid) -> Result<Channel> {
        if !(self.shares_org(a, b, org).await? || (a == b && self.is_org_member(org, a).await?)) {
            bail!("you can only message people in this organization");
        }
        if let Some(existing) = sqlx::query_as::<_, Channel>(
            "SELECT c.id, c.name, c.topic, c.kind, c.created_at, c.org_id FROM channels c
             WHERE c.kind = 'dm' AND c.org_id = $3
               AND EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id AND m.member_kind = 'user' AND m.member_id = $1)
               AND EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id AND m.member_kind = 'user' AND m.member_id = $2)
               AND (SELECT COUNT(*) FROM channel_members m WHERE m.channel_id = c.id) = CASE WHEN $1 = $2 THEN 1 ELSE 2 END
             LIMIT 1",
        )
        .bind(a)
        .bind(b)
        .bind(org)
        .fetch_optional(self.pool())
        .await?
        {
            return Ok(existing);
        }
        let channel = sqlx::query_as::<_, Channel>(
            "INSERT INTO channels (name, kind, created_by, org_id) VALUES ('dm', 'dm', $1, $2) RETURNING id, name, topic, kind, created_at, org_id",
        )
        .bind(a)
        .bind(org)
        .fetch_one(self.pool())
        .await?;
        self.add_member(channel.id, "user", a).await?;
        self.add_member(channel.id, "user", b).await?;
        Ok(channel)
    }

    pub async fn add_member(&self, channel_id: Uuid, kind: &str, id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(channel_id)
        .bind(kind)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn remove_member(&self, channel_id: Uuid, kind: &str, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM channel_members WHERE channel_id = $1 AND member_kind = $2 AND member_id = $3")
            .bind(channel_id)
            .bind(kind)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn channel_members(&self, channel_id: Uuid) -> Result<Vec<ChannelMember>> {
        Ok(sqlx::query_as::<_, ChannelMember>(
            "SELECT cm.member_kind, cm.member_id,
                    COALESCE(a.name, u.name, u.email, '?') AS name, a.avatar_emoji AS emoji
             FROM channel_members cm
             LEFT JOIN agent_profiles a ON cm.member_kind = 'agent' AND a.id = cm.member_id
             LEFT JOIN users u ON cm.member_kind = 'user' AND u.id = cm.member_id
             WHERE cm.channel_id = $1 ORDER BY cm.member_kind, name",
        )
        .bind(channel_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn is_member(&self, channel_id: Uuid, kind: &str, id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM channel_members WHERE channel_id = $1 AND member_kind = $2 AND member_id = $3)",
        )
        .bind(channel_id)
        .bind(kind)
        .bind(id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn agent_reaches_channel(&self, channel_id: Uuid, agent: Uuid, public_too: bool) -> Result<bool> {
        let mut conn = self.pool().acquire().await?;
        Ok(agent_reaches_channel_with(&mut conn, channel_id, agent, public_too).await?)
    }

    pub async fn channel_messages(&self, channel_id: Uuid, after: Option<i64>, limit: i64) -> Result<Vec<ChannelMessage>> {
        self.channel_messages_page(channel_id, after, None, limit).await
    }

    pub async fn channel_messages_page(
        &self,
        channel_id: Uuid,
        after: Option<i64>,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<ChannelMessage>> {
        if let (None, Some(before)) = (after, before) {
            let mut r = sqlx::query_as::<_, ChannelMessage>(&format!(
                "{MESSAGE_SELECT} WHERE m.channel_id = $1 AND m.reply_to IS NULL AND m.id < $2 ORDER BY m.id DESC LIMIT $3"
            ))
            .bind(channel_id)
            .bind(before)
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
            r.reverse();
            return Ok(r);
        }
        let rows = match after {
            Some(after) => sqlx::query_as::<_, ChannelMessage>(&format!(
                "{MESSAGE_SELECT} WHERE m.channel_id = $1 AND m.reply_to IS NULL AND m.id > $2 ORDER BY m.id LIMIT $3"
            ))
            .bind(channel_id)
            .bind(after)
            .bind(limit)
            .fetch_all(self.pool())
            .await?,
            None => {
                let mut r = sqlx::query_as::<_, ChannelMessage>(&format!(
                    "{MESSAGE_SELECT} WHERE m.channel_id = $1 AND m.reply_to IS NULL ORDER BY m.id DESC LIMIT $2"
                ))
                .bind(channel_id)
                .bind(limit)
                .fetch_all(self.pool())
                .await?;
                r.reverse();
                r
            }
        };
        Ok(rows)
    }

    pub async fn edit_channel_message(&self, channel_id: Uuid, message_id: i64, author: Author, body: &str) -> Result<Option<ChannelMessage>> {
        let (kind, column, id) = match author {
            Author::User(id) => ("user", "author_user_id", id),
            Author::Agent(id) => ("agent", "author_agent_id", id),
        };
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_scalar::<_, i64>(&format!(
            "UPDATE channel_messages SET body = $4, edited_at = now()
             WHERE id = $1 AND channel_id = $2 AND author_kind = $5 AND {column} = $3
             RETURNING id"
        ))
        .bind(message_id)
        .bind(channel_id)
        .bind(id)
        .bind(body)
        .bind(kind)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        let event = match author {
            Author::User(user) => NewEvent::new("channel.edited").actor(Some(user)).payload(json!({ "channel_id": channel_id, "message_id": message_id })),
            Author::Agent(agent) => NewEvent::new("channel.edited").payload(json!({ "channel_id": channel_id, "message_id": message_id, "author_agent_id": agent })),
        };
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        Ok(sqlx::query_as::<_, ChannelMessage>(&format!("{MESSAGE_SELECT} WHERE m.id = $1"))
            .bind(message_id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn delete_channel_message(&self, channel_id: Uuid, message_id: i64, user_id: Uuid) -> Result<DeleteOutcome> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, (Option<i64>, i32)>(
            "SELECT reply_to, reply_count FROM channel_messages
             WHERE id = $1 AND channel_id = $2 AND author_kind = 'user' AND author_user_id = $3
             FOR UPDATE",
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((reply_to, replies)) = row else {
            return Ok(DeleteOutcome::NotFound);
        };
        if replies > 0 {
            return Ok(DeleteOutcome::HasReplies);
        }
        sqlx::query("DELETE FROM channel_messages WHERE id = $1").bind(message_id).execute(&mut *tx).await?;
        let event = NewEvent::new("channel.deleted")
            .actor(Some(user_id))
            .payload(json!({ "channel_id": channel_id, "message_id": message_id, "reply_to": reply_to }));
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        Ok(DeleteOutcome::Deleted)
    }

    pub async fn mark_channel_read(&self, user_id: Uuid, channel_id: Uuid, last_id: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO channel_reads (user_id, channel_id, last_read_id) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, channel_id) DO UPDATE SET last_read_id = GREATEST(channel_reads.last_read_id, $3)",
        )
        .bind(user_id)
        .bind(channel_id)
        .bind(last_id)
        .execute(self.pool())
        .await?;
        sqlx::query(
            "UPDATE world_notifications n SET cleared_at = NOW()
               FROM world_events e
              WHERE n.user_id = $1 AND n.kind = 'mention' AND n.ref = $2::text AND n.cleared_at IS NULL
                AND e.seq = n.event_seq
                AND CASE WHEN e.payload->>'message_id' ~ '^[0-9]{1,18}$' THEN (e.payload->>'message_id')::bigint END <= $3",
        )
        .bind(user_id)
        .bind(channel_id)
        .bind(last_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn channel_thread(&self, channel_id: Uuid, root: i64, limit: i64) -> Result<Vec<ChannelMessage>> {
        self.channel_thread_with_reply(channel_id, root, limit, None).await
    }

    pub async fn channel_thread_with_reply(&self, channel_id: Uuid, root: i64, limit: i64, reply: Option<i64>) -> Result<Vec<ChannelMessage>> {
        let mut rows = sqlx::query_as::<_, ChannelMessage>(&format!(
            "{MESSAGE_SELECT} WHERE m.channel_id = $1 AND m.reply_to = $2
             ORDER BY (m.id = $4) DESC NULLS LAST, m.id DESC LIMIT $3"
        ))
        .bind(channel_id)
        .bind(root)
        .bind(limit)
        .bind(reply)
        .fetch_all(self.pool())
        .await?;
        rows.sort_by_key(|message| message.id);
        if let Some(first) = sqlx::query_as::<_, ChannelMessage>(&format!(
            "{MESSAGE_SELECT} WHERE m.channel_id = $1 AND m.id = $2 AND m.reply_to IS NULL"
        ))
        .bind(channel_id)
        .bind(root)
        .fetch_optional(self.pool())
        .await?
        {
            rows.insert(0, first);
        }
        Ok(rows)
    }

    pub async fn channel_thread_root(&self, channel_id: Uuid, message_id: i64) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, i64>("SELECT COALESCE(reply_to, id) FROM channel_messages WHERE id = $1 AND channel_id = $2")
            .bind(message_id)
            .bind(channel_id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn open_channel_thread(&self, agent_id: Uuid, channel_id: Uuid) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT CASE
                 WHEN kind = 'channel.mention' THEN (payload->>'reply_to')::bigint
                 ELSE (payload->'clarification_channel'->>'root_id')::bigint
               END
             FROM agent_inbox
             WHERE agent_id = $1 AND status = 'processing' AND (
               (kind = 'channel.mention' AND payload->>'channel_id' = $2::text AND payload ? 'reply_to')
               OR (kind = 'message' AND payload->>'from_owner' = 'true'
                 AND payload ? 'clarification_continuation'
                 AND payload->'clarification_channel'->>'channel_id' = $2::text)
             )
             ORDER BY id DESC LIMIT 1",
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    pub async fn post_channel_message(&self, channel: &Channel, author: Author, body: &str, meta: Value) -> Result<ChannelMessage> {
        self.post_channel_reply(channel, author, body, meta, None).await
    }

    pub async fn thread_agents(&self, channel_id: Uuid, root: i64) -> Result<Vec<Uuid>> {
        let last: Option<Uuid> = sqlx::query_scalar(
            "SELECT author_agent_id FROM channel_messages
             WHERE channel_id = $1 AND (id = $2 OR reply_to = $2) AND author_agent_id IS NOT NULL
             ORDER BY id DESC LIMIT 1",
        )
        .bind(channel_id)
        .bind(root)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        if let Some(agent) = last {
            return Ok(vec![agent]);
        }
        let tagged: Option<Value> = sqlx::query_scalar("SELECT mentions->'agents' FROM channel_messages WHERE id = $1 AND channel_id = $2")
            .bind(root)
            .bind(channel_id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        Ok(tagged
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
            .collect())
    }

    pub async fn post_channel_reply(&self, channel: &Channel, author: Author, body: &str, meta: Value, reply_to: Option<i64>) -> Result<ChannelMessage> {
        let reply_to = match reply_to {
            Some(id) => Some(self.channel_thread_root(channel.id, id).await?.ok_or_else(|| anyhow::anyhow!("that message is not in this channel"))?),
            None => None,
        };
        let body = body.trim();
        let attachments = meta.get("attachments").cloned().unwrap_or(Value::Null);
        if body.is_empty() && attachments.as_array().is_none_or(|a| a.is_empty()) {
            bail!("empty message");
        }
        let tokens = mention_tokens(body);
        let agents = self.list_agents_in(channel.org_id).await?;
        let users: Vec<MentionUser> = sqlx::query_as(
            "SELECT u.id, u.name, u.email, CASE WHEN u.mention_wakes_main THEN m.main_agent_id END
             FROM org_members m JOIN users u ON u.id = m.user_id WHERE m.org_id = $1",
        )
        .bind(channel.org_id)
        .fetch_all(self.pool())
        .await?;
        let broadcast = broadcast_token(&tokens);
        let everyone = broadcast.is_some();
        let poster = match &author {
            Author::User(id) => Some(*id),
            Author::Agent(id) => agents.iter().find(|a| a.id == *id).and_then(|a| a.owner_user_id),
        };
        let aliases: std::collections::HashMap<Uuid, Vec<String>> = if tokens.is_empty() {
            std::collections::HashMap::new()
        } else {
            sqlx::query_as::<_, (Uuid, String)>("SELECT agent_id, slug FROM agent_slug_aliases")
                .fetch_all(self.pool())
                .await?
                .into_iter()
                .fold(std::collections::HashMap::new(), |mut map, (agent_id, slug)| {
                    map.entry(agent_id).or_insert_with(Vec::new).push(slug);
                    map
                })
        };
        let mut mentioned_agents: Vec<_> = agents
            .iter()
            .filter(|a| {
                tokens.contains(&handle(&a.slug))
                    || tokens.contains(&handle(&a.name))
                    || aliases.get(&a.id).is_some_and(|slugs| slugs.iter().any(|s| tokens.contains(&handle(s))))
            })
            .filter(|a| a.is_main || a.visibility == "public" || (poster.is_some() && a.owner_user_id == poster))
            .collect();
        let mut mentioned_users: Vec<Uuid> = users
            .iter()
            .filter(|(_, name, email, _)| {
                name.as_deref().is_some_and(|n| tokens.contains(&handle(n)))
                    || email
                        .as_deref()
                        .and_then(|e| e.split('@').next())
                        .is_some_and(|local| tokens.contains(&handle(local)))
            })
            .map(|(id, _, _, _)| *id)
            .collect();
        for (id, _, _, main) in &users {
            let Some(main) = main else { continue };
            if !mentioned_users.contains(id) || Some(*id) == poster || mentioned_agents.iter().any(|a| a.id == *main) {
                continue;
            }
            if let Some(agent) = agents.iter().find(|a| a.id == *main) {
                mentioned_agents.push(agent);
            }
        }
        if let (Some(root), Author::User(_)) = (reply_to, &author) {
            if mentioned_agents.is_empty() && mentioned_users.is_empty() && !everyone {
                for id in self.thread_agents(channel.id, root).await? {
                    if let Some(agent) = agents
                        .iter()
                        .find(|a| a.id == id)
                        .filter(|a| a.is_main || a.visibility == "public" || (poster.is_some() && a.owner_user_id == poster))
                    {
                        mentioned_agents.push(agent);
                    }
                }
            }
        }
        let team = if channel.kind == "team" { self.team_of_channel(channel.id).await? } else { None };
        if let (Some(team), Author::User(_)) = (&team, &author) {
            if mentioned_agents.is_empty() && !everyone {
                if let Some(lead) = team.lead_agent_id.and_then(|lead| agents.iter().find(|a| a.id == lead)) {
                    mentioned_agents.push(lead);
                }
            }
        }
        let mut broadcast_agents: Vec<Uuid> = Vec::new();
        if everyone {
            for m in self.channel_members(channel.id).await? {
                if m.member_kind == "user" && !mentioned_users.contains(&m.member_id) {
                    mentioned_users.push(m.member_id);
                }
                if m.member_kind != "agent" || !matches!(author, Author::User(_)) || broadcast_agents.len() >= BROADCAST_AGENT_LIMIT {
                    continue;
                }
                let Some(agent) = agents.iter().find(|a| a.id == m.member_id) else { continue };
                if !(agent.is_main || agent.visibility == "public" || (poster.is_some() && agent.owner_user_id == poster)) {
                    continue;
                }
                if !broadcast.is_some_and(|b| broadcast_reaches(b, agent)) {
                    continue;
                }
                if !mentioned_agents.iter().any(|a| a.id == agent.id) {
                    broadcast_agents.push(agent.id);
                    mentioned_agents.push(agent);
                }
            }
        }
        let (author_kind, author_user, author_agent) = match author {
            Author::User(id) => ("user", Some(id), None),
            Author::Agent(id) => ("agent", None, Some(id)),
        };
        mentioned_users.retain(|u| Some(*u) != author_user);
        let mentions = json!({
            "agents": mentioned_agents.iter().map(|a| a.id).collect::<Vec<_>>(),
            "users": mentioned_users,
        });
        let mut tx = self.pool().begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO channel_messages (channel_id, author_kind, author_user_id, author_agent_id, body, mentions, meta, reply_to)
             VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, '{}'::jsonb), $8) RETURNING id",
        )
        .bind(channel.id)
        .bind(author_kind)
        .bind(author_user)
        .bind(author_agent)
        .bind(body)
        .bind(&mentions)
        .bind(if meta.is_null() { None } else { Some(&meta) })
        .bind(reply_to)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(u) = author_user {
            sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, 'user', $2) ON CONFLICT DO NOTHING")
                .bind(channel.id)
                .bind(u)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(a) = author_agent {
            sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, 'agent', $2) ON CONFLICT DO NOTHING")
                .bind(channel.id)
                .bind(a)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        let message = sqlx::query_as::<_, ChannelMessage>(&format!("{MESSAGE_SELECT} WHERE m.id = $1"))
            .bind(id)
            .fetch_one(self.pool())
            .await?;
        let author_name = message.author_name.clone().unwrap_or_else(|| "someone".into());
        let spoken = author_user.is_some() && meta.get("spoken").and_then(Value::as_bool) == Some(true);
        let label = if channel.kind == "dm" { "a direct message".to_string() } else { format!("#{}", channel.name) };
        let mut event = NewEvent::new("channel.message").actor(author_user).payload(json!({
            "channel_id": channel.id,
            "channel": label,
            "message_id": id,
            "author": author_name,
            "author_kind": author_kind,
            "author_agent_id": author_agent,
            "body": body,
            "attachments": attachments,
            "mentions_users": mentioned_users.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
            "team_id": team.as_ref().map(|t| t.id),
            "reply_to": reply_to,
            "spoken": spoken,
            "quote": meta.get("quote"),
        }));
        if let Some(a) = author_agent {
            event = event.agent(a);
        }
        let mut tx = self.pool().begin().await?;
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;

        let context = match reply_to {
            Some(root) => self.channel_thread(channel.id, root, CONTEXT_MESSAGES).await?,
            None => self.channel_messages(channel.id, None, CONTEXT_MESSAGES).await?,
        };
        let transcript = context
            .iter()
            .map(|m| format!("[msg:ch{}] {}: {}", m.id, m.author_name.as_deref().unwrap_or("?"), m.body))
            .collect::<Vec<_>>()
            .join("\n");
        let team_quiet = team.is_some() && author_agent.is_some() && !self.team_wakes_left(channel.id).await?;
        let wakes = if team_quiet || meta.get("no_wake").and_then(Value::as_bool) == Some(true) { Vec::new() } else { mentioned_agents };
        for agent in wakes {
            if Some(agent.id) == author_agent {
                continue;
            }
            if author_agent.is_some() {
                let recent: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'channel.mention'
                     AND payload->>'author_kind' = 'agent' AND created_at > NOW() - INTERVAL '10 minutes'",
                )
                .bind(agent.id)
                .fetch_one(self.pool())
                .await?;
                if recent >= AGENT_MENTION_LIMIT {
                    continue;
                }
            }
            self.add_member(channel.id, "agent", agent.id).await?;
            let wake = NewEvent::new("channel.mention").agent(agent.id).actor(author_user).payload(json!({
                "channel_id": channel.id,
                "channel": label,
                "channel_name": channel.name,
                "message_id": id,
                "author": author_name,
                "author_kind": author_kind,
                "author_agent_id": author_agent,
                "body": body,
                "attachments": attachments,
                "links": meta.get("links").cloned().unwrap_or(Value::Null),
                "transcript": transcript,
                "team": team.as_ref().map(|t| t.name.clone()),
                "reply_to": reply_to,
                "broadcast": broadcast_agents.contains(&agent.id).then_some(broadcast).flatten(),
                "spoken": spoken,
                "quote": meta.get("quote"),
            }));
            self.emit(wake, Some((agent.id, "channel.mention"))).await?;
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mentions() {
        assert_eq!(mention_tokens("hey @Bholu and @priya-2, mail me@x.com @here."), vec!["bholu", "here", "priya2"]);
        assert!(mention_tokens("no mentions").is_empty());
    }

    #[test]
    fn spots_broadcasts() {
        assert_eq!(broadcast_token(&mention_tokens("@here standup in 5")), Some("here"));
        assert_eq!(broadcast_token(&mention_tokens("heads up @channel, @here")), Some("channel"));
        assert_eq!(broadcast_token(&mention_tokens("@Channel deploy is green")), Some("channel"));
        assert_eq!(broadcast_token(&mention_tokens("mail here@x.com or #channel")), None);
        assert_eq!(broadcast_token(&mention_tokens("@hereford")), None);
    }

    #[tokio::test]
    async fn broadcasts_wake_member_agents_and_replies_stay_in_threads() -> Result<()> {
        let Ok(url) = std::env::var("CHANNELS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let user = |name: &str| {
            let pool = pool.clone();
            let (email, name) = (format!("{name}-{tag}@test.dev"), format!("{name}{tag}"));
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (email, name, world_role) VALUES ($1, $2, 'write') RETURNING id")
                    .bind(email)
                    .bind(name)
                    .fetch_one(&pool)
                    .await
            }
        };
        let (owner, bob) = (user("owner").await?, user("bob").await?);
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        crate::orgs::join_default_for_tests(&pool, bob, None).await?;
        let agent = |slug: String, owner: Uuid, vis: &str| {
            let pool = pool.clone();
            let vis = vis.to_string();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, visibility) VALUES ($1, $1, 'claude', 'sonnet', $2, $3) RETURNING id",
                )
                .bind(slug)
                .bind(owner)
                .bind(vis)
                .fetch_one(&pool)
                .await
            }
        };
        let ada = agent(format!("ada{tag}"), owner, "private").await?;
        let rook = agent(format!("rook{tag}"), owner, "public").await?;
        let outsider = agent(format!("out{tag}"), owner, "public").await?;
        let hidden = agent(format!("hid{tag}"), bob, "private").await?;
        let channel = store.create_channel(None, &format!("ch{tag}"), None, owner).await?;
        for a in [ada, rook, hidden] {
            store.add_member(channel.id, "agent", a).await?;
        }
        let mentions = |agent: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'channel.mention' ORDER BY id")
                    .bind(agent)
                    .fetch_all(&pool)
                    .await
            }
        };

        let root = store.post_channel_message(&channel, Author::User(owner), "@here standup, what are you on?", Value::Null).await?;
        for a in [ada, rook] {
            let got = mentions(a).await?;
            assert_eq!(got.len(), 1, "member agents hear @here");
            assert_eq!(got[0]["broadcast"], "here");
        }
        assert!(mentions(outsider).await?.is_empty(), "agents outside the channel stay asleep");
        assert!(mentions(hidden).await?.is_empty(), "another owner's private agent is not woken by @here");

        let (away, paused) = (agent(format!("away{tag}"), owner, "public").await?, agent(format!("pause{tag}"), owner, "public").await?);
        sqlx::query("UPDATE agent_profiles SET status = 'away' WHERE id = $1").bind(away).execute(&pool).await?;
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'test' WHERE id = $1").bind(paused).execute(&pool).await?;
        for a in [away, paused] {
            store.add_member(channel.id, "agent", a).await?;
        }
        store.post_channel_message(&channel, Author::User(owner), "@here anyone around?", Value::Null).await?;
        assert!(mentions(away).await?.is_empty(), "@here skips away agents");
        store.post_channel_message(&channel, Author::User(owner), "@channel heads up", Value::Null).await?;
        assert_eq!(mentions(away).await?.len(), 1, "@channel reaches away agents too");
        assert!(mentions(paused).await?.is_empty(), "paused agents are never woken by a broadcast");
        assert_eq!(mentions(ada).await?.len(), 3);

        store.post_channel_message(&channel, Author::Agent(rook), "@channel everyone look at this", Value::Null).await?;
        assert_eq!(mentions(ada).await?.len(), 3, "agents cannot broadcast to other agents");

        let reply = store
            .post_channel_reply(&channel, Author::User(owner), &format!("@ada{tag} details please"), Value::Null, Some(root.id))
            .await?;
        assert_eq!(reply.reply_to, Some(root.id));
        let nested = store.post_channel_reply(&channel, Author::Agent(ada), "here they are", Value::Null, Some(reply.id)).await?;
        assert_eq!(nested.reply_to, Some(root.id), "a reply to a reply joins the same thread");
        let woke = mentions(ada).await?;
        assert_eq!(woke.last().expect("threaded mention")["reply_to"], json!(root.id));

        let timeline = store.channel_messages(channel.id, None, 50).await?;
        assert!(timeline.iter().all(|m| m.reply_to.is_none()), "replies stay out of the channel timeline");
        let shown = timeline.iter().find(|m| m.id == root.id).expect("root");
        assert_eq!(shown.reply_count, 2);
        assert!(shown.last_reply_at.is_some());
        let repliers: Vec<String> = shown
            .reply_authors
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x["kind"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        assert_eq!(repliers, vec!["agent", "user"], "latest replier first");
        let thread = store.channel_thread(channel.id, root.id, THREAD_LIMIT).await?;
        assert_eq!(thread.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, reply.id, nested.id]);
        let recent = store.channel_thread(channel.id, root.id, 1).await?;
        assert_eq!(recent.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, nested.id]);
        let focused = store.channel_thread_with_reply(channel.id, root.id, 1, Some(reply.id)).await?;
        assert_eq!(focused.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, reply.id]);
        let unrelated = store.channel_thread_with_reply(channel.id, root.id, 1, Some(i64::MAX)).await?;
        assert_eq!(unrelated.iter().map(|m| m.id).collect::<Vec<_>>(), vec![root.id, nested.id]);

        let other = store.create_channel(None, &format!("other{tag}"), None, owner).await?;
        assert!(store.post_channel_reply(&other, Author::User(owner), "wrong place", Value::Null, Some(root.id)).await.is_err());

        sqlx::query("DELETE FROM channel_messages WHERE id = $1").bind(nested.id).execute(&pool).await?;
        let count: i32 = sqlx::query_scalar("SELECT reply_count FROM channel_messages WHERE id = $1").bind(root.id).fetch_one(&pool).await?;
        assert_eq!(count, 1);

        let ask = store.post_channel_message(&channel, Author::User(owner), &format!("@rook{tag} what is next?"), Value::Null).await?;
        let rook_before = mentions(rook).await?.len();
        store.post_channel_reply(&channel, Author::User(owner), "also the docs", Value::Null, Some(ask.id)).await?;
        let rook_now = mentions(rook).await?;
        assert_eq!(rook_now.len(), rook_before + 1, "an untagged reply wakes the agent the thread was for");
        assert_eq!(rook_now.last().expect("thread wake")["reply_to"], json!(ask.id));
        store.post_channel_reply(&channel, Author::Agent(ada), "I can take the docs", Value::Null, Some(ask.id)).await?;
        let (ada_before, rook_before) = (mentions(ada).await?.len(), mentions(rook).await?.len());
        store.post_channel_reply(&channel, Author::User(owner), "great, go", Value::Null, Some(ask.id)).await?;
        assert_eq!(mentions(ada).await?.len(), ada_before + 1, "the agent who spoke last in the thread hears the next reply");
        assert_eq!(mentions(rook).await?.len(), rook_before, "only the latest agent in the thread is woken");
        store.post_channel_reply(&channel, Author::User(owner), &format!("@bob{tag} can you check?"), Value::Null, Some(ask.id)).await?;
        assert_eq!(mentions(ada).await?.len(), ada_before + 1, "a reply to a person does not wake the thread's agent");
        Ok(())
    }
}
