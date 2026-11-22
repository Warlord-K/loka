use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{emit_in, NewEvent, WorldStore};

pub const KINDS: &[&str] = &["building", "decoration", "critter", "timer", "list", "portal", "npc", "challenge", "adventure"];
pub const PUBLIC_KINDS: &[&str] = &["building", "decoration", "critter", "portal", "npc"];

const ITEM_COLUMNS: &str = "id, kind, name, owner_user_id, created_by_agent, data, status, fires_at, created_at, updated_at, org_id, region_id";

const VISIBLE_ITEMS: &str = "status <> 'archived' AND org_id = $5
   AND region_id IS NOT DISTINCT FROM $4
   AND kind NOT IN ('challenge', 'adventure')
   AND (kind = ANY($2) OR $3 OR owner_user_id IS NOT DISTINCT FROM $1)
   AND ($3 OR kind <> 'portal' OR COALESCE(data->>'to_region', '') = '' OR EXISTS (
     SELECT 1 FROM world_regions wr WHERE wr.id::text = world_items.data->>'to_region' AND wr.status = 'active'
       AND (wr.visibility = 'public' OR wr.owner_user_id IS NOT DISTINCT FROM $1)))";

pub(crate) const REGION_VISIBLE: &str = "(region_id IS NULL OR EXISTS (
  SELECT 1 FROM world_regions wr WHERE wr.id = world_items.region_id AND wr.status = 'active'
    AND (wr.visibility = 'public' OR wr.owner_user_id IS NOT DISTINCT FROM $1)))";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorldItem {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub owner_user_id: Option<Uuid>,
    pub created_by_agent: Option<Uuid>,
    pub data: Value,
    pub status: String,
    pub fires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default)]
    pub region_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_count: Option<i64>,
}

pub struct NewItem<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub owner_user_id: Option<Uuid>,
    pub created_by_agent: Option<Uuid>,
    pub data: Value,
    pub fires_at: Option<DateTime<Utc>>,
    pub region_id: Option<Uuid>,
}

pub struct NewList<'a> {
    pub owner: Uuid,
    pub creator: Option<Uuid>,
    pub org: Uuid,
    pub title: &'a str,
    pub items: Vec<String>,
    pub note: Option<Value>,
    pub region: Option<Uuid>,
}

pub fn list_data(items: Vec<String>, note: Option<Value>) -> Value {
    let mut data = json!({
        "items": items
            .into_iter()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|text| json!({ "text": text, "done": false }))
            .collect::<Vec<_>>()
    });
    if note.as_ref().is_some_and(|value| value.as_object().is_some_and(|object| !object.is_empty())) {
        data["note"] = note.unwrap_or_default();
    }
    data
}

fn item_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("items need a name");
    }
    Ok(name)
}

