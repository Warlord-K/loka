use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::access::Role;
use crate::world::WorldStore;

pub const MAX_PER_GROUP: i64 = 20;
const SNIPPET_CHARS: usize = 160;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AgentHit {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub avatar_emoji: String,
    pub expertise: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MessageHit {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
    pub agent_emoji: Option<String>,
    pub channel_id: Option<Uuid>,
    pub channel_name: Option<String>,
    pub channel_kind: Option<String>,
    pub author: String,
    pub author_kind: String,
    pub author_emoji: Option<String>,
    #[serde(rename = "excerpt")]
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct QuestHit {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub assigned_agent_id: Option<Uuid>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TodoHit {
    pub id: Uuid,
    pub text: String,
    pub status: String,
    pub priority: i16,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ListHit {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub status: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ReportHit {
    pub id: Uuid,
    pub title: String,
    pub folder: String,
    pub visibility: String,
    pub author_agent_id: Option<Uuid>,
    pub excerpt: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ChannelHit {
    pub id: Uuid,
    pub name: String,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ScheduleHit {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub kind: String,
    pub cron_expression: Option<String>,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct FeedHit {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub kind: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TeamHit {
    pub id: Uuid,
    pub name: String,
    pub purpose: String,
    pub lead_agent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RegionHit {
    pub id: Uuid,
    pub name: String,
    pub theme: String,
    pub visibility: String,
    pub mine: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PlaceHit {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub region_id: Option<Uuid>,
    pub region_name: Option<String>,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RuleHit {
    pub id: Uuid,
    pub scope: String,
    #[serde(rename = "excerpt")]
    pub body: String,
    pub enabled: bool,
    pub global: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct KitHit {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub agent_id: Uuid,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DeviceHit {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PersonHit {
    pub id: Uuid,
    pub name: Option<String>,
    pub email: String,
    pub role: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchResults {
    pub agents: Vec<AgentHit>,
    pub messages: Vec<MessageHit>,
    pub reports: Vec<ReportHit>,
    pub quests: Vec<QuestHit>,
    pub todos: Vec<TodoHit>,
    pub lists: Vec<ListHit>,
    pub channels: Vec<ChannelHit>,
    pub schedules: Vec<ScheduleHit>,
    pub feeds: Vec<FeedHit>,
    pub teams: Vec<TeamHit>,
    pub regions: Vec<RegionHit>,
    pub places: Vec<PlaceHit>,
    pub rules: Vec<RuleHit>,
    pub kit: Vec<KitHit>,
    pub devices: Vec<DeviceHit>,
    pub people: Vec<PersonHit>,
}

async fn when<T, F>(on: bool, rows: F) -> Result<Vec<T>>
where
    F: std::future::Future<Output = sqlx::Result<Vec<T>>>,
{
    if on {
        Ok(rows.await?)
    } else {
        Ok(Vec::new())
    }
}

pub fn like_pattern(query: &str) -> String {
    format!("%{}%", query.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

pub fn snippet(body: &str, query: &str, max: usize) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        return flat;
    }
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let needle: Vec<char> = query.trim().to_lowercase().chars().collect();
    let at = if needle.is_empty() || lower.len() != chars.len() {
        0
    } else {
        lower.windows(needle.len()).position(|w| w == needle.as_slice()).unwrap_or(0)
    };
    let start = at.saturating_sub(max / 3).min(chars.len() - max);
    let end = start + max;
    let mut out = String::new();
    if start > 0 {
        out.push_str("...");
    }
    out.push_str(chars[start..end].iter().collect::<String>().trim());
    if end < chars.len() {
        out.push_str("...");
    }
    out
}

pub fn plain_markdown(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '#' | '*' | '`' | '>')).collect()
}

pub fn id_prefix(query: &str) -> Option<String> {
    let bare = query.trim().trim_start_matches("quest:").trim_start_matches('#').to_ascii_lowercase();
    let hex = bare.chars().filter(|c| c.is_ascii_hexdigit()).count();
    (hex >= 6 && bare.len() <= 36 && bare.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then(|| format!("{bare}%"))
}

pub fn trigram_indexable(query: &str) -> bool {
    let mut run = 0;
    for c in query.chars() {
        run = if c.is_alphanumeric() { run + 1 } else { 0 };
        if run >= 3 {
            return true;
        }
    }
    false
}

const AUTHOR: &str = "COALESCE(au.name, u.name, split_part(u.email, '@', 1), 'system')";

impl WorldStore {
    pub async fn search(&self, user: Uuid, role: Role, org: Uuid, query: &str, limit: i64) -> Result<SearchResults> {
        let query = query.trim();
        if query.chars().count() < 2 {
            return Ok(SearchResults::default());
        }
        let limit = limit.clamp(1, MAX_PER_GROUP);
        let pattern = like_pattern(query);
        let text = |column: &str| if trigram_indexable(query) { column.to_string() } else { format!("({column} || '')") };
        let (message_body, report_title, report_body, quest_title, quest_description) =
            (text("m.body"), text("r.title"), text("r.body"), text("t.title"), text("t.description"));
        let admin = role == Role::Admin;
        let read = role >= Role::Read;
        let write = role >= Role::Write;
        let pool = self.pool();

        let agents = async {
            sqlx::query_as::<_, AgentHit>(
                "SELECT a.id, a.name, a.slug, a.avatar_emoji, a.expertise, a.tags
             FROM agent_profiles a
             WHERE a.org_id = $6 AND ($2 OR a.visibility = 'public' OR a.owner_user_id = $1)
               AND (a.name ILIKE $3 OR a.slug ILIKE $3 OR COALESCE(a.expertise, '') ILIKE $3
                    OR EXISTS (SELECT 1 FROM unnest(a.tags) t WHERE t ILIKE $3))
             ORDER BY (a.name ILIKE $4) DESC, a.name
             LIMIT $5",
            )
            .bind(user)
            .bind(admin)
            .bind(&pattern)
            .bind(format!("{}%", pattern.trim_start_matches('%')))
            .bind(limit)
            .bind(org)
            .fetch_all(pool)
            .await
        };

        let messages = async {
            let mut messages = if read {
                sqlx::query_as::<_, MessageHit>(&format!(
                "SELECT * FROM (
                   SELECT m.id::text AS reference, 'agent' AS kind, m.agent_id, t.name AS agent_name, t.avatar_emoji AS agent_emoji,
                          NULL::uuid AS channel_id, NULL::text AS channel_name, NULL::text AS channel_kind,
                          {AUTHOR} AS author, m.author_kind, au.avatar_emoji AS author_emoji,
                          left(m.body, 4000) AS body, m.created_at
                   FROM agent_messages m
                   JOIN agent_profiles t ON t.id = m.agent_id
                   LEFT JOIN agent_profiles au ON au.id = m.author_agent_id
                   LEFT JOIN users u ON u.id = m.author_user_id
                   WHERE $3 AND t.org_id = $7 AND ($2 OR t.visibility = 'public' OR t.owner_user_id = $1) AND {thread}
                     AND COALESCE(m.meta->>'voice_request', 'false') <> 'true'
                     AND (t.owner_user_id = $1 OR (
                       COALESCE(m.meta->>'kind', '') <> 'shell'
                       AND COALESCE(m.meta->>'shell_share', 'false') <> 'true'
                       AND COALESCE(m.meta->>'shell_command', 'false') <> 'true'))
                     AND {message_body} ILIKE $5
                   UNION ALL
                   SELECT 'ch' || m.id::text AS reference, 'channel' AS kind, NULL::uuid AS agent_id, NULL::text AS agent_name, NULL::text AS agent_emoji,
                          c.id AS channel_id, c.name AS channel_name, c.kind AS channel_kind,
                          {AUTHOR} AS author, m.author_kind, au.avatar_emoji AS author_emoji,
                          left(m.body, 4000) AS body, m.created_at
                   FROM channel_messages m
                   JOIN channels c ON c.id = m.channel_id
                   LEFT JOIN agent_profiles au ON au.id = m.author_agent_id
                   LEFT JOIN users u ON u.id = m.author_user_id
                   WHERE {message_body} ILIKE $5 AND c.org_id = $7
                     AND CASE WHEN c.kind <> 'channel'
                              THEN EXISTS (SELECT 1 FROM channel_members cm WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $1)
                              ELSE $4 END
                 ) found ORDER BY created_at DESC LIMIT $6",
                thread = crate::guests::thread_filter("t.id", "$1")
            ))
            .bind(user)
            .bind(admin)
            .bind(write)
            .bind(read)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .fetch_all(pool)
            .await?
            } else {
                Vec::new()
            };
            for m in &mut messages {
                m.body = snippet(&m.body, query, SNIPPET_CHARS);
            }
            Ok::<_, anyhow::Error>(messages)
        };

        let reports = async {
            let mut reports = sqlx::query_as::<_, ReportHit>(&format!(
                "SELECT r.id, r.title, r.folder, r.visibility, r.author_agent_id, r.updated_at,
                        substr(r.body, greatest(1, strpos(lower(r.body), lower($3)) - 300), 1200) AS excerpt
                 FROM world_reports r
                 WHERE r.org_id = $5 AND (r.owner_user_id = $1 OR r.visibility = 'public')
                   AND ({report_title} ILIKE $2 OR {report_body} ILIKE $2 OR r.tags @> ARRAY[lower($3)])
                 ORDER BY (r.title ILIKE $2) DESC, r.updated_at DESC
                 LIMIT $4"
            ))
            .bind(user)
            .bind(&pattern)
            .bind(query)
            .bind(limit)
            .bind(org)
            .fetch_all(pool)
            .await?;
            for r in &mut reports {
                r.excerpt = snippet(&plain_markdown(&r.excerpt), query, SNIPPET_CHARS);
            }
            Ok::<_, anyhow::Error>(reports)
        };

        let quest_id = id_prefix(query);
        let quests = async {
            if read {
                sqlx::query_as::<_, QuestHit>(&format!(
                "SELECT t.id, t.title, t.status, t.assigned_agent_id, t.updated_at
                 FROM agent_tasks t
                 LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
                 WHERE t.org_id = $6 AND (t.assigned_agent_id IS NOT NULL
                        OR (COALESCE(t.source, '') <> 'chat' AND (t.user_id = $1 OR t.assignee_user_id = $1)))
                   AND CASE WHEN t.assigned_agent_id IS NULL THEN $3
                            ELSE ($2 OR a.visibility = 'public' OR a.owner_user_id = $1) END
                   AND ({quest_title} ILIKE $4 OR {quest_description} ILIKE $4 OR t.id::text LIKE $7)
                 ORDER BY COALESCE(t.id::text LIKE $7, false) DESC, t.updated_at DESC NULLS LAST
                 LIMIT $5"
                ))
            .bind(user)
            .bind(admin)
            .bind(write)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .bind(&quest_id)
            .fetch_all(pool)
            .await
            } else {
                Ok(Vec::new())
            }
        };

        let todos = sqlx::query_as::<_, TodoHit>(
            "SELECT id, text, status, priority, added_at FROM world_todos
             WHERE owner_user_id = $1 AND text ILIKE $2
             ORDER BY (status = 'open') DESC, added_at DESC
             LIMIT $3",
        )
        .bind(user)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(pool);

        let lists = sqlx::query_as::<_, ListHit>(
            "SELECT id, kind, name, status, updated_at FROM world_items
             WHERE kind IN ('list', 'timer') AND owner_user_id = $1 AND org_id = $4 AND status <> 'archived'
               AND (name ILIKE $2 OR COALESCE(data->>'items', '') ILIKE $2)
             ORDER BY (kind = 'list') DESC, updated_at DESC
             LIMIT $3",
        )
        .bind(user)
        .bind(&pattern)
        .bind(limit)
        .bind(org)
        .fetch_all(pool);

        let channels = async {
            if read {
                sqlx::query_as::<_, ChannelHit>(
                "SELECT id, name, topic FROM channels
                 WHERE kind = 'channel' AND org_id = $3 AND (name ILIKE $1 OR COALESCE(topic, '') ILIKE $1)
                 ORDER BY name LIMIT $2",
            )
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .fetch_all(pool)
            .await
            } else {
                Ok(Vec::new())
            }
        };

        let manage = write || admin;
        let schedules = when(
            manage,
            sqlx::query_as::<_, ScheduleHit>(
                "SELECT s.id, s.agent_id, s.name, s.kind, s.cron_expression, s.enabled, s.next_run_at
                 FROM schedules s JOIN agent_profiles a ON a.id = s.agent_id
                 WHERE a.org_id = $4 AND ($5 OR a.owner_user_id = $1)
                   AND (s.name ILIKE $2 OR s.prompt ILIKE $2)
                 ORDER BY s.enabled DESC, s.name
                 LIMIT $3",
            )
            .bind(user)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .bind(admin)
            .fetch_all(pool),
        );

        let feeds = when(
            manage,
            sqlx::query_as::<_, FeedHit>(
                "SELECT f.id, f.agent_id, f.name, f.kind, f.paused
                 FROM agent_feeds f JOIN agent_profiles a ON a.id = f.agent_id
                 WHERE a.org_id = $4 AND ($5 OR a.owner_user_id = $1)
                   AND (f.name ILIKE $2 OR f.url ILIKE $2)
                 ORDER BY f.paused, f.name
                 LIMIT $3",
            )
            .bind(user)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .bind(admin)
            .fetch_all(pool),
        );

        let teams = when(
            read,
            sqlx::query_as::<_, TeamHit>(
                "SELECT t.id, t.name, t.purpose, t.lead_agent_id
                 FROM agent_teams t JOIN channels c ON c.id = t.channel_id
                 WHERE c.org_id = $4
                   AND (t.owner_user_id = $1 OR EXISTS (
                     SELECT 1 FROM agent_team_members m WHERE m.team_id = t.id AND m.member_kind = 'user' AND m.member_id = $1))
                   AND (t.name ILIKE $2 OR t.slug ILIKE $2 OR t.purpose ILIKE $2)
                 ORDER BY lower(t.name)
                 LIMIT $3",
            )
            .bind(user)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .fetch_all(pool),
        );

        let regions = sqlx::query_as::<_, RegionHit>(
            "SELECT r.id, r.name, r.theme, r.visibility, r.owner_user_id = $1 AS mine
             FROM world_regions r
             WHERE r.status = 'active' AND r.org_id = $4 AND (r.owner_user_id = $1 OR r.visibility = 'public' OR $5)
               AND (r.name ILIKE $2 OR r.slug ILIKE $2)
             ORDER BY (r.owner_user_id = $1) DESC, r.name
             LIMIT $3",
        )
        .bind(user)
        .bind(&pattern)
        .bind(limit)
        .bind(org)
        .bind(admin)
        .fetch_all(pool);

        let places = sqlx::query_as::<_, PlaceHit>(
            "SELECT i.id, i.kind, i.name, i.region_id, r.name AS region_name, i.data
             FROM world_items i
             LEFT JOIN world_regions r ON r.id = i.region_id
             WHERE i.kind IN ('building', 'npc') AND i.status <> 'archived' AND i.org_id = $4
               AND (i.region_id IS NULL OR (r.status = 'active' AND r.org_id = $4
                    AND (r.owner_user_id = $1 OR r.visibility = 'public' OR $5)))
               AND i.name ILIKE $2
             ORDER BY (i.region_id IS NULL) DESC, i.name
             LIMIT $3",
        )
        .bind(user)
        .bind(&pattern)
        .bind(limit)
        .bind(org)
        .bind(admin)
        .fetch_all(pool);

        let rules = async {
            let mut rules = when(
                read,
                sqlx::query_as::<_, RuleHit>(
                    "SELECT id, scope, left(body, 2000) AS body, enabled, user_id IS NULL AS global
                     FROM world_rules
                     WHERE ((user_id IS NULL AND org_id = $4) OR (user_id = $1 AND (org_id IS NULL OR org_id = $4)))
                       AND body ILIKE $2
                     ORDER BY position, created_at
                     LIMIT $3",
                )
                .bind(user)
                .bind(&pattern)
                .bind(limit)
                .bind(org)
                .fetch_all(pool),
            )
            .await?;
            for r in &mut rules {
                r.body = snippet(&r.body, query, SNIPPET_CHARS);
            }
            Ok::<_, anyhow::Error>(rules)
        };

        let kit = when(
            manage,
            sqlx::query_as::<_, KitHit>(
                "SELECT * FROM (
                   SELECT s.id, 'skill' AS kind, s.name, left(s.description, 300) AS description,
                          (SELECT p.id FROM agent_profiles p
                           WHERE p.org_id = $4 AND ($5 OR p.owner_user_id = $1)
                           ORDER BY EXISTS (SELECT 1 FROM agent_skills x WHERE x.skill_id = s.id AND x.agent_id = p.id) DESC,
                                    (p.owner_user_id = $1) DESC,
                                    EXISTS (SELECT 1 FROM org_members om WHERE om.user_id = $1 AND om.org_id = $4 AND om.main_agent_id = p.id) DESC,
                                    p.created_at
                           LIMIT 1) AS agent_id
                   FROM world_skills s
                   WHERE s.org_id = $4 AND ($5 OR s.scope = 'global' OR s.created_by = $1
                     OR EXISTS (SELECT 1 FROM agent_skills a JOIN agent_profiles p ON p.id = a.agent_id
                                WHERE a.skill_id = s.id AND p.org_id = $4
                                  AND (p.owner_user_id = $1 OR (p.visibility = 'public' AND s.name NOT LIKE '%-notes'))))
                     AND (s.name ILIKE $2 OR s.description ILIKE $2)
                   UNION ALL
                   SELECT m.id, 'mcp' AS kind, m.name, '' AS description,
                          (SELECT p.id FROM agent_profiles p
                           WHERE p.org_id = $4 AND ($5 OR p.owner_user_id = $1)
                           ORDER BY EXISTS (SELECT 1 FROM agent_mcp_servers x WHERE x.server_id = m.id AND x.agent_id = p.id) DESC,
                                    (p.owner_user_id = $1) DESC,
                                    EXISTS (SELECT 1 FROM org_members om WHERE om.user_id = $1 AND om.org_id = $4 AND om.main_agent_id = p.id) DESC,
                                    p.created_at
                           LIMIT 1) AS agent_id
                   FROM world_mcp_servers m
                   WHERE m.org_id = $4 AND m.name ILIKE $2
                 ) found WHERE agent_id IS NOT NULL ORDER BY name LIMIT $3",
            )
            .bind(user)
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .bind(admin)
            .fetch_all(pool),
        );

        let devices = when(
            write,
            sqlx::query_as::<_, DeviceHit>(
                "SELECT id, name, platform,
                        CASE WHEN status = 'online' AND last_seen_at < NOW() - INTERVAL '75 seconds' THEN 'offline' ELSE status END AS status
                 FROM world_runners
                 WHERE owner_user_id = $1 AND status <> 'revoked' AND name ILIKE $2
                 ORDER BY created_at
                 LIMIT $3",
            )
            .bind(user)
            .bind(&pattern)
            .bind(limit)
            .fetch_all(pool),
        );

        let people = when(
            read,
            sqlx::query_as::<_, PersonHit>(
                "SELECT u.id, u.name, u.email, m.role
                 FROM org_members m JOIN users u ON u.id = m.user_id
                 WHERE m.org_id = $3 AND (u.name ILIKE $1 OR u.email ILIKE $1)
                 ORDER BY u.name NULLS LAST, u.email
                 LIMIT $2",
            )
            .bind(&pattern)
            .bind(limit)
            .bind(org)
            .fetch_all(pool),
        );

        let ((agents, messages, reports, quests, todos, lists, channels), (schedules, feeds, teams, regions, places, rules, kit, devices, people)) = futures_util::try_join!(
            async {
                futures_util::try_join!(
                    async { agents.await.map_err(anyhow::Error::from) },
                    messages,
                    reports,
                    async { quests.await.map_err(anyhow::Error::from) },
                    async { todos.await.map_err(anyhow::Error::from) },
                    async { lists.await.map_err(anyhow::Error::from) },
                    async { channels.await.map_err(anyhow::Error::from) },
                )
            },
            async {
                futures_util::try_join!(
                    schedules,
                    feeds,
                    teams,
                    async { regions.await.map_err(anyhow::Error::from) },
                    async { places.await.map_err(anyhow::Error::from) },
                    rules,
                    kit,
                    devices,
                    people,
                )
            },
        )?;
        Ok(SearchResults { agents, messages, reports, quests, todos, lists, channels, schedules, feeds, teams, regions, places, rules, kit, devices, people })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_centres_on_match() {
        let body = format!("{} needle here {}", "a ".repeat(200), "b ".repeat(200));
        let out = snippet(&body, "NEEDLE", 60);
        assert!(out.contains("needle here"));
        assert!(out.starts_with("...") && out.ends_with("..."));
        assert_eq!(snippet("short text", "x", 60), "short text");
    }

    #[test]
    fn short_queries_skip_trigram_indexes() {
        assert!(trigram_indexable("deploy"));
        assert!(trigram_indexable("a bcd"));
        assert!(!trigram_indexable("ok"));
        assert!(!trigram_indexable("a b c"));
        assert!(!trigram_indexable("x-1"));
        assert_eq!(plain_markdown("# Title **bold** `code` > quote"), " Title bold code  quote");
    }

    #[test]
    fn quest_ids_match_by_prefix() {
        assert_eq!(id_prefix("quest:3F2a9c"), Some("3f2a9c%".to_string()));
        assert_eq!(id_prefix("3f2a9c1d-77"), Some("3f2a9c1d-77%".to_string()));
        assert_eq!(id_prefix("deadbe"), Some("deadbe%".to_string()));
        assert_eq!(id_prefix("abc"), None);
        assert_eq!(id_prefix("fix the build"), None);
    }

    #[test]
    fn escapes_like() {
        assert_eq!(like_pattern(" 50%_a\\ "), "%50\\%\\_a\\\\%");
    }

    #[tokio::test]
    async fn search_respects_access() -> Result<()> {
        let Ok(url) = std::env::var("SEARCH_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let word = format!("qz{tag}");
        let user = |name: &str| {
            let pool = pool.clone();
            let email = format!("{name}-{tag}@search.test");
            let label = format!("{name} qz{tag}");
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO users (name, email) VALUES ($1, $2) RETURNING id")
                    .bind(label)
                    .bind(email)
                    .fetch_one(&pool)
                    .await
            }
        };
        let alice = user("alice").await?;
        let bob = user("bob").await?;
        let agent = |owner: Uuid, visibility: &str, name: String| {
            let pool = pool.clone();
            let visibility = visibility.to_string();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility)
                     VALUES ($1, $1, '🦊', 'claude', 'sonnet', $2, $3) RETURNING id",
                )
                .bind(name)
                .bind(owner)
                .bind(visibility)
                .fetch_one(&pool)
                .await
            }
        };
        let private = agent(alice, "private", format!("{word}-secret")).await?;
        let public = agent(alice, "public", format!("{word}-open")).await?;
        for a in [private, public] {
            sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body) VALUES ($1, 'agent', $1, $2)")
                .bind(a)
                .bind(format!("thread note {word} from {a}"))
                .execute(&pool)
                .await?;
        }
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, $3)")
            .bind(public)
            .bind(bob)
            .bind(format!("bob asks about {word}"))
            .execute(&pool)
            .await?;
        let dm: Uuid = sqlx::query_scalar("INSERT INTO channels (name, kind) VALUES ($1, 'dm') RETURNING id")
            .bind(format!("dm-{tag}"))
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, 'user', $2)")
            .bind(dm)
            .bind(alice)
            .execute(&pool)
            .await?;
        let room: Uuid = sqlx::query_scalar("INSERT INTO channels (name, kind, topic) VALUES ($1, 'channel', $2) RETURNING id")
            .bind(format!("room-{tag}"))
            .bind(format!("about {word}"))
            .fetch_one(&pool)
            .await?;
        for c in [dm, room] {
            sqlx::query("INSERT INTO channel_messages (channel_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, $3)")
                .bind(c)
                .bind(alice)
                .bind(format!("channel post {word}"))
                .execute(&pool)
                .await?;
        }
        for a in [Some(private), Some(public), None] {
            sqlx::query("INSERT INTO agent_tasks (user_id, title, assigned_agent_id) VALUES ($1, $2, $3)")
                .bind(alice)
                .bind(format!("quest {word}"))
                .bind(a)
                .execute(&pool)
                .await?;
        }
        sqlx::query("INSERT INTO world_todos (owner_user_id, text) VALUES ($1, $2)")
            .bind(alice)
            .bind(format!("todo {word}"))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_items (kind, name, owner_user_id, data) VALUES ('list', $2, $1, '{\"items\": []}')")
            .bind(alice)
            .bind(format!("list {word}"))
            .execute(&pool)
            .await?;

        for (slug, visibility, title, body) in [("secret", "private", "Private notes".to_string(), format!("deep inside {word} body")), ("open", "public", format!("Public {word}"), "plain".to_string())] {
            sqlx::query("INSERT INTO world_reports (owner_user_id, title, slug, body, visibility, org_id) VALUES ($1, $2, $3, $4, $5, $6)")
                .bind(alice)
                .bind(title)
                .bind(format!("{slug}-{tag}"))
                .bind(body)
                .bind(visibility)
                .bind(crate::orgs::DEFAULT_ORG)
                .execute(&pool)
                .await?;
        }

        let org = crate::orgs::DEFAULT_ORG;
        crate::orgs::join_default_for_tests(&pool, alice, None).await?;
        crate::orgs::join_default_for_tests(&pool, bob, None).await?;
        sqlx::query("INSERT INTO world_items (kind, name, owner_user_id, data) VALUES ('timer', $2, $1, '{}')")
            .bind(alice)
            .bind(format!("timer {word}"))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO schedules (user_id, agent_id, name, prompt, cron_expression) VALUES ($1, $2, $3, 'check', '0 9 * * *')")
            .bind(alice)
            .bind(private)
            .bind(format!("routine {word}"))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_feeds (agent_id, owner_user_id, kind, name, url) VALUES ($1, $2, 'rss', $3, 'https://example.com/feed')")
            .bind(public)
            .bind(alice)
            .bind(format!("feed {word}"))
            .execute(&pool)
            .await?;
        let team_room: Uuid = sqlx::query_scalar("INSERT INTO channels (name, kind, org_id) VALUES ($1, 'team', $2) RETURNING id")
            .bind(format!("team-{tag}"))
            .bind(org)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_teams (owner_user_id, name, slug, channel_id) VALUES ($1, $2, $3, $4)")
            .bind(alice)
            .bind(format!("team {word}"))
            .bind(format!("team-{tag}"))
            .bind(team_room)
            .execute(&pool)
            .await?;
        let mut region_ids = Vec::new();
        for visibility in ["private", "public"] {
            region_ids.push(
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO world_regions (owner_user_id, name, slug, visibility, org_id) VALUES ($1, $2, $3, $4, $5) RETURNING id",
                )
                .bind(alice)
                .bind(format!("{visibility} land {word}"))
                .bind(format!("{visibility}-{tag}"))
                .bind(visibility)
                .bind(org)
                .fetch_one(&pool)
                .await?,
            );
        }
        for (kind, region) in [("building", None), ("npc", Some(region_ids[0])), ("npc", Some(region_ids[1]))] {
            sqlx::query("INSERT INTO world_items (kind, name, owner_user_id, data, region_id, org_id) VALUES ($1, $2, $3, '{}', $4, $5)")
                .bind(kind)
                .bind(format!("{kind} {word}"))
                .bind(alice)
                .bind(region)
                .bind(org)
                .execute(&pool)
                .await?;
        }
        sqlx::query("INSERT INTO world_rules (user_id, body, org_id) VALUES ($1, $2, $3), (NULL, $4, $3)")
            .bind(alice)
            .bind(format!("always {word} carefully"))
            .bind(org)
            .bind(format!("everyone {word} politely"))
            .execute(&pool)
            .await?;
        let skill: Uuid = sqlx::query_scalar("INSERT INTO world_skills (name, body, created_by, org_id) VALUES ($1, 'body', $2, $3) RETURNING id")
            .bind(format!("skill-{word}"))
            .bind(alice)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_skills (agent_id, skill_id) VALUES ($1, $2)").bind(public).bind(skill).execute(&pool).await?;
        sqlx::query("INSERT INTO world_mcp_servers (name, definition, created_by, org_id) VALUES ($1, '{\"url\": \"https://mcp.example.com\"}', $2, $3)")
            .bind(format!("mcp-{word}"))
            .bind(alice)
            .bind(org)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_runners (owner_user_id, name, token_hash) VALUES ($1, $2, $3)")
            .bind(alice)
            .bind(format!("mac {word}"))
            .bind([Uuid::new_v4().into_bytes(), Uuid::new_v4().into_bytes()].concat())
            .execute(&pool)
            .await?;

        let mine = store.search(alice, Role::Write, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert_eq!(mine.agents.len(), 2);
        assert_eq!(mine.messages.iter().filter(|m| m.kind == "agent").count(), 2);
        assert_eq!(mine.messages.iter().filter(|m| m.kind == "channel").count(), 2);
        assert!(mine.messages.iter().any(|m| m.reference.starts_with("ch")));
        assert_eq!(mine.quests.len(), 3);
        assert_eq!(mine.todos.len(), 1);
        assert_eq!(mine.lists.iter().filter(|l| l.kind == "list").count(), 1);
        assert_eq!(mine.channels.len(), 1);
        assert_eq!(mine.reports.len(), 2);
        assert!(mine.reports[0].title.starts_with("Public"));
        assert!(mine.reports[1].excerpt.contains(&word));

        assert_eq!(mine.lists.iter().filter(|l| l.kind == "timer").count(), 1);
        assert_eq!(mine.schedules.len(), 1);
        assert_eq!(mine.feeds.len(), 1);
        assert_eq!(mine.teams.len(), 1);
        assert_eq!(mine.regions.len(), 2);
        assert_eq!(mine.places.len(), 3);
        assert_eq!(mine.rules.len(), 2);
        assert!(mine.rules.iter().any(|r| r.global));
        assert_eq!(mine.kit.len(), 2);
        assert_eq!(mine.kit.iter().find(|k| k.kind == "skill").map(|k| k.agent_id), Some(public));
        assert_eq!(mine.devices.len(), 1);
        assert_eq!(mine.people.len(), 2);

        let private_quest: Uuid = sqlx::query_scalar("SELECT id FROM agent_tasks WHERE title = $1 AND assigned_agent_id = $2")
            .bind(format!("quest {word}"))
            .bind(private)
            .fetch_one(&pool)
            .await?;
        let by_id = format!("quest:{}", &private_quest.to_string()[..8]);
        let found = store.search(alice, Role::Write, crate::orgs::DEFAULT_ORG, &by_id, 20).await?;
        assert_eq!(found.quests.first().map(|q| q.id), Some(private_quest));
        let hidden = store.search(bob, Role::Write, crate::orgs::DEFAULT_ORG, &by_id, 20).await?;
        assert!(hidden.quests.iter().all(|q| q.id != private_quest));

        let theirs = store.search(bob, Role::Write, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert_eq!(theirs.agents.iter().map(|a| a.id).collect::<Vec<_>>(), vec![public]);
        assert!(theirs.messages.iter().all(|m| m.agent_id != Some(private) && m.channel_id != Some(dm)));
        assert!(theirs.messages.iter().filter(|m| m.kind == "agent").all(|m| m.body.contains("bob asks")), "bob sees only his own thread with the public agent");
        assert_eq!(theirs.messages.len(), 2);
        assert!(theirs.quests.iter().all(|q| q.assigned_agent_id != Some(private)));
        assert_eq!(theirs.quests.len(), 1);
        assert!(theirs.todos.is_empty() && theirs.lists.is_empty());
        assert!(theirs.schedules.is_empty() && theirs.feeds.is_empty() && theirs.teams.is_empty());
        assert_eq!(theirs.regions.iter().map(|r| r.visibility.as_str()).collect::<Vec<_>>(), vec!["public"]);
        assert_eq!(theirs.places.len(), 2);
        assert!(theirs.places.iter().all(|p| p.region_id != Some(region_ids[0])));
        assert_eq!(theirs.rules.iter().map(|r| r.global).collect::<Vec<_>>(), vec![true]);
        assert!(theirs.kit.is_empty() && theirs.devices.is_empty());
        assert_eq!(theirs.people.len(), 2);
        assert_eq!(theirs.reports.iter().map(|r| r.visibility.as_str()).collect::<Vec<_>>(), vec!["public"]);
        let short = store.search(alice, Role::Write, crate::orgs::DEFAULT_ORG, &word[..2], 20).await?;
        assert!(short.reports.iter().any(|r| r.visibility == "private"));

        let reader = store.search(bob, Role::Read, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert!(reader.messages.iter().all(|m| m.kind == "channel"));
        assert_eq!(reader.quests.len(), 1);
        let viewer = store.search(bob, Role::View, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert!(viewer.messages.is_empty() && viewer.quests.is_empty() && viewer.channels.is_empty());
        assert!(viewer.people.is_empty() && viewer.rules.is_empty() && viewer.teams.is_empty() && viewer.devices.is_empty());
        let alice_viewer = store.search(alice, Role::View, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert!(alice_viewer.schedules.is_empty() && alice_viewer.feeds.is_empty() && alice_viewer.kit.is_empty() && alice_viewer.devices.is_empty());
        assert_eq!(alice_viewer.regions.len(), 2);
        let admin = store.search(bob, Role::Admin, crate::orgs::DEFAULT_ORG, &word, 20).await?;
        assert_eq!(admin.reports.len(), 1);
        assert_eq!(admin.agents.len(), 2);
        assert_eq!(admin.messages.len(), 2, "admins do not read other people's threads either");
        assert_eq!(admin.quests.len(), 2);
        assert_eq!(admin.regions.len(), 2);
        assert_eq!(admin.places.len(), 3);
        assert!(admin.teams.is_empty() && admin.devices.is_empty());
        let elsewhere = store.search(alice, Role::Write, Uuid::new_v4(), &word, 20).await?;
        assert!(elsewhere.schedules.is_empty() && elsewhere.regions.is_empty() && elsewhere.places.is_empty() && elsewhere.rules.is_empty());
        assert!(elsewhere.kit.is_empty() && elsewhere.people.is_empty() && elsewhere.teams.is_empty());

        sqlx::query("DELETE FROM agent_tasks WHERE title = $1").bind(format!("quest {word}")).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE id = $1").bind(skill).execute(&pool).await?;
        sqlx::query("DELETE FROM world_mcp_servers WHERE name = $1").bind(format!("mcp-{word}")).execute(&pool).await?;
        sqlx::query("DELETE FROM world_rules WHERE body LIKE $1").bind(format!("%{word}%")).execute(&pool).await?;
        sqlx::query("DELETE FROM world_items WHERE name LIKE $1").bind(format!("%{word}")).execute(&pool).await?;
        sqlx::query("DELETE FROM world_regions WHERE id = ANY($1)").bind(&region_ids).execute(&pool).await?;
        sqlx::query("DELETE FROM channels WHERE id = ANY($1)").bind(vec![dm, room, team_room]).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![private, public]).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![alice, bob]).execute(&pool).await?;
        Ok(())
    }
}
