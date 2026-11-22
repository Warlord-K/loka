use anyhow::{anyhow, bail, Result};
use loka_shared::teams::{NewTeam, Team, TeamDetail, TeamEdit};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub const TEAM_TOOLS: &[&str] = &["list_teams", "create_team", "update_team", "delete_team", "add_team_members", "remove_team_members"];
pub const READ_TOOLS: &[&str] = &["list_teams"];

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value, manage: bool) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let names = |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let team = s("Team name, slug, channel name or id from list_teams");
    let mut out = vec![tool(
        "list_teams",
        "List your person's teams: purpose, lead, members, team channel, open quests and what the team spent today and this week.",
        json!({ "team": s("Show only this team, optional") }),
        &[],
    )];
    if !manage {
        return out;
    }
    out.extend([
        tool(
            "create_team",
            "Form a team of your person's agents (and optionally people) that works together in its own team channel, created with it. Members see the team's purpose, lead and rules in their instructions. Quests given to the team (create_task team) go to the lead, or to the best fit member when there is no lead.",
            json!({
                "name": s("Team name, like Research or Sales"),
                "purpose": s("What the team is for, a sentence or two"),
                "lead": s("Agent name or slug who leads: picks up team quests and splits them, optional"),
                "agents": names("Agents on the team, by name or slug"),
                "people": names("People to add, by name or @handle, optional"),
                "channel": s("Team channel name, optional, defaults to the team name"),
                "rules": s("Team rules every member follows, optional, one per line")
            }),
            &["name"],
        ),
        tool(
            "update_team",
            "Rename a team, rewrite its purpose or rules, or change its lead (lead \"none\" leaves it without one).",
            json!({ "team": team.clone(), "name": s("New name"), "purpose": s("New purpose"), "rules": s("New rules, replaces the old ones; empty clears them"), "lead": s("Agent name or slug, or none") }),
            &["team"],
        ),
        tool(
            "delete_team",
            "Remove a team and its team channel for good. Its agents and quests stay; the quests just stop belonging to a team. Only when the person asks.",
            json!({ "team": team.clone() }),
            &["team"],
        ),
        tool(
            "add_team_members",
            "Add agents or people to a team; they join its channel too.",
            json!({ "team": team.clone(), "agents": names("Agents by name or slug"), "people": names("People by name or @handle") }),
            &["team"],
        ),
        tool(
            "remove_team_members",
            "Take agents or people off a team and its channel. Removing the lead leaves the team without one.",
            json!({ "team": team, "agents": names("Agents by name or slug"), "people": names("People by name or @handle") }),
            &["team"],
        ),
    ]);
    out
}

fn list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

pub async fn resolve_team(store: &WorldStore, owner: Uuid, org: Option<Uuid>, key: &str) -> Result<Team> {
    store
        .find_team(owner, org, key)
        .await?
        .ok_or_else(|| anyhow!("no team called {key}; list_teams shows your person's teams"))
}

async fn own_agent(store: &WorldStore, owner: Uuid, org: Option<Uuid>, name: &str) -> Result<WorldAgent> {
    store
        .find_agent_in(org, name)
        .await?
        .filter(|a| a.owner_user_id == Some(owner))
        .ok_or_else(|| anyhow!("no agent called {name} works for your person; use world_agents for exact handles"))
}

async fn people(store: &WorldStore, org: Option<Uuid>, wanted: &[String]) -> Result<Vec<Uuid>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let everyone = store.people_in(org.unwrap_or_default()).await?;
    let handle = |t: &str| t.trim().trim_start_matches('@').chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let mut out = Vec::new();
    for w in wanted {
        let key = handle(w);
        let hits: Vec<_> = everyone
            .iter()
            .filter(|p| {
                p.name.as_deref().is_some_and(|n| handle(n) == key)
                    || p.email.as_deref().and_then(|e| e.split('@').next()).is_some_and(|l| handle(l) == key)
            })
            .collect();
        match hits.as_slice() {
            [p] => out.push(p.id),
            [] => bail!("nobody called {w} uses Loka"),
            _ => bail!("more than one person is called {w}; ask which"),
        }
    }
    Ok(out)
}