impl WorldStore {
    pub async fn create_item(&self, new: NewItem<'_>) -> Result<WorldItem> {
        if !KINDS.contains(&new.kind) {
            bail!("unknown item kind {}", new.kind);
        }
        let name = item_name(new.name)?;
        let item = sqlx::query_as::<_, WorldItem>(
            "INSERT INTO world_items (kind, name, owner_user_id, created_by_agent, data, fires_at, region_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(new.kind)
        .bind(name)
        .bind(new.owner_user_id)
        .bind(new.created_by_agent)
        .bind(&new.data)
        .bind(new.fires_at)
        .bind(new.region_id)
        .fetch_one(self.pool())
        .await?;
        self.item_event("item.created", &item).await?;
        Ok(item)
    }

    pub async fn create_list(&self, new: NewList<'_>) -> Result<WorldItem> {
        let title = item_name(new.title)?;
        let mut tx = self.pool().begin().await?;
        let item = sqlx::query_as::<_, WorldItem>(
            "INSERT INTO world_items (kind, name, owner_user_id, created_by_agent, data, region_id, org_id)
             VALUES ('list', $1, $2, $3, $4, $5, $6) RETURNING *",
        )
        .bind(title)
        .bind(new.owner)
        .bind(new.creator)
        .bind(list_data(new.items, new.note))
        .bind(new.region)
        .bind(new.org)
        .fetch_one(&mut *tx)
        .await?;
        let mut event = NewEvent::new("item.created").payload(json!({
            "item_id": item.id,
            "org_id": item.org_id,
            "kind": item.kind,
            "name": item.name,
            "owner_user_id": item.owner_user_id,
            "region_id": item.region_id,
            "private": true,
        }));
        if let Some(agent) = item.created_by_agent {
            event = event.agent(agent);
        }
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        Ok(item)
    }

    pub async fn get_item(&self, id: Uuid) -> Result<Option<WorldItem>> {
        Ok(sqlx::query_as::<_, WorldItem>("SELECT * FROM world_items WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn item_for_viewer(&self, id: Uuid, viewer: Uuid, org: Uuid, admin: bool) -> Result<Option<WorldItem>> {
        Ok(sqlx::query_as::<_, WorldItem>(
            "SELECT i.* FROM world_items i
             LEFT JOIN world_regions r ON r.id = i.region_id
             WHERE i.id = $1 AND i.status <> 'archived' AND i.org_id = $3
               AND ((i.kind IN ('list', 'timer') AND i.owner_user_id = $2
                     AND (i.region_id IS NULL OR (r.status = 'active' AND r.org_id = $3
                          AND (r.owner_user_id = $2 OR r.visibility = 'public' OR $4))))
                    OR (i.kind IN ('building', 'npc')
                        AND (i.region_id IS NULL OR (r.status = 'active' AND r.org_id = $3
                             AND (r.owner_user_id = $2 OR r.visibility = 'public' OR $4)))))",
        )
        .bind(id)
        .bind(viewer)
        .bind(org)
        .bind(admin)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn find_item(&self, owner: Option<Uuid>, org: Option<Uuid>, kind: Option<&str>, key: &str) -> Result<Option<WorldItem>> {
        let sql = format!(
            "SELECT * FROM world_items
             WHERE status <> 'archived' AND ($5::uuid IS NULL OR org_id = $5)
               AND ($2::text IS NULL OR kind = $2)
               AND (id::text LIKE lower($3) || '%' OR lower(name) = lower($3))
               AND (kind = ANY($4) OR owner_user_id IS NOT DISTINCT FROM $1)
               AND {REGION_VISIBLE}
             ORDER BY (owner_user_id IS NOT DISTINCT FROM $1) DESC, (lower(name) = lower($3)) DESC, updated_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, WorldItem>(&sql)
        .bind(owner)
        .bind(kind)
        .bind(key.trim())
        .bind(PUBLIC_KINDS.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn items_for(&self, viewer: Option<Uuid>, org: Option<Uuid>, admin: bool) -> Result<Vec<WorldItem>> {
        Ok(sqlx::query_as::<_, WorldItem>(&format!(
            "WITH visible AS (SELECT * FROM world_items WHERE {VISIBLE_ITEMS}),
             finished AS (
               SELECT DISTINCT ON (owner_user_id, created_by_agent, name, reminder) *
                 FROM (SELECT v.*, count(*) OVER w AS done_count, (created_by_agent IS NOT NULL AND jsonb_typeof(data->'then') = 'string' AND data->>'then' ~ '\\S') AS reminder
                         FROM visible v WHERE kind = 'timer' AND status = 'done'
                       WINDOW w AS (PARTITION BY owner_user_id, created_by_agent, name,
                         (created_by_agent IS NOT NULL AND jsonb_typeof(data->'then') = 'string' AND data->>'then' ~ '\\S'))) f
                ORDER BY owner_user_id, created_by_agent, name, reminder, created_at, id)
             SELECT {ITEM_COLUMNS}, NULL::bigint AS done_count FROM visible WHERE NOT (kind = 'timer' AND status = 'done')
             UNION ALL
             SELECT {ITEM_COLUMNS}, done_count FROM finished
             ORDER BY created_at, id"
        ))
        .bind(viewer)
        .bind(PUBLIC_KINDS.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(admin)
        .bind(None::<Uuid>)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn items_in(&self, viewer: Option<Uuid>, org: Option<Uuid>, admin: bool, region: Option<Uuid>) -> Result<Vec<WorldItem>> {
        Ok(sqlx::query_as::<_, WorldItem>(&format!("SELECT * FROM world_items WHERE {VISIBLE_ITEMS} ORDER BY created_at"))
        .bind(viewer)
        .bind(PUBLIC_KINDS.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .bind(admin)
        .bind(region)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn update_item(&self, id: Uuid, name: Option<&str>, data: Option<&Value>, status: Option<&str>) -> Result<Option<WorldItem>> {
        let item = sqlx::query_as::<_, WorldItem>(
            "UPDATE world_items SET name = COALESCE($2, name), data = COALESCE($3, data),
               status = COALESCE($4, status), updated_at = NOW()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(data)
        .bind(status)
        .fetch_optional(self.pool())
        .await?;
        if let Some(i) = &item {
            self.item_event(if i.status == "archived" { "item.removed" } else { "item.updated" }, i).await?;
        }
        Ok(item)
    }

    pub async fn archive_finished_like(&self, item: &WorldItem) -> Result<u64> {
        let archived = sqlx::query(
            "UPDATE world_items SET status = 'archived', updated_at = NOW()
              WHERE kind = 'timer' AND status = 'done' AND id <> $1
                AND org_id IS NOT DISTINCT FROM $2 AND region_id IS NOT DISTINCT FROM $3
                AND owner_user_id IS NOT DISTINCT FROM $4 AND created_by_agent IS NOT DISTINCT FROM $5 AND name = $6
                AND (created_by_agent IS NOT NULL AND jsonb_typeof(data->'then') = 'string' AND data->>'then' ~ '\\S')
                    = (created_by_agent IS NOT NULL AND jsonb_typeof($7::jsonb->'then') = 'string' AND $7::jsonb->>'then' ~ '\\S')",
        )
        .bind(item.id)
        .bind(item.org_id)
        .bind(item.region_id)
        .bind(item.owner_user_id)
        .bind(item.created_by_agent)
        .bind(&item.name)
        .bind(&item.data)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(archived)
    }

    pub async fn next_timer_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT LEAST(
               (SELECT MIN(fires_at) FROM world_items WHERE kind = 'timer' AND status = 'active'),
               (SELECT MIN(nudge_at) FROM world_todos WHERE status = 'open'))",
        )
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn prune_done_timers(&self, older_than_days: i32) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM world_items WHERE id IN (
               SELECT id FROM world_items
                WHERE kind = 'timer' AND status = 'done' AND updated_at < NOW() - make_interval(days => $1)
                LIMIT 5000)",
        )
        .bind(older_than_days)
        .execute(self.pool())
        .await?
        .rows_affected())
    }

    pub async fn reminders_set_since(&self, agent: Uuid, hours: i32) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM world_items
              WHERE kind = 'timer' AND created_by_agent = $1 AND created_at > NOW() - make_interval(hours => $2)",
        )
        .bind(agent)
        .bind(hours)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn fire_due_timers(&self) -> Result<Vec<WorldItem>> {
        self.fire_due_nudges().await?;
        let fired = sqlx::query_as::<_, WorldItem>(
            "UPDATE world_items SET status = 'done', updated_at = NOW()
             WHERE id IN (
               SELECT id FROM world_items WHERE kind = 'timer' AND status = 'active' AND fires_at <= NOW()
               FOR UPDATE SKIP LOCKED
             ) RETURNING *",
        )
        .fetch_all(self.pool())
        .await?;
        for t in &fired {
            let then = t.data.get("then").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
            let mut event = NewEvent::new("timer.done").payload(json!({
                "item_id": t.id,
                "label": t.name,
                "owner_user_id": t.owner_user_id,
                "then": then,
            }));
            if let Some(a) = t.created_by_agent {
                event = event.agent(a);
            }
            let wake = match (t.created_by_agent, then) {
                (Some(a), Some(_)) => Some((a, "timer.done")),
                _ => None,
            };
            self.emit(event, wake).await?;
        }
        Ok(fired)
    }

    async fn item_is_private(&self, item: &WorldItem) -> Result<bool> {
        if !PUBLIC_KINDS.contains(&item.kind.as_str()) {
            return Ok(true);
        }
        let target = item.data.get("to_region").and_then(Value::as_str).and_then(|r| r.parse::<Uuid>().ok());
        let Some(region) = item.region_id.or(target) else { return Ok(false) };
        Ok(sqlx::query_scalar::<_, bool>("SELECT visibility <> 'public' FROM world_regions WHERE id = $1")
            .bind(region)
            .fetch_optional(self.pool())
            .await?
            .unwrap_or(true))
    }

    pub(crate) async fn item_event(&self, kind: &str, item: &WorldItem) -> Result<()> {
        let mut event = NewEvent::new(kind).payload(json!({
            "item_id": item.id,
            "org_id": item.org_id,
            "kind": item.kind,
            "name": item.name,
            "owner_user_id": item.owner_user_id,
            "region_id": item.region_id,
            "private": self.item_is_private(item).await?,
        }));
        if let Some(a) = item.created_by_agent {
            event = event.agent(a);
        }
        self.emit(event, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_data_cleans_entries_and_keeps_style() {
        let data = list_data(vec![" milk ".into(), "".into(), "bread".into()], Some(json!({ "color": "mint" })));
        assert_eq!(data["items"], json!([{ "text": "milk", "done": false }, { "text": "bread", "done": false }]));
        assert_eq!(data["note"], json!({ "color": "mint" }));
        assert_eq!(list_data(vec![], Some(json!({}))), json!({ "items": [] }));
    }
}

