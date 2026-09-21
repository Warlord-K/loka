use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::items::{NewItem, WorldItem};
use crate::world::{AgentSession, NewEvent, NewMessage, WorldAgent, WorldEvent, WorldStore};

pub const THEMES: &[&str] = &["meadow", "forest", "docks", "academy", "snow", "desert", "mountain", "swamp", "city", "island"];
pub const OPENS_KINDS: &[&str] = &["channel", "quest", "list", "url", "challenge", "region", "surface"];
pub const SURFACES: &[&str] = &["timers", "schedules", "sessions", "workshop", "terminal", "devices", "inbox", "usage", "feeds", "reports"];
pub const WEATHERS: &[&str] = &["clear", "sunny", "cloudy", "rain", "storm", "snow", "fog", "wind", "aurora"];
pub const TIMES_OF_DAY: &[&str] = &["auto", "dawn", "day", "dusk", "night"];
pub const MAX_REGIONS_PER_OWNER: i64 = 24;
pub const MAX_NPC_LINES_PER_DAY: i64 = 200;
const NPC_HISTORY: i64 = 6;
pub const CARTOGRAPHER_TAG: &str = "cartographer";

pub fn presets() -> Value {
    json!([{
        "id": CARTOGRAPHER_TAG,
        "name": "Atlas",
        "avatar_emoji": "\u{1F5FA}\u{FE0F}",
        "persona": "A curious, unhurried cartographer owl who maps everything their person does into Loka. Builds new regions with care, fills them with characters worth meeting, and runs the Academy, setting short, fair challenges pitched just above their person's level.",
        "tags": [CARTOGRAPHER_TAG, "worldbuilding", "teaching"],
        "expertise": "Grows Loka and sets Academy challenges and adventures.",
        "look": { "species": "owl", "archetype": "cartographer", "palette": ["indigo", "parchment"], "accessories": ["rolled map", "compass"] },
        "gear": { "quest": "a brass compass and a satchel of blank maps" },
        "visibility": "private"
    }])
}
const PRIVATE_PREFIXES: &[&str] = &["region.", "npc.", "challenge.", "adventure.", "academy.", "music.", "report.", "team."];

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Region {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
    pub theme: String,
    pub seed: i64,
    pub visibility: String,
    pub status: String,
    pub created_by_agent: Option<Uuid>,
    pub data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegionDetail {
    pub region: Region,
    pub items: Vec<WorldItem>,
}

#[derive(Debug, Clone, Default)]
pub struct RegionPatch {
    pub name: Option<String>,
    pub theme: Option<String>,
    pub visibility: Option<String>,
    pub data: Option<Value>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct NpcLine {
    pub session_id: Uuid,
    pub said: Option<String>,
    pub reply: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NpcTalk {
    pub npc_id: Uuid,
    pub session_id: Uuid,
    pub run_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub status: &'static str,
}

pub fn staffed_by(npc: &WorldItem) -> Option<Uuid> {
    npc.data.get("agent_id").and_then(Value::as_str).and_then(|s| s.parse().ok())
}

#[derive(Debug)]
pub enum TalkError {
    NotFound,
    Busy,
    Blocked(String),
    Invalid(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for TalkError {
    fn from(e: anyhow::Error) -> Self {
        TalkError::Other(e)
    }
}

impl From<sqlx::Error> for TalkError {
    fn from(e: sqlx::Error) -> Self {
        TalkError::Other(e.into())
    }
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(48).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "region".into()
    } else {
        out
    }
}

pub fn hidden_from(event: &WorldEvent, viewer: Uuid) -> bool {
    let owner = event.payload.get("owner_user_id").and_then(Value::as_str);
    let foreign = owner != Some(viewer.to_string().as_str());
    if !foreign {
        return false;
    }
    let public = event.payload.get("visibility").and_then(Value::as_str) == Some("public");
    if PRIVATE_PREFIXES.iter().any(|p| event.event_type.starts_with(p)) {
        return !((event.event_type.starts_with("region.") || event.event_type.starts_with("report.")) && public);
    }
    event.event_type.starts_with("item.") && event.payload.get("private").and_then(Value::as_bool) == Some(true)
}

fn pick(value: Option<&str>, allowed: &[&str], what: &str) -> Result<Option<String>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => {
            let v = v.to_lowercase();
            if allowed.contains(&v.as_str()) {
                Ok(Some(v))
            } else {
                bail!("{what} must be one of {}", allowed.join(", "))
            }
        }
    }
}

pub fn clean_ambient(current: &Value, args: &Value) -> Result<Value> {
    let mut ambient = current.as_object().cloned().unwrap_or_default();
    if let Some(w) = pick(args.get("weather").and_then(Value::as_str), WEATHERS, "weather")? {
        ambient.insert("weather".into(), json!(w));
    }
    if let Some(t) = pick(args.get("time_of_day").and_then(Value::as_str), TIMES_OF_DAY, "time_of_day")? {
        ambient.insert("time_of_day".into(), json!(t));
    }
    if let Some(list) = args.get("critters").and_then(Value::as_array) {
        let critters: Vec<Value> = list
            .iter()
            .filter_map(|c| {
                let species = c.get("species").and_then(Value::as_str).or_else(|| c.as_str())?.trim();
                if species.is_empty() || species.len() > 40 {
                    return None;
                }
                let count = c.get("count").and_then(Value::as_i64).unwrap_or(1).clamp(1, 8);
                Some(json!({ "species": species.to_lowercase(), "count": count }))
            })
            .take(8)
            .collect();
        ambient.insert("critters".into(), json!(critters));
    }
    if let Some(music) = args.get("music").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()) {
        ambient.insert("music".into(), json!(music.chars().take(40).collect::<String>()));
    }
    Ok(Value::Object(ambient))
}

pub fn parse_dialogue(value: Option<&Value>) -> Result<Option<Value>> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(None) };
    let nodes = value.as_array().ok_or_else(|| anyhow!("dialogue must be a list of lines"))?;
    if nodes.len() > 40 {
        bail!("dialogue can have at most 40 lines");
    }
    let mut out = Vec::new();
    for (n, node) in nodes.iter().enumerate() {
        let text = node.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
        let text = text.ok_or_else(|| anyhow!("dialogue line {} needs text", n + 1))?;
        let id = node.get("id").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| format!("l{}", n + 1));
        let options: Vec<Value> = node
            .get("options")
            .and_then(Value::as_array)
            .map(|opts| {
                opts.iter()
                    .filter_map(|o| {
                        let label = o.get("label").and_then(Value::as_str)?.trim();
                        (!label.is_empty()).then(|| json!({ "label": label.chars().take(80).collect::<String>(), "next": o.get("next").and_then(Value::as_str) }))
                    })
                    .take(4)
                    .collect()
            })
            .unwrap_or_default();
        out.push(json!({ "id": id, "text": text.chars().take(600).collect::<String>(), "options": options }));
    }
    Ok(Some(json!(out)))
}