async fn agents(store: &WorldStore, owner: Uuid, org: Option<Uuid>, wanted: &[String]) -> Result<Vec<Uuid>> {
    let mut out = Vec::new();
    for w in wanted {
        let id = own_agent(store, owner, org, w).await?.id;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn describe(d: &TeamDetail) -> String {
    let lead = d.team.lead_agent_id;
    let members: Vec<String> = d
        .members
        .iter()
        .map(|m| {
            let tag = if Some(m.member_id) == lead { " (lead)" } else if m.member_kind == "user" { " (person)" } else { "" };
            format!("{}{}{tag}", m.name, m.slug.as_deref().map(|s| format!(" @{s}")).unwrap_or_default())
        })
        .collect();
    let mut lines = vec![format!(
        "- {} ({}), channel #{}, {} open quests, spent ${:.2} today and ${:.2} this week",
        d.team.name,
        &d.team.id.to_string()[..8],
        d.team.channel_name.as_deref().unwrap_or("none"),
        d.open_quests,
        d.cost_today_usd,
        d.cost_week_usd
    )];
    if !d.team.purpose.is_empty() {
        lines.push(format!("  purpose: {}", d.team.purpose));
    }
    lines.push(format!("  members: {}", if members.is_empty() { "none".into() } else { members.join(", ") }));
    if !d.team.rules.is_empty() {
        lines.push(format!("  rules: {}", d.team.rules.replace('\n', "; ")));
    }
    for q in d.quests.iter().filter(|q| !matches!(q.status.as_str(), "done" | "cancelled" | "failed")).take(6) {
        lines.push(format!("  quest {} [{}] {}", &q.id.to_string()[..8], q.status, q.title));
    }
    lines.join("\n")
}

fn guard_rules_and_people(caller: &WorldAgent, rules: bool, people: bool) -> Result<()> {
    if (rules || people) && !caller.has_power(loka_shared::powers::MANAGE_POWERS) {
        bail!("setting team rules or adding people needs the manage_powers power, like writing your person's rules; ask your person");
    }
    Ok(())
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, name: &str, args: &Value) -> Result<String> {
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("teams belong to a person and you have none"))?;
    let org = caller.org_id;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let need = |k: &str| arg(k).ok_or_else(|| anyhow!("{k} is required"));
    match name {
        "list_teams" => {
            let teams = match arg("team") {
                Some(key) => vec![resolve_team(store, owner, org, key).await?],
                None => store.teams_for_owner(owner, org).await?,
            };
            if teams.is_empty() {
                return Ok("Your person has no teams yet.".into());
            }
            let mut out = Vec::new();
            for team in teams {
                out.push(describe(&store.team_detail(team).await?));
            }
            Ok(out.join("\n"))
        }
        "create_team" => {
            guard_rules_and_people(caller, arg("rules").is_some(), !list(args, "people").is_empty())?;
            let lead = match arg("lead") {
                Some(l) => Some(own_agent(store, owner, org, l).await?.id),
                None => None,
            };
            let team = store
                .create_team(
                    owner,
                    NewTeam {
                        name: need("name")?.to_string(),
                        purpose: arg("purpose").unwrap_or("").to_string(),
                        rules: arg("rules").unwrap_or("").to_string(),
                        lead,
                        agents: agents(store, owner, org, &list(args, "agents")).await?,
                        people: people(store, caller.org_id, &list(args, "people")).await?,
                        channel: arg("channel").map(String::from),
                        created_by_agent: Some(caller.id),
                        org,
                    },
                )
                .await?;
            let heading = format!("Formed team {} with its channel #{}.", team.name, team.channel_name.as_deref().unwrap_or("?"));
            Ok(format!("{heading}\n{}", describe(&store.team_detail(team).await?)))
        }
        "update_team" => {
            guard_rules_and_people(caller, args.get("rules").is_some(), false)?;
            let team = resolve_team(store, owner, org, need("team")?).await?;
            let lead = match arg("lead") {
                Some(l) if l.eq_ignore_ascii_case("none") => Some(None),
                Some(l) => Some(Some(own_agent(store, owner, org, l).await?.id)),
                None => None,
            };
            let edit = TeamEdit {
                name: arg("name").map(String::from),
                purpose: args.get("purpose").and_then(Value::as_str).map(String::from),
                rules: args.get("rules").and_then(Value::as_str).map(String::from),
                lead,
                ..Default::default()
            };
            if edit.name.is_none() && edit.purpose.is_none() && edit.rules.is_none() && edit.lead.is_none() {
                bail!("give name, purpose, rules or lead");
            }
            let team = store.update_team(team.id, &edit).await?;
            Ok(format!("Updated team {}.", team.name))
        }
        "delete_team" => {
            let team = resolve_team(store, owner, org, need("team")?).await?;
            store.delete_team(team.id).await?;
            Ok(format!("Removed team {} and its channel.", team.name))
        }
        "add_team_members" | "remove_team_members" => {
            guard_rules_and_people(caller, false, name == "add_team_members" && !list(args, "people").is_empty())?;
            let team = resolve_team(store, owner, org, need("team")?).await?;
            let agents = agents(store, owner, org, &list(args, "agents")).await?;
            let people = people(store, caller.org_id, &list(args, "people")).await?;
            if agents.is_empty() && people.is_empty() {
                bail!("give agents or people");
            }
            let members = if name == "add_team_members" {
                store.add_team_members(team.id, &agents, &people).await?
            } else {
                store.remove_team_members(team.id, &agents, &people).await?
            };
            Ok(format!(
                "Team {} now has: {}.",
                team.name,
                if members.is_empty() { "nobody".into() } else { members.iter().map(|m| m.name.clone()).collect::<Vec<_>>().join(", ") }
            ))
        }
        other => bail!("unknown team tool {other}"),
    }
}
