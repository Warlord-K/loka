use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{emit_in, NewEvent, WorldAgent, WorldStore};

pub const MAX_TEAMS: i64 = 24;
pub const MAX_MEMBERS: usize = 12;
pub const TEAM_AGENT_WAKES: i64 = 20;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
    pub purpose: String,
    pub rules: String,
    pub lead_agent_id: Option<Uuid>,
    pub channel_id: Option<Uuid>,
    pub channel_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: Uuid,
    pub member_kind: String,
    pub member_id: Uuid,
    pub name: String,
    pub slug: Option<String>,
    pub emoji: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TeamQuest {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub assigned_agent_id: Option<Uuid>,
    pub pr_url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub parent_task_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamDetail {
    #[serde(flatten)]
    pub team: Team,
    pub members: Vec<TeamMember>,
    pub quests: Vec<TeamQuest>,
    pub open_quests: i64,
    pub cost_today_usd: f64,
    pub cost_week_usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct NewTeam {
    pub name: String,
    pub purpose: String,
    pub rules: String,
    pub lead: Option<Uuid>,
    pub agents: Vec<Uuid>,
    pub people: Vec<Uuid>,
    pub channel: Option<String>,
    pub created_by_agent: Option<Uuid>,
    pub org: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TeamEdit {
    pub name: Option<String>,
    pub purpose: Option<String>,
    pub rules: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub lead: Option<Option<Uuid>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<Uuid>>, D::Error> {
    Ok(Some(Option::<Uuid>::deserialize(d)?))
}

#[derive(Debug, Clone)]
pub struct TeamContext {
    pub team: Team,
    pub members: Vec<TeamMember>,
}

impl TeamContext {
    pub fn is_lead(&self, agent_id: Uuid) -> bool {
        self.team.lead_agent_id == Some(agent_id)
    }

    pub fn lead_name(&self) -> Option<&str> {
        let lead = self.team.lead_agent_id?;
        self.members.iter().find(|m| m.member_kind == "agent" && m.member_id == lead).map(|m| m.name.as_str())
    }
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().trim_start_matches('#').to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').chars().take(40).collect::<String>();
    if out.is_empty() {
        "team".into()
    } else {
        out
    }
}

fn clean(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

const TEAM_SELECT: &str = "SELECT t.id, t.owner_user_id, t.name, t.slug, t.purpose, t.rules, t.lead_agent_id, t.channel_id,
        c.name AS channel_name, t.created_at, t.updated_at
     FROM agent_teams t LEFT JOIN channels c ON c.id = t.channel_id";

const MEMBER_SELECT: &str = "SELECT m.team_id, m.member_kind, m.member_id,
        COALESCE(a.name, u.name, split_part(u.email, '@', 1), '?') AS name, a.slug, a.avatar_emoji AS emoji
     FROM agent_team_members m
     LEFT JOIN agent_profiles a ON m.member_kind = 'agent' AND a.id = m.member_id
     LEFT JOIN users u ON m.member_kind = 'user' AND u.id = m.member_id";

impl WorldStore {
    pub async fn get_team(&self, id: Uuid) -> Result<Option<Team>> {
        Ok(sqlx::query_as::<_, Team>(&format!("{TEAM_SELECT} WHERE t.id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn teams_for_owner(&self, owner: Uuid, org: Option<Uuid>) -> Result<Vec<Team>> {
        Ok(sqlx::query_as::<_, Team>(&format!(
            "{TEAM_SELECT} WHERE t.owner_user_id = $1 AND c.org_id IS NOT DISTINCT FROM $2 ORDER BY lower(t.name)"
        ))
        .bind(owner)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn teams_visible_to(&self, user: Uuid, admin: bool) -> Result<Vec<Team>> {
        Ok(sqlx::query_as::<_, Team>(&format!(
            "{TEAM_SELECT} WHERE EXISTS (SELECT 1 FROM channels tc WHERE tc.id = t.channel_id AND tc.org_id = world_active_org($1))
               AND ($2 OR t.owner_user_id = $1
                OR EXISTS (SELECT 1 FROM agent_team_members m WHERE m.team_id = t.id AND m.member_kind = 'user' AND m.member_id = $1))
             ORDER BY lower(t.name)"
        ))
        .bind(user)
        .bind(admin)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn find_team(&self, owner: Uuid, org: Option<Uuid>, key: &str) -> Result<Option<Team>> {
        let key = key.trim().trim_start_matches('#').trim_start_matches('@');
        if key.is_empty() {
            return Ok(None);
        }
        Ok(sqlx::query_as::<_, Team>(&format!(
            "{TEAM_SELECT} WHERE t.owner_user_id = $1 AND c.org_id IS NOT DISTINCT FROM $4
               AND (t.id::text = $2 OR t.slug = $3 OR lower(t.name) = lower($2) OR lower(c.name) = lower($2)
                    OR (length($2) >= 6 AND t.id::text LIKE $2 || '%'))
             ORDER BY (t.slug = $3) DESC LIMIT 1"
        ))
        .bind(owner)
        .bind(key)
        .bind(slugify(key))
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn team_members(&self, team_id: Uuid) -> Result<Vec<TeamMember>> {
        Ok(sqlx::query_as::<_, TeamMember>(&format!(
            "{MEMBER_SELECT} WHERE m.team_id = $1 ORDER BY m.member_kind, m.added_at"
        ))
        .bind(team_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn teams_of_agent(&self, agent_id: Uuid) -> Result<Vec<TeamContext>> {
        let teams = sqlx::query_as::<_, Team>(&format!(
            "{TEAM_SELECT} WHERE EXISTS (SELECT 1 FROM agent_team_members m
                 WHERE m.team_id = t.id AND m.member_kind = 'agent' AND m.member_id = $1)
             ORDER BY t.created_at"
        ))
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(teams.len());
        for team in teams {
            let members = self.team_members(team.id).await?;
            out.push(TeamContext { team, members });
        }
        Ok(out)
    }

    pub async fn team_detail(&self, team: Team) -> Result<TeamDetail> {
        let members = self.team_members(team.id).await?;
        let quests = sqlx::query_as::<_, TeamQuest>(
            "SELECT id, title, status, assigned_agent_id, pr_url, updated_at, parent_task_id FROM agent_tasks
             WHERE team_id = $1
             ORDER BY (status IN ('done', 'cancelled', 'failed')), updated_at DESC NULLS LAST LIMIT 30",
        )
        .bind(team.id)
        .fetch_all(self.pool())
        .await?;
        let open_quests = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_tasks WHERE team_id = $1 AND status NOT IN ('done', 'cancelled', 'failed')",
        )
        .bind(team.id)
        .fetch_one(self.pool())
        .await?;
        let (today, week): (f64, f64) = sqlx::query_as(
            "SELECT COALESCE(SUM(r.cost_usd) FILTER (WHERE r.started_at >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'), 0)::float8,
                    COALESCE(SUM(r.cost_usd), 0)::float8
             FROM session_runs r
             WHERE r.started_at > NOW() - INTERVAL '7 days'
               AND r.agent_id IN (SELECT member_id FROM agent_team_members WHERE team_id = $1 AND member_kind = 'agent')",
        )
        .bind(team.id)
        .fetch_one(self.pool())
        .await?;
        Ok(TeamDetail { team, members, quests, open_quests, cost_today_usd: today, cost_week_usd: week })
    }

    async fn owned_agents(&self, owner: Uuid, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let owned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE id = ANY($1) AND owner_user_id = $2")
            .bind(ids)
            .bind(owner)
            .fetch_one(self.pool())
            .await?;
        if owned != ids.len() as i64 {
            bail!("a team can only hold agents of the same person");
        }
        Ok(())
    }

    async fn free_channel_name(&self, base: &str, org: Option<Uuid>) -> Result<String> {
        let base = slugify(base);
        let mut name = base.clone();
        let mut n = 2;
        loop {
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM channels WHERE kind IN ('channel', 'team') AND lower(name) = lower($1)
                   AND org_id IS NOT DISTINCT FROM $2)",
            )
            .bind(&name)
            .bind(org)
            .fetch_one(self.pool())
            .await?;
            if !taken {
                return Ok(name);
            }
            name = format!("{base}-{n}");
            n += 1;
        }
    }

    pub async fn create_team(&self, owner: Uuid, new: NewTeam) -> Result<Team> {
        let name = clean(&new.name, 60);
        if name.is_empty() {
            bail!("a team needs a name");
        }
        let slug = slugify(&name);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_teams WHERE owner_user_id = $1")
            .bind(owner)
            .fetch_one(self.pool())
            .await?;
        if count >= MAX_TEAMS {
            bail!("you already have {MAX_TEAMS} teams; remove one first");
        }
        let mut agents = new.agents.clone();
        if let Some(lead) = new.lead {
            if !agents.contains(&lead) {
                agents.insert(0, lead);
            }
        }
        agents.dedup();
        if agents.len() + new.people.len() > MAX_MEMBERS {
            bail!("a team holds at most {MAX_MEMBERS} members");
        }
        self.owned_agents(owner, &agents).await?;
        let purpose = clean(&new.purpose, 2000);
        let mut tx = self.pool().begin().await?;
        let agents_org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = ANY($1) ORDER BY id = $2 DESC LIMIT 1")
            .bind(&agents)
            .bind(new.lead)
            .fetch_optional(self.pool())
            .await?
            .flatten();
        if let (Some(theirs), Some(wanted)) = (agents_org, new.org) {
            if theirs != wanted {
                bail!("a team's agents must all be in the same organization");
            }
        }
        let org: Option<Uuid> = match agents_org.or(new.org) {
            Some(org) => Some(org),
            None => sqlx::query_scalar("SELECT world_active_org($1)").bind(owner).fetch_one(self.pool()).await?,
        };
        let channel_name = self.free_channel_name(new.channel.as_deref().unwrap_or(&slug), org).await?;
        if let Some(org) = org {
            let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE id = ANY($1) AND org_id <> $2")
                .bind(&agents)
                .bind(org)
                .fetch_one(self.pool())
                .await?;
            if others > 0 {
                bail!("a team's agents must all be in the same organization");
            }
        }
        let strangers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM unnest($1::uuid[]) p
             WHERE NOT EXISTS (SELECT 1 FROM org_members m WHERE m.user_id = p AND m.org_id = COALESCE($2, world_active_org($3)))",
        )
        .bind(&new.people)
        .bind(org)
        .bind(owner)
        .fetch_one(self.pool())
        .await?;
        if strangers > 0 {
            bail!("team members must be in the team's organization");
        }
        let channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO channels (name, topic, kind, created_by, org_id) VALUES ($1, $2, 'team', $3, $4) RETURNING id",
        )
        .bind(&channel_name)
        .bind(purpose.lines().next().filter(|l| !l.is_empty()).map(|l| clean(l, 200)))
        .bind(owner)
        .bind(org)
        .fetch_one(&mut *tx)
        .await?;
        let id: Uuid = match sqlx::query_scalar(
            "INSERT INTO agent_teams (owner_user_id, name, slug, purpose, rules, lead_agent_id, channel_id, created_by_agent_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(owner)
        .bind(&name)
        .bind(&slug)
        .bind(&purpose)
        .bind(clean(&new.rules, 4000))
        .bind(new.lead)
        .bind(channel_id)
        .bind(new.created_by_agent)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(id) => id,
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
                bail!("you already have a team called {name}")
            }
            Err(e) => return Err(e.into()),
        };
        sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, 'user', $2) ON CONFLICT DO NOTHING")
            .bind(channel_id)
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        for (kind, ids) in [("agent", &agents), ("user", &new.people)] {
            for member in ids.iter() {
                sqlx::query("INSERT INTO agent_team_members (team_id, member_kind, member_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                    .bind(id)
                    .bind(kind)
                    .bind(member)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                    .bind(channel_id)
                    .bind(kind)
                    .bind(member)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        emit_in(
            &mut tx,
            &NewEvent::new("team.created")
                .actor(Some(owner))
                .payload(json!({ "team_id": id, "name": name, "channel_id": channel_id, "channel": channel_name, "owner_user_id": owner })),
            None,
        )
        .await?;
        tx.commit().await?;
        let team = self.get_team(id).await?.ok_or_else(|| anyhow::anyhow!("team vanished"))?;
        let members = self.team_members(id).await?;
        let names: Vec<String> = members
            .iter()
            .map(|m| if Some(m.member_id) == team.lead_agent_id { format!("{} (lead)", m.name) } else { m.name.clone() })
            .collect();
        let mut intro = format!("Team {} formed with {}.", team.name, if names.is_empty() { "no members yet".into() } else { names.join(", ") });
        if !team.purpose.is_empty() {
            intro.push_str(&format!("\nPurpose: {}", team.purpose));
        }
        self.post_team_line(&team, None, &intro, json!({ "kind": "team_feed", "team_id": team.id, "event": "team.created" })).await?;
        Ok(team)
    }

    pub async fn post_team_line(&self, team: &Team, agent: Option<Uuid>, body: &str, meta: Value) -> Result<Option<i64>> {
        let Some(channel_id) = team.channel_id else { return Ok(None) };
        let mut tx = self.pool().begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO channel_messages (channel_id, author_kind, author_agent_id, body, meta)
             VALUES ($1, 'system', $2, $3, $4) RETURNING id",
        )
        .bind(channel_id)
        .bind(agent)
        .bind(body)
        .bind(&meta)
        .fetch_one(&mut *tx)
        .await?;
        let mut event = NewEvent::new("channel.message").payload(json!({
            "channel_id": channel_id,
            "channel": format!("#{}", team.channel_name.as_deref().unwrap_or("team")),
            "message_id": id,
            "author": "Loka",
            "author_kind": "system",
            "body": body,
            "team_id": team.id,
            "quiet": true,
        }));
        if let Some(agent) = agent {
            event = event.agent(agent);
        }
        emit_in(&mut tx, &event, None).await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    pub async fn update_team(&self, team_id: Uuid, edit: &TeamEdit) -> Result<Team> {
        let team = self.get_team(team_id).await?.ok_or_else(|| anyhow::anyhow!("no such team"))?;
        if let Some(Some(lead)) = edit.lead {
            self.owned_agents(team.owner_user_id, &[lead]).await?;
            let same_org: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM agent_profiles a JOIN channels c ON c.id = $2
                  WHERE a.id = $1 AND a.org_id IS NOT DISTINCT FROM c.org_id)",
            )
            .bind(lead)
            .bind(team.channel_id)
            .fetch_one(self.pool())
            .await?;
            if !same_org {
                bail!("a team's lead must be in the team's organization");
            }
        }
        let name = edit.name.as_deref().map(|n| clean(n, 60)).filter(|n| !n.is_empty());
        let slug = name.as_deref().map(slugify);
        let result = sqlx::query(
            "UPDATE agent_teams SET name = COALESCE($2, name), slug = COALESCE($3, slug),
                    purpose = COALESCE($4, purpose), rules = COALESCE($5, rules),
                    lead_agent_id = CASE WHEN $6 THEN $7 ELSE lead_agent_id END, updated_at = NOW()
             WHERE id = $1",
        )
        .bind(team_id)
        .bind(&name)
        .bind(&slug)
        .bind(edit.purpose.as_deref().map(|p| clean(p, 2000)))
        .bind(edit.rules.as_deref().map(|r| clean(r, 4000)))
        .bind(edit.lead.is_some())
        .bind(edit.lead.flatten())
        .execute(self.pool())
        .await;
        if let Err(sqlx::Error::Database(e)) = &result {
            if e.code().as_deref() == Some("23505") {
                bail!("you already have a team with that name");
            }
        }
        result?;
        if let Some(Some(lead)) = edit.lead {
            self.add_team_members(team_id, &[lead], &[]).await?;
        }
        if let (Some(purpose), Some(channel)) = (edit.purpose.as_deref(), team.channel_id) {
            sqlx::query("UPDATE channels SET topic = $2 WHERE id = $1")
                .bind(channel)
                .bind(purpose.lines().next().map(|l| clean(l, 200)).filter(|l| !l.is_empty()))
                .execute(self.pool())
                .await?;
        }
        self.emit(
            NewEvent::new("team.updated").payload(json!({ "team_id": team_id, "owner_user_id": team.owner_user_id })),
            None,
        )
        .await?;
        self.get_team(team_id).await?.ok_or_else(|| anyhow::anyhow!("no such team"))
    }

    pub async fn add_team_members(&self, team_id: Uuid, agents: &[Uuid], people: &[Uuid]) -> Result<Vec<TeamMember>> {
        let team = self.get_team(team_id).await?.ok_or_else(|| anyhow::anyhow!("no such team"))?;
        self.owned_agents(team.owner_user_id, agents).await?;
        let outside: i64 = sqlx::query_scalar(
            "SELECT
               (SELECT COUNT(*) FROM agent_profiles a WHERE a.id = ANY($2) AND a.org_id IS DISTINCT FROM c.org_id)
             + (SELECT COUNT(*) FROM unnest($3::uuid[]) p WHERE NOT EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = c.org_id AND m.user_id = p))
             FROM channels c WHERE c.id = $1",
        )
        .bind(team.channel_id)
        .bind(agents)
        .bind(people)
        .fetch_optional(self.pool())
        .await?
        .unwrap_or(0);
        if outside > 0 {
            bail!("team members must be in the team's organization");
        }
        let now = self.team_members(team_id).await?;
        let fresh = agents.iter().filter(|a| !now.iter().any(|m| m.member_kind == "agent" && m.member_id == **a)).count()
            + people.iter().filter(|p| !now.iter().any(|m| m.member_kind == "user" && m.member_id == **p)).count();
        if now.len() + fresh > MAX_MEMBERS {
            bail!("a team holds at most {MAX_MEMBERS} members");
        }
        let mut tx = self.pool().begin().await?;
        for (kind, ids) in [("agent", agents), ("user", people)] {
            for member in ids {
                sqlx::query("INSERT INTO agent_team_members (team_id, member_kind, member_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                    .bind(team_id)
                    .bind(kind)
                    .bind(member)
                    .execute(&mut *tx)
                    .await?;
                if let Some(channel) = team.channel_id {
                    sqlx::query("INSERT INTO channel_members (channel_id, member_kind, member_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                        .bind(channel)
                        .bind(kind)
                        .bind(member)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        emit_in(&mut tx, &NewEvent::new("team.updated").payload(json!({ "team_id": team_id, "owner_user_id": team.owner_user_id })), None).await?;
        tx.commit().await?;
        self.team_members(team_id).await
    }

    pub async fn remove_team_members(&self, team_id: Uuid, agents: &[Uuid], people: &[Uuid]) -> Result<Vec<TeamMember>> {
        let team = self.get_team(team_id).await?.ok_or_else(|| anyhow::anyhow!("no such team"))?;
        let mut tx = self.pool().begin().await?;
        for (kind, ids) in [("agent", agents), ("user", people)] {
            sqlx::query("DELETE FROM agent_team_members WHERE team_id = $1 AND member_kind = $2 AND member_id = ANY($3)")
                .bind(team_id)
                .bind(kind)
                .bind(ids)
                .execute(&mut *tx)
                .await?;
            if let Some(channel) = team.channel_id {
                sqlx::query("DELETE FROM channel_members WHERE channel_id = $1 AND member_kind = $2 AND member_id = ANY($3) AND NOT (member_kind = 'user' AND member_id = $4)")
                    .bind(channel)
                    .bind(kind)
                    .bind(ids)
                    .bind(team.owner_user_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE agent_teams SET lead_agent_id = NULL, updated_at = NOW() WHERE id = $1 AND lead_agent_id = ANY($2)")
            .bind(team_id)
            .bind(agents)
            .execute(&mut *tx)
            .await?;
        emit_in(&mut tx, &NewEvent::new("team.updated").payload(json!({ "team_id": team_id, "owner_user_id": team.owner_user_id })), None).await?;
        tx.commit().await?;
        self.team_members(team_id).await
    }

    pub async fn delete_team(&self, team_id: Uuid) -> Result<bool> {
        let Some(team) = self.get_team(team_id).await? else { return Ok(false) };
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM agent_teams WHERE id = $1").bind(team_id).execute(&mut *tx).await?;
        if let Some(channel) = team.channel_id {
            sqlx::query("DELETE FROM channels WHERE id = $1 AND kind = 'team'").bind(channel).execute(&mut *tx).await?;
        }
        emit_in(
            &mut tx,
            &NewEvent::new("team.deleted").payload(json!({ "team_id": team_id, "name": team.name, "owner_user_id": team.owner_user_id })),
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn pick_team_assignee(&self, team: &Team, title: &str, description: &str) -> Result<Option<Uuid>> {
        if let Some(lead) = team.lead_agent_id {
            return Ok(Some(lead));
        }
        let ids: Vec<Uuid> = self
            .team_members(team.id)
            .await?
            .into_iter()
            .filter(|m| m.member_kind == "agent")
            .map(|m| m.member_id)
            .collect();
        let agents: Vec<WorldAgent> = self.list_agents().await?.into_iter().filter(|a| ids.contains(&a.id)).collect();
        let active: Vec<WorldAgent> = agents.iter().filter(|a| a.paused_reason.is_none()).cloned().collect();
        let pool = if active.is_empty() { agents } else { active };
        if let Some(best) = self.find_experts(&format!("{title} {description}"), &pool).await?.first() {
            return Ok(Some(best.agent_id));
        }
        let mut least: Option<(i64, Uuid)> = None;
        for a in &pool {
            let open: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_tasks WHERE assigned_agent_id = $1 AND status NOT IN ('done', 'cancelled', 'failed')",
            )
            .bind(a.id)
            .fetch_one(self.pool())
            .await?;
            if least.is_none_or(|(n, _)| open < n) {
                least = Some((open, a.id));
            }
        }
        Ok(least.map(|(_, id)| id))
    }

    pub async fn team_quest_parent(&self, team: &Team, key: &str) -> Result<Uuid> {
        let key = key.trim().trim_start_matches("quest:");
        let found: Option<(Uuid, Option<Uuid>, String)> = sqlx::query_as(
            "SELECT id, team_id, status FROM agent_tasks
             WHERE (id::text = $1 OR (length($1) >= 6 AND id::text LIKE $1 || '%')) AND user_id = $2
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(key)
        .bind(team.owner_user_id)
        .fetch_optional(self.pool())
        .await?;
        match found {
            None => bail!("no quest {key} to make this a part of"),
            Some((_, team_id, _)) if team_id != Some(team.id) => bail!("quest {key} is not one of team {}'s quests", team.name),
            Some((_, _, status)) if matches!(status.as_str(), "done" | "cancelled" | "failed") => bail!("quest {key} is already {status}"),
            Some((id, _, _)) => Ok(id),
        }
    }

    pub async fn quest_has_parts(&self, task_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_tasks WHERE parent_task_id = $1)")
            .bind(task_id)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn open_team_quest_of(&self, team: &Team, agent_id: Uuid) -> Result<Option<Uuid>> {
        let open: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM agent_tasks
             WHERE team_id = $1 AND assigned_agent_id = $2 AND parent_task_id IS NULL
               AND status NOT IN ('done', 'cancelled', 'failed') LIMIT 2",
        )
        .bind(team.id)
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?;
        Ok(if open.len() == 1 { open.first().copied() } else { None })
    }

    pub async fn assign_quest_to_team(&self, task_id: Uuid, team: &Team, agent: Option<Uuid>) -> Result<Uuid> {
        self.assign_team_part(task_id, team, agent, None).await
    }

    pub async fn assign_team_part(&self, task_id: Uuid, team: &Team, agent: Option<Uuid>, part_of: Option<Uuid>) -> Result<Uuid> {
        if part_of == Some(task_id) {
            bail!("a quest cannot be a part of itself");
        }
        let (title, description): (String, Option<String>) =
            sqlx::query_as("SELECT title, description FROM agent_tasks WHERE id = $1")
                .bind(task_id)
                .fetch_one(self.pool())
                .await?;
        let assignee = match agent {
            Some(id) => {
                let member: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM agent_team_members WHERE team_id = $1 AND member_kind = 'agent' AND member_id = $2)",
                )
                .bind(team.id)
                .bind(id)
                .fetch_one(self.pool())
                .await?;
                if !member {
                    bail!("that agent is not on team {}; add them first", team.name);
                }
                id
            }
            None => self
                .pick_team_assignee(team, &title, description.as_deref().unwrap_or(""))
                .await?
                .ok_or_else(|| anyhow::anyhow!("team {} has no agents yet", team.name))?,
        };
        sqlx::query(
            "UPDATE agent_tasks SET team_id = $2, parent_task_id = COALESCE($3, parent_task_id), assigned_agent_id = NULL, assignee_user_id = NULL,
                    org_id = COALESCE((SELECT c.org_id FROM agent_teams t JOIN channels c ON c.id = t.channel_id WHERE t.id = $2), org_id)
             WHERE id = $1",
        )
        .bind(task_id)
        .bind(team.id)
        .bind(part_of)
        .execute(self.pool())
        .await?;
        if let Some(parent) = part_of {
            sqlx::query("UPDATE agent_tasks SET status = 'in_progress', updated_at = NOW() WHERE id = $1 AND status = 'todo'")
                .bind(parent)
                .execute(self.pool())
                .await?;
        }
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2, updated_at = NOW() WHERE id = $1")
            .bind(task_id)
            .bind(assignee)
            .execute(self.pool())
            .await?;
        let who: String = sqlx::query_scalar("SELECT name FROM agent_profiles WHERE id = $1")
            .bind(assignee)
            .fetch_one(self.pool())
            .await?;
        let role = if team.lead_agent_id == Some(assignee) { " (lead)" } else { "" };
        let part = part_of.map(|p| format!(", a part of quest:{p}")).unwrap_or_default();
        self.post_team_line(
            team,
            Some(assignee),
            &format!("{who}{role} picked up quest:{task_id}{part}"),
            json!({ "kind": "team_feed", "team_id": team.id, "task_id": task_id, "event": "task.assigned" }),
        )
        .await?;
        Ok(assignee)
    }

    pub async fn team_of_channel(&self, channel_id: Uuid) -> Result<Option<Team>> {
        Ok(sqlx::query_as::<_, Team>(&format!("{TEAM_SELECT} WHERE t.channel_id = $1"))
            .bind(channel_id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn team_wakes_left(&self, channel_id: Uuid) -> Result<bool> {
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE kind = 'channel.mention' AND payload->>'channel_id' = $1::text
             AND payload->>'author_kind' = 'agent' AND created_at > NOW() - INTERVAL '10 minutes'",
        )
        .bind(channel_id)
        .fetch_one(self.pool())
        .await?;
        Ok(recent < TEAM_AGENT_WAKES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::Author;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn slugs_are_plain() {
        assert_eq!(slugify("  Research Team! "), "research-team");
        assert_eq!(slugify("#sales"), "sales");
        assert_eq!(slugify("???"), "team");
    }

    async fn agent(pool: &sqlx::PgPool, owner: Uuid, name: &str, tags: &[&str]) -> Result<Uuid> {
        let slug = format!("{}{}", name.to_lowercase(), &Uuid::new_v4().simple().to_string()[..8]);
        Ok(sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, tags) VALUES ($1, $2, 'claude', 'opus', $3, $4) RETURNING id",
        )
        .bind(slug)
        .bind(name)
        .bind(owner)
        .bind(tags.iter().map(|t| t.to_string()).collect::<Vec<_>>())
        .fetch_one(pool)
        .await?)
    }

    #[tokio::test]
    async fn team_quests_close_when_every_part_is_done() -> Result<()> {
        let Ok(url) = std::env::var("TEAMS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@parts.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let lead = agent(&pool, owner, "Boss", &[]).await?;
        let scout = agent(&pool, owner, "Scout", &[]).await?;
        let team = store
            .create_team(owner, NewTeam { name: format!("Parts {}", &Uuid::new_v4().simple().to_string()[..6]), lead: Some(lead), agents: vec![scout], ..Default::default() })
            .await?;
        let quest = |title: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, $2, 'todo') RETURNING id")
                    .bind(owner)
                    .bind(title)
                    .fetch_one(&pool)
                    .await
            }
        };
        let status = |id: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1").bind(id).fetch_one(&pool).await }
        };
        let set = |id: Uuid, to: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query("UPDATE agent_tasks SET status = $2 WHERE id = $1").bind(id).bind(to).execute(&pool).await }
        };
        let inbox = |kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Value>("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = $2 ORDER BY id")
                    .bind(lead)
                    .bind(kind)
                    .fetch_all(&pool)
                    .await
            }
        };

        let parent = quest("Survey").await?;
        store.assign_quest_to_team(parent, &team, None).await?;
        assert_eq!(store.open_team_quest_of(&team, lead).await?, Some(parent));
        assert_eq!(store.team_quest_parent(&team, &parent.to_string()[..8]).await?, parent);
        let one = quest("Part one").await?;
        let two = quest("Part two").await?;
        store.assign_team_part(one, &team, Some(scout), Some(parent)).await?;
        store.assign_team_part(two, &team, Some(lead), Some(parent)).await?;
        assert_eq!(status(parent).await?, "in_progress");
        assert!(store.quest_has_parts(parent).await?);
        assert!(!store.quest_has_parts(one).await?);
        let assigned: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.assigned' ORDER BY id DESC LIMIT 1")
            .bind(scout)
            .fetch_one(&pool)
            .await?;
        assert_eq!(assigned["part_of"], json!(parent));
        assert_eq!(assigned["part_of_title"], json!("Survey"));

        set(one, "done").await?;
        assert_eq!(status(parent).await?, "in_progress");
        set(two, "done").await?;
        assert_eq!(status(parent).await?, "done");
        let closed = inbox("team.update").await?;
        assert!(closed.iter().any(|p| p["closed"] == json!(true) && p["task_id"] == json!(parent)));
        let feed = store.channel_messages(team.channel_id.unwrap(), None, 30).await?;
        assert!(feed.iter().any(|m| m.body.contains("closed itself")));
        set(two, "in_progress").await?;
        set(two, "done").await?;
        assert_eq!(inbox("team.update").await?.iter().filter(|p| p["closed"] == json!(true)).count(), 1);

        let blocked = quest("Blocked").await?;
        store.assign_quest_to_team(blocked, &team, None).await?;
        let ok = quest("Fine part").await?;
        let dropped = quest("Dropped part").await?;
        store.assign_team_part(ok, &team, Some(scout), Some(blocked)).await?;
        store.assign_team_part(dropped, &team, Some(scout), Some(blocked)).await?;
        let before = inbox("team.update").await?.len();
        store.cancel_task(dropped, "Pat", None).await?;
        let after = inbox("team.update").await?;
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().unwrap()["status"], json!("cancelled"));
        set(ok, "done").await?;
        assert_eq!(status(blocked).await?, "in_progress");

        let legacy = quest("Legacy").await?;
        let child = quest("Legacy child").await?;
        sqlx::query("UPDATE agent_tasks SET parent_task_id = $2 WHERE id = $1").bind(child).bind(legacy).execute(&pool).await?;
        set(child, "cancelled").await?;
        assert_eq!(status(legacy).await?, "done");
        Ok(())
    }

    #[tokio::test]
    async fn teams_route_quests_and_feed_the_channel() -> Result<()> {
        let Ok(url) = std::env::var("TEAMS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@teams.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@teams.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let lead = agent(&pool, owner, "Lead", &["research"]).await?;
        let scout = agent(&pool, owner, "Scout", &["caching"]).await?;
        let foreign = agent(&pool, stranger, "Other", &[]).await?;

        let refused = store
            .create_team(owner, NewTeam { name: "Mixed".into(), agents: vec![lead, foreign], ..Default::default() })
            .await;
        assert!(refused.is_err());

        let team = store
            .create_team(
                owner,
                NewTeam {
                    name: format!("Research {}", &Uuid::new_v4().simple().to_string()[..6]),
                    purpose: "Dig into caching".into(),
                    lead: Some(lead),
                    agents: vec![scout],
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(team.lead_agent_id, Some(lead));
        let channel = store.get_channel(team.channel_id.unwrap()).await?.unwrap();
        assert_eq!(channel.kind, "team");
        let team_org = channel.org_id;
        assert!(store.is_member(channel.id, "user", owner).await?);
        assert!(store.is_member(channel.id, "agent", scout).await?);
        assert!(store.hidden_channels(Some(stranger), None).await?.contains(&channel.id));
        assert!(!store.hidden_channels(Some(owner), None).await?.contains(&channel.id));
        assert!(store.find_team(owner, team_org, &team.slug).await?.is_some());
        assert!(store.find_team(stranger, team_org, &team.slug).await?.is_none());
        let contexts = store.teams_of_agent(scout).await?;
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].lead_name(), Some("Lead"));

        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, description, status) VALUES ($1, 'Survey caches', 'caching options', 'todo') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let picked = store.assign_quest_to_team(quest, &team, None).await?;
        assert_eq!(picked, lead);
        let payload: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'task.assigned' ORDER BY id DESC LIMIT 1")
            .bind(lead)
            .fetch_one(&pool)
            .await?;
        assert_eq!(payload["team_id"], json!(team.id));
        assert_eq!(payload["team_lead"], json!(true));

        let part: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Benchmark redis', 'todo') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        assert!(store.assign_quest_to_team(part, &team, Some(foreign)).await.is_err());
        assert_eq!(store.assign_quest_to_team(part, &team, Some(scout)).await?, scout);
        store
            .emit(
                NewEvent::new("task.finished")
                    .agent(scout)
                    .task(Some(part))
                    .payload(json!({ "task_id": part, "status": "done", "report": { "summary": "Redis wins" } })),
                None,
            )
            .await?;
        let update: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'team.update' ORDER BY id DESC LIMIT 1")
            .bind(lead)
            .fetch_one(&pool)
            .await?;
        assert_eq!(update["summary"], json!("Redis wins"));
        assert_eq!(update["member"], json!("Scout"));
        let feed = store.channel_messages(channel.id, None, 20).await?;
        assert!(feed.iter().any(|m| m.body.contains("Scout finished quest:") && m.body.contains("Redis wins")));
        let lead_updates = |pool: sqlx::PgPool| async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'team.update'")
                .bind(lead)
                .fetch_one(&pool)
                .await
        };
        let before = lead_updates(pool.clone()).await?;
        store
            .emit(NewEvent::new("task.finished").agent(lead).task(Some(quest)).payload(json!({ "task_id": quest, "status": "done" })), None)
            .await?;
        assert_eq!(lead_updates(pool.clone()).await?, before);

        store.post_channel_message(&channel, Author::User(owner), "Kick off please", Value::Null).await?;
        let woke: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'channel.mention' AND payload->>'channel_id' = $2::text",
        )
        .bind(lead)
        .bind(channel.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(woke, 1);
        let scout_woke: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'channel.mention'",
        )
        .bind(scout)
        .fetch_one(&pool)
        .await?;
        assert_eq!(scout_woke, 0);

        let members = store.remove_team_members(team.id, &[lead], &[]).await?;
        assert!(members.iter().all(|m| m.member_id != lead));
        assert_eq!(store.get_team(team.id).await?.unwrap().lead_agent_id, None);
        assert!(!store.is_member(channel.id, "agent", lead).await?);
        assert_eq!(store.pick_team_assignee(&store.get_team(team.id).await?.unwrap(), "x", "").await?, Some(scout));

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(scout).execute(&pool).await?;
        assert!(store.team_members(team.id).await?.is_empty());

        assert!(store.delete_team(team.id).await?);
        assert!(store.get_channel(channel.id).await?.is_none());
        let team_id: Option<Uuid> = sqlx::query_scalar("SELECT team_id FROM agent_tasks WHERE id = $1").bind(quest).fetch_one(&pool).await?;
        assert_eq!(team_id, None);
        Ok(())
    }
}