impl WorldStore {
    pub async fn create_region(
        &self,
        owner: Uuid,
        name: &str,
        theme: Option<&str>,
        visibility: Option<&str>,
        description: Option<&str>,
        created_by_agent: Option<Uuid>,
    ) -> Result<Region> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            bail!("a region needs a name of at most 80 characters");
        }
        let theme = pick(theme, THEMES, "theme")?.unwrap_or_else(|| "meadow".into());
        let visibility = pick(visibility, &["private", "public"], "visibility")?.unwrap_or_else(|| "private".into());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_regions WHERE owner_user_id = $1 AND status = 'active'")
            .bind(owner)
            .fetch_one(self.pool())
            .await?;
        if count >= MAX_REGIONS_PER_OWNER {
            bail!("your Loka already has {MAX_REGIONS_PER_OWNER} regions; archive one first");
        }
        let base = slugify(name);
        let mut slug = base.clone();
        let mut n = 2;
        while sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM world_regions WHERE owner_user_id = $1 AND slug = $2)")
            .bind(owner)
            .bind(&slug)
            .fetch_one(self.pool())
            .await?
        {
            slug = format!("{base}-{n}");
            n += 1;
        }
        let mut data = Map::new();
        if let Some(d) = description.map(str::trim).filter(|d| !d.is_empty()) {
            data.insert("description".into(), json!(d.chars().take(600).collect::<String>()));
        }
        let region = sqlx::query_as::<_, Region>(
            "INSERT INTO world_regions (owner_user_id, name, slug, theme, visibility, created_by_agent, data)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(owner)
        .bind(name)
        .bind(&slug)
        .bind(&theme)
        .bind(&visibility)
        .bind(created_by_agent)
        .bind(Value::Object(data))
        .fetch_one(self.pool())
        .await?;
        self.region_event("region.created", &region, created_by_agent).await?;
        self.create_item(NewItem {
            kind: "portal",
            name: &region.name,
            owner_user_id: Some(owner),
            created_by_agent,
            data: json!({ "to_region": region.id, "label": format!("To {}", region.name) }),
            fires_at: None,
            region_id: None,
        })
        .await?;
        self.create_item(NewItem {
            kind: "portal",
            name: "Home",
            owner_user_id: Some(owner),
            created_by_agent,
            data: json!({ "to_region": Value::Null, "label": "Back home" }),
            fires_at: None,
            region_id: Some(region.id),
        })
        .await?;
        Ok(region)
    }

    pub async fn get_region(&self, id: Uuid) -> Result<Option<Region>> {
        Ok(sqlx::query_as::<_, Region>("SELECT * FROM world_regions WHERE id = $1").bind(id).fetch_optional(self.pool()).await?)
    }

    pub async fn find_region(&self, owner: Uuid, org: Option<Uuid>, key: &str) -> Result<Option<Region>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        Ok(sqlx::query_as::<_, Region>(
            "SELECT * FROM world_regions
             WHERE owner_user_id = $1 AND status = 'active' AND ($3::uuid IS NULL OR org_id = $3)
               AND (id::text LIKE lower($2) || '%' OR slug = lower($2) OR lower(name) = lower($2))
             ORDER BY (lower(name) = lower($2)) DESC, created_at DESC LIMIT 1",
        )
        .bind(owner)
        .bind(key)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn find_region_any(&self, owner: Uuid, org: Option<Uuid>, key: &str) -> Result<Option<Region>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        Ok(sqlx::query_as::<_, Region>(
            "SELECT * FROM world_regions
             WHERE owner_user_id = $1 AND ($3::uuid IS NULL OR org_id = $3)
               AND (id::text LIKE lower($2) || '%' OR slug = lower($2) OR lower(name) = lower($2))
             ORDER BY (status = 'active') DESC, (lower(name) = lower($2)) DESC, created_at DESC LIMIT 1",
        )
        .bind(owner)
        .bind(key)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn archived_regions(&self, owner: Uuid, org: Option<Uuid>) -> Result<Vec<Region>> {
        Ok(sqlx::query_as::<_, Region>(
            "SELECT * FROM world_regions WHERE owner_user_id = $1 AND status = 'archived' AND ($2::uuid IS NULL OR org_id = $2)
             ORDER BY updated_at DESC",
        )
        .bind(owner)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn regions_for(&self, viewer: Uuid, org: Option<Uuid>, admin: bool) -> Result<Vec<Region>> {
        Ok(sqlx::query_as::<_, Region>(
            "SELECT * FROM world_regions
             WHERE status = 'active' AND org_id = $3 AND (owner_user_id = $1 OR visibility = 'public' OR $2)
             ORDER BY (owner_user_id = $1) DESC, created_at",
        )
        .bind(viewer)
        .bind(admin)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn region_for(&self, viewer: Uuid, org: Option<Uuid>, admin: bool, id: Uuid) -> Result<Option<RegionDetail>> {
        let Some(region) = self.get_region(id).await?.filter(|r| r.status == "active" && org.is_some() && r.org_id == org) else { return Ok(None) };
        if !(admin || region.owner_user_id == viewer || region.visibility == "public") {
            return Ok(None);
        }
        let items = self.items_in(Some(viewer), org, admin, Some(region.id)).await?;
        Ok(Some(RegionDetail { region, items }))
    }

    pub async fn update_region(&self, id: Uuid, patch: &RegionPatch, by_agent: Option<Uuid>) -> Result<Option<Region>> {
        let theme = pick(patch.theme.as_deref(), THEMES, "theme")?;
        let visibility = pick(patch.visibility.as_deref(), &["private", "public"], "visibility")?;
        let name = patch.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        if name.is_some_and(|n| n.chars().count() > 80) {
            bail!("region names are at most 80 characters");
        }
        let status = patch.archived.map(|a| if a { "archived" } else { "active" });
        let before = self.get_region(id).await?;
        let reopening = status == Some("active") && before.as_ref().is_some_and(|r| r.status == "archived");
        if let (true, Some(r)) = (reopening, &before) {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_regions WHERE owner_user_id = $1 AND status = 'active'")
                .bind(r.owner_user_id)
                .fetch_one(self.pool())
                .await?;
            if count >= MAX_REGIONS_PER_OWNER {
                bail!("your Loka already has {MAX_REGIONS_PER_OWNER} regions; archive one first");
            }
        }
        let region = sqlx::query_as::<_, Region>(
            "UPDATE world_regions SET name = COALESCE($2, name), theme = COALESCE($3, theme),
               visibility = COALESCE($4, visibility), data = data || COALESCE($5, '{}'::jsonb),
               status = COALESCE($6, status), updated_at = NOW()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(theme)
        .bind(visibility)
        .bind(patch.data.as_ref())
        .bind(status)
        .fetch_optional(self.pool())
        .await?;
        if let Some(r) = &region {
            if status == Some("archived") {
                sqlx::query(
                    "UPDATE world_items SET status = 'archived', data = data || '{\"closed_with_region\": true}'::jsonb, updated_at = NOW()
                     WHERE kind = 'portal' AND status = 'active' AND data->>'to_region' = $1::text",
                )
                .bind(r.id)
                .execute(self.pool())
                .await?;
            }
            if reopening {
                sqlx::query(
                    "UPDATE world_items SET status = 'active', data = data - 'closed_with_region', updated_at = NOW()
                     WHERE kind = 'portal' AND status = 'archived' AND data->>'to_region' = $1::text AND data ? 'closed_with_region'",
                )
                .bind(r.id)
                .execute(self.pool())
                .await?;
                let open: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM world_items WHERE kind = 'portal' AND status = 'active' AND data->>'to_region' = $1::text)",
                )
                .bind(r.id)
                .fetch_one(self.pool())
                .await?;
                if !open {
                    self.create_item(NewItem {
                        kind: "portal",
                        name: &r.name,
                        owner_user_id: Some(r.owner_user_id),
                        created_by_agent: by_agent,
                        data: json!({ "to_region": r.id, "label": format!("To {}", r.name) }),
                        fires_at: None,
                        region_id: None,
                    })
                    .await?;
                }
            }
            let kind = if r.status == "archived" { "region.archived" } else { "region.updated" };
            self.region_event(kind, r, by_agent).await?;
        }
        Ok(region)
    }

    pub async fn set_region_ambient(&self, id: Uuid, args: &Value, by_agent: Option<Uuid>) -> Result<Region> {
        let region = self.get_region(id).await?.ok_or_else(|| anyhow!("that region is gone"))?;
        let ambient = clean_ambient(region.data.get("ambient").unwrap_or(&Value::Null), args)?;
        let patch = RegionPatch { data: Some(json!({ "ambient": ambient })), ..Default::default() };
        self.update_region(id, &patch, by_agent).await?.ok_or_else(|| anyhow!("that region is gone"))
    }

    async fn region_event(&self, kind: &str, region: &Region, by_agent: Option<Uuid>) -> Result<()> {
        let mut event = NewEvent::new(kind).payload(json!({
            "region_id": region.id,
            "name": region.name,
            "slug": region.slug,
            "theme": region.theme,
            "visibility": region.visibility,
            "owner_user_id": region.owner_user_id,
        }));
        if let Some(a) = by_agent {
            event = event.agent(a);
        }
        self.emit(event, None).await?;
        Ok(())
    }

    pub async fn resolve_opens(&self, owner: Uuid, value: &Value) -> Result<Value> {
        let kind = value.get("kind").and_then(Value::as_str).map(str::trim).unwrap_or("").to_lowercase();
        let reference = value.get("ref").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if !OPENS_KINDS.contains(&kind.as_str()) {
            bail!("opens.kind must be one of {}", OPENS_KINDS.join(", "));
        }
        if reference.is_empty() {
            bail!("opens.ref is required");
        }
        let resolved = match kind.as_str() {
            "surface" => {
                let r = reference.to_lowercase();
                if !SURFACES.contains(&r.as_str()) {
                    bail!("a surface is one of {}", SURFACES.join(", "));
                }
                r
            }
            "url" => {
                if !(reference.starts_with("https://") || reference.starts_with("http://")) || reference.len() > 2000 {
                    bail!("opens.ref must be an http or https URL");
                }
                reference.to_string()
            }
            "region" => self.find_region(owner, None, reference).await?.ok_or_else(|| anyhow!("no region called {reference}"))?.id.to_string(),
            "challenge" | "list" => self
                .find_item(Some(owner), None, Some(kind.as_str()), reference)
                .await?
                .filter(|i| i.owner_user_id == Some(owner))
                .ok_or_else(|| anyhow!("no {kind} called {reference}"))?
                .id
                .to_string(),
            "quest" => {
                let id: Option<Uuid> = sqlx::query_scalar(
                    "SELECT t.id FROM agent_tasks t
                     LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id
                     WHERE (t.user_id = $1 OR a.owner_user_id = $1) AND t.id::text LIKE lower($2) || '%'
                     ORDER BY t.created_at DESC LIMIT 1",
                )
                .bind(owner)
                .bind(reference)
                .fetch_optional(self.pool())
                .await?;
                id.ok_or_else(|| anyhow!("no quest {reference}; pass its id or first 8 characters"))?.to_string()
            }
            _ => reference.trim_start_matches('#').chars().take(120).collect(),
        };
        Ok(json!({ "kind": kind, "ref": resolved }))
    }

    pub async fn staff_agent(&self, owner: Uuid, key: &str) -> Result<WorldAgent> {
        let key = key.trim().trim_start_matches('@');
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM agent_profiles a
             WHERE owner_user_id = $1
               AND (id::text LIKE lower($2) || '%' OR lower(slug) = lower($2) OR lower(name) = lower($2)
                    OR EXISTS (SELECT 1 FROM agent_slug_aliases aa WHERE aa.agent_id = a.id AND lower(aa.slug) = lower($2)))
             ORDER BY (lower(slug) = lower($2)) DESC, (lower(name) = lower($2)) DESC, created_at
             LIMIT 1",
        )
        .bind(owner)
        .bind(key)
        .fetch_optional(self.pool())
        .await?;
        let id = id.filter(|_| !key.is_empty()).ok_or_else(|| anyhow!("no agent of your person called {key}; a character can only be staffed by one of their own agents"))?;
        self.get_agent(id).await?.ok_or_else(|| anyhow!("that agent is gone"))
    }

    pub async fn npc_history(&self, user: Uuid, npc: Uuid) -> Result<Vec<NpcLine>> {
        if let Some(agent) = self.get_item(npc).await?.as_ref().and_then(staffed_by) {
            return self.staffed_history(user, npc, agent).await;
        }
        let mut lines = sqlx::query_as::<_, NpcLine>(
            "SELECT id AS session_id, asked_by->>'question' AS said,
                    CASE WHEN status = 'completed' THEN summary END AS reply, status, created_at
             FROM agent_sessions
             WHERE role = 'aside' AND asked_by->>'kind' = 'npc'
               AND asked_by->>'npc_id' = $1::text AND asked_by->>'user_id' = $2::text
             ORDER BY created_at DESC LIMIT 30",
        )
        .bind(npc)
        .bind(user)
        .fetch_all(self.pool())
        .await?;
        lines.reverse();
        Ok(lines)
    }

    pub async fn talk_to_npc(&self, user: Uuid, npc_id: Uuid, text: &str) -> std::result::Result<NpcTalk, TalkError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > 2000 {
            return Err(TalkError::Invalid("say something of at most 2000 characters".into()));
        }
        let npc = self.get_item(npc_id).await?.filter(|i| i.kind == "npc" && i.status == "active").ok_or(TalkError::NotFound)?;
        if npc.owner_user_id != Some(user) {
            return Err(TalkError::NotFound);
        }
        if let Some(agent) = staffed_by(&npc) {
            return self.talk_to_staffed(user, &npc, agent, text).await;
        }
        let main = self.main_agent_for(user).await?.ok_or_else(|| TalkError::Blocked("you have no main agent yet".into()))?;
        if let Some(reason) = self.spend_blocker(main.id).await? {
            return Err(TalkError::Blocked(reason));
        }
        let (busy, today): (bool, i64) = sqlx::query_as(
            "SELECT
               EXISTS(SELECT 1 FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
                      WHERE s.asked_by->>'kind' = 'npc' AND s.asked_by->>'npc_id' = $1::text
                        AND s.asked_by->>'user_id' = $2::text AND r.status IN ('queued', 'running')),
               (SELECT COUNT(*) FROM agent_sessions s
                 WHERE s.role = 'aside' AND s.asked_by->>'kind' = 'npc' AND s.asked_by->>'user_id' = $2::text
                   AND s.created_at >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')",
        )
        .bind(npc.id)
        .bind(user)
        .fetch_one(self.pool())
        .await?;
        if busy {
            return Err(TalkError::Busy);
        }
        if today >= MAX_NPC_LINES_PER_DAY {
            return Err(TalkError::Blocked(format!("you have talked to Loka characters {MAX_NPC_LINES_PER_DAY} times today; they rest until tomorrow")));
        }
        let owner = self.user_display_name(user).await?.unwrap_or_else(|| "your visitor".into());
        let region = match npc.region_id {
            Some(r) => self.get_region(r).await?,
            None => None,
        };
        let history = self.npc_history(user, npc.id).await?;
        let prompt = npc_prompt(&npc, &owner, region.as_ref(), &main, &history, text);
        let asked_by = json!({
            "kind": "npc",
            "npc_id": npc.id,
            "npc": npc.name,
            "user_id": user,
            "name": owner,
            "question": text,
        });
        let run = self.start_lean_aside(main.id, asked_by, &prompt).await.map_err(|e| TalkError::Blocked(e.to_string()))?;
        self.emit(
            NewEvent::new("npc.talked").agent(main.id).session(run.session_id).actor(Some(user)).payload(json!({
                "npc_id": npc.id,
                "npc": npc.name,
                "session_id": run.session_id,
                "said": text,
                "region_id": npc.region_id,
                "owner_user_id": user,
            })),
            None,
        )
        .await?;
        Ok(NpcTalk { npc_id: npc.id, session_id: run.session_id, run_id: Some(run.id), agent_id: None, status: "queued" })
    }

    async fn talk_to_staffed(&self, user: Uuid, npc: &WorldItem, agent_id: Uuid, text: &str) -> std::result::Result<NpcTalk, TalkError> {
        let agent = self.get_agent(agent_id).await?.filter(|a| a.owner_user_id == Some(user));
        let agent = agent.ok_or_else(|| TalkError::Blocked(format!("{} is no longer staffed by one of your agents", npc.name)))?;
        if let Some(reason) = self.spend_blocker(agent.id).await? {
            return Err(TalkError::Blocked(reason.replace("your main agent", &agent.name)));
        }
        if self.onboarding_for(user, agent.id).await?.is_some() {
            return Err(TalkError::Blocked(format!("finish setting up {} in chat first", agent.name)));
        }
        let region = match npc.region_id {
            Some(r) => self.get_region(r).await?,
            None => None,
        };
        let via = json!({
            "id": npc.id,
            "name": npc.name,
            "region_id": npc.region_id,
            "region": region.as_ref().map(|r| r.name.clone()),
        });
        let author = self.user_display_name(user).await?.unwrap_or_else(|| "someone".into());
        let msg = self
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "user",
                author_user_id: Some(user),
                author_agent_id: None,
                body: text,
                session_id: None,
                run_id: None,
                meta: json!({ "npc": via }),
            })
            .await?;
        self.emit(
            NewEvent::new("message.created").agent(agent.id).actor(Some(user)).payload(json!({
                "message_id": msg.id,
                "author": author,
                "author_kind": "user",
                "body": text,
                "attachments": [],
                "options": Value::Null,
                "via_npc": via,
            })),
            Some((agent.id, "message")),
        )
        .await?;
        self.emit(
            NewEvent::new("npc.talked").agent(agent.id).actor(Some(user)).payload(json!({
                "npc_id": npc.id,
                "npc": npc.name,
                "session_id": msg.id,
                "message_id": msg.id,
                "agent_id": agent.id,
                "said": text,
                "region_id": npc.region_id,
                "owner_user_id": user,
            })),
            None,
        )
        .await?;
        Ok(NpcTalk { npc_id: npc.id, session_id: msg.id, run_id: None, agent_id: Some(agent.id), status: "queued" })
    }

    async fn staffed_history(&self, user: Uuid, npc: Uuid, agent: Uuid) -> Result<Vec<NpcLine>> {
        let mut lines = sqlx::query_as::<_, NpcLine>(
            "SELECT m.id AS session_id, m.body AS said, r.reply,
                    CASE WHEN r.reply IS NOT NULL OR nx.at IS NOT NULL THEN 'completed'
                         WHEN EXISTS (SELECT 1 FROM agent_inbox i WHERE i.agent_id = m.agent_id AND i.status IN ('pending', 'processing')
                                        AND i.kind = 'message' AND i.payload->>'message_id' = m.id::text) THEN 'active'
                         WHEN m.created_at > NOW() - interval '2 minutes' THEN 'active'
                         ELSE 'completed' END AS status,
                    m.created_at
             FROM agent_messages m
             LEFT JOIN LATERAL (
               SELECT MIN(n.created_at) AS at FROM agent_messages n
               WHERE n.agent_id = m.agent_id AND n.author_kind = 'user' AND n.author_user_id = m.author_user_id AND n.created_at > m.created_at
             ) nx ON TRUE
             LEFT JOIN LATERAL (
               SELECT string_agg(a.body, E'\n\n' ORDER BY a.created_at) AS reply FROM agent_messages a
               WHERE a.agent_id = m.agent_id AND a.author_kind = 'agent' AND a.created_at > m.created_at
                 AND COALESCE((a.meta->>'thread_user_id')::uuid, (SELECT owner_user_id FROM agent_profiles WHERE id = a.agent_id)) = m.author_user_id
                 AND (nx.at IS NULL OR a.created_at < nx.at)
             ) r ON TRUE
             WHERE m.agent_id = $1 AND m.author_user_id = $2 AND m.author_kind = 'user' AND m.meta->'npc'->>'id' = $3::text
             ORDER BY m.created_at DESC LIMIT 30",
        )
        .bind(agent)
        .bind(user)
        .bind(npc)
        .fetch_all(self.pool())
        .await?;
        lines.reverse();
        Ok(lines)
    }

    pub async fn deliver_loka_answer(&self, agent: &WorldAgent, session: &AgentSession, answer: &str, ok: bool) -> Result<bool> {
        let asked_by = session.asked_by.clone().unwrap_or(Value::Null);
        match asked_by.get("kind").and_then(Value::as_str) {
            Some("npc") => {
                self.deliver_npc_reply(agent, session, &asked_by, answer, ok).await?;
                Ok(true)
            }
            Some("grade") => {
                self.deliver_grade(&asked_by, answer, ok).await?;
                Ok(true)
            }
            Some(crate::setup_scan::KIND) => {
                self.deliver_setup_scan(agent, session, answer, ok).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn deliver_npc_reply(&self, agent: &WorldAgent, session: &AgentSession, asked_by: &Value, answer: &str, ok: bool) -> Result<()> {
        let delivered = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM world_events WHERE session_id = $1 AND type = 'npc.replied')")
            .bind(session.id)
            .fetch_one(self.pool())
            .await?;
        if delivered {
            return Ok(());
        }
        let reply: String = answer.trim().chars().take(2000).collect();
        self.emit(
            NewEvent::new("npc.replied").agent(agent.id).session(session.id).payload(json!({
                "npc_id": asked_by.get("npc_id"),
                "npc": asked_by.get("npc"),
                "session_id": session.id,
                "said": asked_by.get("question"),
                "reply": if ok { Value::String(reply.clone()) } else { Value::Null },
                "ok": ok,
                "error": if ok { Value::Null } else { Value::String(reply) },
                "owner_user_id": asked_by.get("user_id"),
            })),
            None,
        )
        .await?;
        Ok(())
    }
}

fn npc_prompt(npc: &WorldItem, owner: &str, region: Option<&Region>, main: &WorldAgent, history: &[NpcLine], text: &str) -> String {
    let d = |k: &str| npc.data.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let place = match region {
        Some(r) => format!("the {} region \"{}\"", r.theme, r.name),
        None => "the home town".to_string(),
    };
    let mut out = format!(
        "You are {}, a character who lives in {place} of {owner}'s Loka, a pixel world where their AI agents live and work.\n",
        npc.name
    );
    if let Some(p) = d("persona") {
        out.push_str(&format!("Who you are: {p}\n"));
    }
    if let Some(r) = region.and_then(|r| r.data.get("description")).and_then(Value::as_str) {
        out.push_str(&format!("About this place: {r}\n"));
    }
    if let Some(script) = npc.data.get("dialogue").and_then(Value::as_array).filter(|s| !s.is_empty()) {
        let lines: Vec<String> = script.iter().filter_map(|l| l.get("text").and_then(Value::as_str)).take(12).map(|t| format!("- {t}")).collect();
        out.push_str(&format!("Lines you are known for, use them when they fit:\n{}\n", lines.join("\n")));
    }
    out.push_str(&format!(
        "Stay in character. Reply in one to four short spoken sentences, plain text, no markdown, lists or code fences. You have no tools and cannot change anything in Loka or do real work. If {owner} asks for real work, tell them to ask {} in the home town.\n",
        main.name
    ));
    let past: Vec<String> = history
        .iter()
        .rev()
        .take(NPC_HISTORY as usize)
        .rev()
        .filter_map(|l| Some(format!("{owner}: {}\n{}: {}", l.said.as_deref()?, npc.name, l.reply.as_deref()?)))
        .collect();
    if past.is_empty() {
        if let Some(g) = d("greeting") {
            out.push_str(&format!("\nYou greeted them with: {g}\n"));
        }
    } else {
        out.push_str(&format!("\nConversation so far:\n{}\n", past.join("\n")));
    }
    out.push_str(&format!("\n{owner} says:\n{text}"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: Value) -> WorldEvent {
        serde_json::from_value(json!({
            "seq": 1, "type": kind, "agent_id": null, "session_id": null, "run_id": null, "task_id": null,
            "actor_user_id": null, "payload": payload, "created_at": Utc::now()
        }))
        .unwrap()
    }

    #[test]
    fn loka_events_stay_with_their_owner() {
        let me = Uuid::new_v4();
        let other = Uuid::new_v4();
        let private = event("region.created", json!({ "owner_user_id": other, "visibility": "private" }));
        let public = event("region.created", json!({ "owner_user_id": other, "visibility": "public" }));
        let graded = event("challenge.graded", json!({ "owner_user_id": other }));
        let hidden_item = event("item.created", json!({ "owner_user_id": other, "private": true }));
        let open_item = event("item.created", json!({ "owner_user_id": other, "private": false }));
        assert!(hidden_from(&private, me));
        assert!(!hidden_from(&private, other));
        assert!(!hidden_from(&public, me));
        assert!(hidden_from(&graded, me));
        assert!(hidden_from(&hidden_item, me));
        assert!(!hidden_from(&open_item, me));
        assert!(!hidden_from(&event("task.finished", json!({})), me));
    }

    #[test]
    fn slugs_and_ambient() {
        assert_eq!(slugify("  Whispering Pines! "), "whispering-pines");
        assert_eq!(slugify("***"), "region");
        let a = clean_ambient(&json!({ "weather": "rain" }), &json!({ "time_of_day": "dusk", "critters": ["fox", { "species": "Owl", "count": 30 }] })).unwrap();
        assert_eq!(a["weather"], "rain");
        assert_eq!(a["time_of_day"], "dusk");
        assert_eq!(a["critters"][1], json!({ "species": "owl", "count": 8 }));
        assert!(clean_ambient(&Value::Null, &json!({ "weather": "lava" })).is_err());
    }

    async fn person(pool: &sqlx::PgPool, name: &str) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, name, email, world_role) VALUES ($1, $2, $3, 'write')")
            .bind(id)
            .bind(name)
            .bind(format!("{id}@example.test"))
            .execute(pool)
            .await?;
        Ok(id)
    }

    #[tokio::test]
    async fn regions_and_their_items_stay_with_their_owner() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LOKA_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let ana = person(&pool, "Ana").await?;
        let ben = person(&pool, "Ben").await?;
        let pines = store.create_region(ana, "Whispering Pines", Some("forest"), None, Some("Quiet woods"), None).await?;
        assert_eq!(pines.slug, "whispering-pines");
        assert_eq!(pines.visibility, "private");
        let again = store.create_region(ana, "Whispering Pines", None, None, None, None).await?;
        assert_eq!(again.slug, "whispering-pines-2");
        assert!(store.create_region(ana, "Lava", Some("volcano"), None, None, None).await.is_err());

        let hearth_portal = |items: &[WorldItem]| items.iter().any(|i| i.kind == "portal" && i.data["to_region"] == json!(pines.id));
        assert!(hearth_portal(&store.items_for(Some(ana), Some(crate::orgs::DEFAULT_ORG), false).await?));
        assert!(!hearth_portal(&store.items_for(Some(ben), Some(crate::orgs::DEFAULT_ORG), false).await?), "a portal to a private region stays hidden");
        assert!(hearth_portal(&store.items_for(Some(ben), Some(crate::orgs::DEFAULT_ORG), true).await?), "admins see everything");

        assert!(store.regions_for(ben, Some(crate::orgs::DEFAULT_ORG), false).await?.iter().all(|r| r.id != pines.id));
        assert!(store.region_for(ben, Some(crate::orgs::DEFAULT_ORG), false, pines.id).await?.is_none());
        let inside = store.region_for(ana, Some(crate::orgs::DEFAULT_ORG), false, pines.id).await?.expect("own region");
        assert!(inside.items.iter().any(|i| i.kind == "portal" && i.data["to_region"].is_null()));
        assert!(store.find_region(ben, None, "whispering-pines").await?.is_none());

        let ben_npc = store
            .create_item(NewItem { kind: "npc", name: "Home", owner_user_id: Some(ben), created_by_agent: None, data: json!({ "persona": "x" }), fires_at: None, region_id: None })
            .await?;
        let found = store.find_item(Some(ben), None, None, "Home").await?.expect("ben finds something");
        assert_eq!(found.id, ben_npc.id, "the return portal inside Ana's private region is invisible to Ben");

        let created: WorldEvent = sqlx::query_as("SELECT * FROM world_events WHERE type = 'region.created' AND payload->>'region_id' = $1")
            .bind(pines.id.to_string())
            .fetch_one(&pool)
            .await?;
        assert!(hidden_from(&created, ben));
        assert!(!hidden_from(&created, ana));
        let portal_event: WorldEvent = sqlx::query_as(
            "SELECT * FROM world_events WHERE type = 'item.created' AND payload->>'owner_user_id' = $1 AND payload->>'kind' = 'portal' AND payload->>'region_id' IS NULL",
        )
        .bind(ana.to_string())
        .fetch_one(&pool)
        .await?;
        assert!(hidden_from(&portal_event, ben), "the home town portal event names a private region");

        let r = store.set_region_ambient(pines.id, &json!({ "weather": "fog", "critters": [{ "species": "firefly", "count": 5 }] }), None).await?;
        assert_eq!(r.data["ambient"]["weather"], "fog");
        assert_eq!(r.data["description"], "Quiet woods");

        let public = RegionPatch { visibility: Some("public".into()), ..Default::default() };
        store.update_region(pines.id, &public, None).await?;
        assert!(store.regions_for(ben, Some(crate::orgs::DEFAULT_ORG), false).await?.iter().any(|r| r.id == pines.id));
        assert!(store.region_for(ben, Some(crate::orgs::DEFAULT_ORG), false, pines.id).await?.is_some());
        assert!(hearth_portal(&store.items_for(Some(ben), Some(crate::orgs::DEFAULT_ORG), false).await?));

        let archive = RegionPatch { archived: Some(true), ..Default::default() };
        store.update_region(pines.id, &archive, None).await?;
        assert!(store.region_for(ana, Some(crate::orgs::DEFAULT_ORG), false, pines.id).await?.is_none());
        assert!(!hearth_portal(&store.items_for(Some(ana), Some(crate::orgs::DEFAULT_ORG), false).await?), "archiving closes its portals");
        assert!(store.archived_regions(ana, Some(crate::orgs::DEFAULT_ORG)).await?.iter().any(|r| r.id == pines.id));
        assert_eq!(store.find_region_any(ana, Some(crate::orgs::DEFAULT_ORG), &pines.slug).await?.map(|r| r.id), Some(pines.id));
        let reopen = RegionPatch { archived: Some(false), ..Default::default() };
        store.update_region(pines.id, &reopen, None).await?;
        assert!(hearth_portal(&store.items_for(Some(ana), Some(crate::orgs::DEFAULT_ORG), false).await?), "restoring reopens its portals");
        store.update_region(pines.id, &archive, None).await?;

        let opens = store.resolve_opens(ana, &json!({ "kind": "surface", "ref": "Timers" })).await?;
        assert_eq!(opens, json!({ "kind": "surface", "ref": "timers" }));
        assert!(store.resolve_opens(ana, &json!({ "kind": "surface", "ref": "bank" })).await.is_err());
        assert!(store.resolve_opens(ana, &json!({ "kind": "url", "ref": "javascript:alert(1)" })).await.is_err());
        assert!(store.resolve_opens(ana, &json!({ "kind": "region", "ref": again.slug })).await.is_ok());
        assert!(store.resolve_opens(ben, &json!({ "kind": "region", "ref": again.slug })).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn npc_talk_and_grading_run_as_lean_asides() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LOKA_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let me = person(&pool, "Cleo").await?;
        let stranger = person(&pool, "Dev").await?;
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Mira', 'codex', 'gpt-5.5', $2) RETURNING id")
            .bind(format!("mira-{}", &me.to_string()[..8]))
            .bind(me)
            .fetch_one(&pool)
            .await?;
        sqlx::query("UPDATE users SET main_agent_id = $2 WHERE id = $1").bind(me).bind(main).execute(&pool).await?;
        let docks = store.create_region(me, "Docks", Some("docks"), None, None, Some(main)).await?;
        let npc = store
            .create_item(NewItem {
                kind: "npc",
                name: "Old Salt",
                owner_user_id: Some(me),
                created_by_agent: Some(main),
                data: json!({ "persona": "A gruff harbourmaster", "greeting": "Ahoy" }),
                fires_at: None,
                region_id: Some(docks.id),
            })
            .await?;
        assert!(matches!(store.talk_to_npc(me, npc.id, "hello").await, Err(TalkError::Blocked(_))), "no main session yet");
        sqlx::query("INSERT INTO agent_sessions (agent_id, role, backend, model, working_dir, status) VALUES ($1, 'main', 'codex', 'gpt-5.5', '/tmp/mira', 'active')")
            .bind(main)
            .execute(&pool)
            .await?;
        assert!(matches!(store.talk_to_npc(stranger, npc.id, "hello").await, Err(TalkError::NotFound)));
        let talk = store.talk_to_npc(me, npc.id, "Any ships today?").await.map_err(|e| anyhow!("{e:?}"))?;
        assert!(matches!(store.talk_to_npc(me, npc.id, "again").await, Err(TalkError::Busy)));
        let session = store.get_session(talk.session_id).await?.expect("aside session");
        assert_eq!((session.role.as_str(), session.backend.as_str(), session.model.as_str()), ("aside", "claude", "sonnet"));
        assert!(crate::world::toolless_aside(session.asked_by.as_ref()));
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1").bind(talk.run_id).fetch_one(&pool).await?;
        assert!(prompt.contains("Old Salt") && prompt.contains("gruff harbourmaster") && prompt.contains("Any ships today?"));

        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1").bind(talk.run_id).execute(&pool).await?;
        store.set_session_status(session.id, "completed", Some("Three trawlers, lad.")).await?;
        let agent = store.get_agent(main).await?.unwrap();
        assert!(store.deliver_loka_answer(&agent, &session, "Three trawlers, lad.", true).await?);
        assert!(store.deliver_loka_answer(&agent, &session, "Three trawlers, lad.", true).await?);
        let replies: Vec<Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE type = 'npc.replied' AND session_id = $1")
            .bind(session.id)
            .fetch_all(&pool)
            .await?;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["reply"], "Three trawlers, lad.");
        assert_eq!(replies[0]["owner_user_id"], json!(me));
        let history = store.npc_history(me, npc.id).await?;
        assert_eq!(history.last().and_then(|l| l.reply.as_deref()), Some("Three trawlers, lad."));

        let (challenge, _) = store
            .create_challenge(me, Some(main), &json!({ "title": "Mooring knots", "type": "puzzle", "prompt": "Which knot?", "skill": "logic", "rubric": "bowline" }), Some(docks.id))
            .await?;
        let attempt = store.submit_attempt(me, challenge.id, "A bowline").await.map_err(|e| anyhow!("{e:?}"))?;
        assert_eq!(attempt.status, "pending");
        let grading = store.get_session(attempt.session_id.expect("grading session")).await?.unwrap();
        let grade_prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE session_id = $1").bind(grading.id).fetch_one(&pool).await?;
        assert!(grade_prompt.contains("bowline") && grade_prompt.contains("<<<ANSWER"));
        store.deliver_loka_answer(&agent, &grading, "Spot on.\n<!-- grade {\"passed\": true, \"score\": 95} grade -->", true).await?;
        let done = store.get_attempt(attempt.id).await?.unwrap();
        assert_eq!((done.status.as_str(), done.score, done.xp_awarded), ("passed", Some(95), 40));

        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Daily budget of $1.00 reached' WHERE id = $1").bind(main).execute(&pool).await?;
        assert!(matches!(store.talk_to_npc(me, npc.id, "hi").await, Err(TalkError::Blocked(_))));
        Ok(())
    }

    #[tokio::test]
    async fn staffed_characters_message_their_agent() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("LOKA_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let me = person(&pool, "Esme").await?;
        let other = person(&pool, "Finn").await?;
        let agent = |name: &str, owner: Uuid| {
            sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id")
                .bind(format!("{}-{}", name.to_lowercase(), &Uuid::new_v4().to_string()[..8]))
                .bind(name.to_string())
                .bind(owner)
                .fetch_one(&pool)
        };
        let smith = agent("Smith", me).await?;
        let theirs = agent("Rook", other).await?;
        assert_eq!(store.staff_agent(me, "smith").await?.id, smith);
        assert!(store.staff_agent(me, "Rook").await.is_err(), "another owner's agent cannot staff my character");
        assert!(store.staff_agent(me, &theirs.to_string()).await.is_err());

        let forge = store.create_region(me, "Forge Row", Some("city"), None, None, None).await?;
        let npc = store
            .create_item(NewItem {
                kind: "npc",
                name: "Smithy",
                owner_user_id: Some(me),
                created_by_agent: None,
                data: json!({ "persona": "x", "agent_id": smith }),
                fires_at: None,
                region_id: Some(forge.id),
            })
            .await?;
        assert_eq!(staffed_by(&npc), Some(smith));
        assert!(matches!(store.talk_to_npc(other, npc.id, "hi").await, Err(TalkError::NotFound)));
        let talk = store.talk_to_npc(me, npc.id, "Fix the login bug").await.map_err(|e| anyhow!("{e:?}"))?;
        assert_eq!((talk.agent_id, talk.run_id), (Some(smith), None));

        let (author, meta): (Option<Uuid>, Value) = sqlx::query_as("SELECT author_user_id, meta FROM agent_messages WHERE id = $1").bind(talk.session_id).fetch_one(&pool).await?;
        assert_eq!(author, Some(me));
        assert_eq!(meta["npc"]["name"], "Smithy");
        let inbox: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'message'").bind(smith).fetch_one(&pool).await?;
        assert_eq!(inbox["via_npc"]["region"], "Forge Row");
        assert_eq!(inbox["from_owner"], json!(true), "the inbox trigger verifies the owner");
        let asides: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE agent_id = $1").bind(smith).fetch_one(&pool).await?;
        assert_eq!(asides, 0, "a staffed character never runs a lean aside");

        let waiting = store.npc_history(me, npc.id).await?;
        assert_eq!((waiting.len(), waiting[0].status.as_str(), waiting[0].reply.as_deref()), (1, "active", None));
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, created_at) VALUES ($1, 'agent', $1, 'On it, starting a quest.', NOW() + interval '1 second')")
            .bind(smith)
            .execute(&pool)
            .await?;
        let answered = store.npc_history(me, npc.id).await?;
        assert_eq!((answered[0].status.as_str(), answered[0].reply.as_deref()), ("completed", Some("On it, starting a quest.")));
        assert!(store.npc_history(other, npc.id).await?.is_empty());

        sqlx::query("UPDATE agent_profiles SET paused_reason = 'Model key rejected: X' WHERE id = $1").bind(smith).execute(&pool).await?;
        match store.talk_to_npc(me, npc.id, "hello?").await {
            Err(TalkError::Blocked(m)) => assert!(m.contains("Smith")),
            other => panic!("expected blocked, got {other:?}"),
        }
        crate::orgs::join_default_for_tests(&pool, other, None).await?;
        sqlx::query("UPDATE agent_profiles SET paused_reason = NULL, owner_user_id = $2 WHERE id = $1").bind(smith).bind(other).execute(&pool).await?;
        assert!(matches!(store.talk_to_npc(me, npc.id, "hello?").await, Err(TalkError::Blocked(_))), "a transferred agent no longer staffs my character");
        Ok(())
    }

    #[test]
    fn dialogue_is_cleaned() {
        let d = parse_dialogue(Some(&json!([{ "text": "Hi", "options": [{ "label": "Bye", "next": "l2" }, { "label": " " }] }, { "id": "l2", "text": "See you" }]))).unwrap().unwrap();
        assert_eq!(d[0]["id"], "l1");
        assert_eq!(d[0]["options"].as_array().unwrap().len(), 1);
        assert!(parse_dialogue(Some(&json!([{ "text": "" }]))).is_err());
        assert!(parse_dialogue(None).unwrap().is_none());
    }
}
