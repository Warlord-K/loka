use anyhow::{anyhow, bail, Result};
use loka_shared::orgs::OrgRole;
use loka_shared::powers;
use loka_shared::world::{AgentUpdate, NewEvent, WorldAgent, APPROVAL_MODES, DEFAULT_MAIN_SUBSCRIPTIONS};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{slugify, Server};

pub(super) const GOVERN_TOOLS: &[&str] = &[
    "pause_agent",
    "resume_agent",
    "archive_agent",
    "unarchive_agent",
    "set_budget",
    "set_approval_mode",
    "fork_agent",
    "make_main",
];

const OWNER_ONLY: &str = "only when your verified owner asks for it in this turn";
const LOCKED_PAUSES: &[&str] = &["Organization ", "Owner left", "Owner account disabled", loka_shared::archive::ARCHIVED];

pub(super) fn locked_pause(reason: &str) -> bool {
    LOCKED_PAUSES.iter().any(|p| reason.starts_with(p))
}

impl Server {
    pub(super) fn govern_tools(&self, tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
        let s = |d: &str| json!({ "type": "string", "description": d });
        let mut tools = Vec::new();
        if self.caller.owner_user_id.is_none() {
            return tools;
        }
        if self.can(powers::GOVERN) {
            tools.push(tool("pause_agent", &format!("Pause one of your person's agents: it stops taking turns and starting work until resumed, and queued work waits. Running work is not stopped (use stop_run). You cannot pause yourself. Use it {OWNER_ONLY}."), json!({
                "agent": s("Agent name or slug"),
                "reason": s("Why, optional; shown on the agent")
            }), &["agent"]));
            tools.push(tool("resume_agent", &format!("Resume one of your person's paused agents, whatever paused it (your person, a daily budget, a rejected model key), so it picks up its waiting messages and work. A budget pause comes back on its next run unless the budget is raised (set_budget). Pauses from the organization stay. Use it {OWNER_ONLY}."), json!({
                "agent": s("Agent name or slug")
            }), &["agent"]));
        }
        if self.can(powers::MANAGE_AGENTS) {
            tools.push(tool("archive_agent", &format!("Archive one of your person's agents they no longer need but do not want to delete yet: it stops working (like a pause), is hidden from the map, lists and counts, and keeps its memory, chats and kit. Optionally delete it for good after delete_after_days (off unless given). Calling it again on an archived agent changes or clears that date (delete_after_days 0 turns auto delete off). Main agents and you cannot be archived. Use it {OWNER_ONLY}; pause_agent is for a short break, remove_agent deletes at once."), json!({
                "agent": s("Agent name or slug"),
                "delete_after_days": { "type": "integer", "description": "Delete it permanently after this many days, 1 to 3650; 0 or omitted means never" }
            }), &["agent"]));
            tools.push(tool("unarchive_agent", &format!("Bring an archived agent back: it shows again and picks up its waiting work. Use it {OWNER_ONLY}."), json!({
                "agent": s("Agent name or slug")
            }), &["agent"]));
        }
        if self.can(powers::GOVERN) {
            tools.push(tool("set_budget", &format!("Set or clear the daily limits of one of your person's agents, or yourself: dollars and/or tokens per UTC day. Every CLI counts toward both (Claude, omp and OpenCode report dollars, Cursor and Codex are priced from list prices; tokens are new input plus output, cached reads not counted), and running work is stopped when a limit is hit. A token limit also covers models with no known price. At 80% your person is warned, at 100% the agent pauses until the next day. Raising or clearing a limit resumes an agent its budget paused. world_usage shows spend. Use it {OWNER_ONLY}."), json!({
                "agent": s("Agent name or slug, optional, defaults to you"),
                "daily_usd": { "type": "number", "description": "Limit in USD per day, 0 to 100000" },
                "daily_tokens": { "type": "integer", "description": "Limit in tokens per day (new input plus output), 1 to 10000000000" },
                "clear": { "type": "boolean", "description": "Remove both limits instead" }
            }), &[]));
            tools.push(tool("set_approval_mode", &format!("Set how freely one of your person's agents, or you, may act in task work: auto (works on its own inside the sandbox, the default), full (asks for less, more access) or read_only (can look but not change files). Use it {OWNER_ONLY}."), json!({
                "agent": s("Agent name or slug, optional, defaults to you"),
                "mode": s("auto, full or read_only")
            }), &["mode"]));
        }
        if self.can(powers::MANAGE_AGENTS) {
            tools.push(tool("fork_agent", "Make a copy of an agent for your person: same persona, look, tags and kit, with a memory of the original's recent work, but no keys (grant them again) and no history. The original is untouched.", json!({
                "agent": s("Agent name or slug to copy"),
                "name": s("Name for the copy, optional, defaults to <name> Jr")
            }), &["agent"]));
            if self.caller.is_main {
                tools.push(tool("make_main", &format!("Make another of your person's agents their main agent in place of you: it gets their chat, notifications and the main powers, and you become a normal agent. Use it {OWNER_ONLY}."), json!({
                    "agent": s("Agent name or slug")
                }), &["agent"]));
            }
        }
        tools
    }

    async fn governed_agent(&self, args: &Value) -> Result<WorldAgent> {
        match args.get("agent").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
            Some(name) => self.managed_agent(name).await,
            None => Ok(self.caller.clone()),
        }
    }

    async fn owner_org_role(&self) -> Result<Option<OrgRole>> {
        let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
        self.store.role_in_org(owner, self.caller.org_id).await
    }

    async fn emit_by_owner(&self, kind: &str, agent: Uuid, payload: Value) -> Result<()> {
        self.store
            .emit(NewEvent::new(kind).agent(agent).actor(self.caller.owner_user_id).payload(payload), None)
            .await?;
        Ok(())
    }

    pub(super) async fn govern_call(&self, name: &str, args: &Value) -> Result<String> {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let need = |k: &str| arg(k).ok_or_else(|| anyhow!("{k} is required"));
        let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
        match name {
            "pause_agent" => {
                self.require(powers::GOVERN)?;
                let a = self.managed_agent(need("agent")?).await?;
                if a.id == self.caller.id {
                    bail!("you cannot pause yourself; your person can pause you in the app");
                }
                self.require_owner_asked().await?;
                let reason = match arg("reason") {
                    Some(why) => format!("Paused by {}: {}", self.caller.name, super::short(why, 200)),
                    None => format!("Paused by {}", self.caller.name),
                };
                if a.paused_reason.is_some() {
                    return Ok(format!("{} is already paused ({}).", a.name, a.paused_reason.unwrap_or_default()));
                }
                if self.store.set_paused(a.id, Some(&reason)).await? {
                    self.emit_by_owner("agent.paused", a.id, json!({ "name": a.name, "reason": reason, "by": self.caller.name })).await?;
                }
                Ok(format!("Paused {}. Resume them with resume_agent when your person says so.", a.name))
            }
            "archive_agent" => {
                self.require(powers::MANAGE_AGENTS)?;
                let a = self.managed_agent(need("agent")?).await?;
                if a.id == self.caller.id {
                    bail!("you cannot archive yourself");
                }
                if a.is_main {
                    bail!("{} is a main agent; make another agent main first", a.name);
                }
                let days = match args.get("delete_after_days").and_then(Value::as_i64) {
                    None | Some(0) => None,
                    Some(d) if loka_shared::archive::delete_days_valid(d as i32) => Some(d as i32),
                    Some(_) => bail!("delete_after_days must be between 1 and 3650, or 0 for never"),
                };
                self.require_owner_asked().await?;
                let delete_at = self.store.archive_agent(a.id, days).await?;
                self.emit_by_owner("agent.archived", a.id, json!({ "name": a.name, "delete_at": delete_at, "by": self.caller.name })).await?;
                Ok(match delete_at {
                    Some(at) => format!("Archived {}. It will be deleted for good on {} unless your person unarchives it or turns auto delete off.", a.name, at.format("%Y-%m-%d")),
                    None => format!("Archived {}. It stays until your person unarchives or deletes it.", a.name),
                })
            }
            "unarchive_agent" => {
                self.require(powers::MANAGE_AGENTS)?;
                let a = self.managed_agent(need("agent")?).await?;
                if a.archived_at.is_none() {
                    return Ok(format!("{} is not archived.", a.name));
                }
                self.require_owner_asked().await?;
                if self.store.unarchive_agent(a.id).await? {
                    self.emit_by_owner("agent.unarchived", a.id, json!({ "name": a.name, "by": self.caller.name })).await?;
                }
                let still = self.store.get_agent(a.id).await?.and_then(|x| x.paused_reason);
                Ok(match still {
                    Some(reason) => format!("Unarchived {}. It is still paused: {reason}.", a.name),
                    None => format!("Unarchived {}. It is back at work.", a.name),
                })
            }
            "resume_agent" => {
                self.require(powers::GOVERN)?;
                let a = self.managed_agent(need("agent")?).await?;
                let Some(reason) = a.paused_reason.clone() else {
                    return Ok(format!("{} is not paused.", a.name));
                };
                if reason == loka_shared::archive::ARCHIVED {
                    bail!("{} is archived; use unarchive_agent to bring it back", a.name);
                }
                if locked_pause(&reason) {
                    bail!("{} is paused ({reason}) and cannot be resumed from here", a.name);
                }
                self.require_owner_asked().await?;
                if self.store.set_paused(a.id, None).await? {
                    self.emit_by_owner("agent.resumed", a.id, json!({ "name": a.name, "reason": Value::Null, "by": self.caller.name })).await?;
                }
                let note = if reason.starts_with("Daily budget of") {
                    " Their daily budget is still spent, so their next run pauses them again unless the budget is raised with set_budget."
                } else {
                    ""
                };
                Ok(format!("Resumed {} (was: {reason}).{note}", a.name))
            }
            "set_budget" => {
                self.require(powers::GOVERN)?;
                let a = self.governed_agent(args).await?;
                let clear = args.get("clear").and_then(Value::as_bool) == Some(true);
                let usd = args.get("daily_usd").and_then(Value::as_f64);
                let tokens = args.get("daily_tokens").and_then(Value::as_f64);
                if let Some(n) = usd.filter(|n| !(0.0..=100_000.0).contains(n)) {
                    bail!("daily_usd must be between 0 and 100000, not {n}");
                }
                if let Some(n) = tokens.filter(|n| !(1.0..=loka_shared::world::MAX_DAILY_TOKEN_BUDGET).contains(n)) {
                    bail!("daily_tokens must be between 1 and 10000000000, not {n}");
                }
                if !clear && usd.is_none() && tokens.is_none() {
                    bail!("give daily_usd, daily_tokens, or clear true");
                }
                self.require_owner_asked().await?;
                let update = if clear {
                    AgentUpdate { daily_budget_usd: Some(Value::Null), daily_token_budget: Some(Value::Null), ..Default::default() }
                } else {
                    AgentUpdate { daily_budget_usd: usd.map(|n| json!(n)), daily_token_budget: tokens.map(|n| json!(n.round())), ..Default::default() }
                };
                let saved = self.store.update_agent(a.id, &update).await?.ok_or_else(|| anyhow!("agent vanished"))?;
                let released = !self.store.release_budget_pauses(Some(a.id)).await?.is_empty();
                self.emit_by_owner(
                    "agent.updated",
                    a.id,
                    json!({ "name": a.name, "daily_budget_usd": saved.daily_budget_usd, "daily_token_budget": saved.daily_token_budget, "by": self.caller.name }),
                )
                .await?;
                let mut limits = Vec::new();
                if let Some(n) = saved.daily_budget_usd {
                    limits.push(format!("${n:.2}"));
                }
                if let Some(n) = saved.daily_token_budget {
                    limits.push(format!("{} tokens", loka_shared::usage::tokens(n)));
                }
                let what = if limits.is_empty() {
                    format!("{} has no daily budget now.", a.name)
                } else {
                    format!("{}'s daily budget is now {} per day.", a.name, limits.join(" and "))
                };
                Ok(if released { format!("{what} Their budget pause is lifted.") } else { what })
            }
            "set_approval_mode" => {
                self.require(powers::GOVERN)?;
                let a = self.governed_agent(args).await?;
                let mode = need("mode")?;
                if !APPROVAL_MODES.contains(&mode) {
                    bail!("mode must be auto, full or read_only");
                }
                self.require_owner_asked().await?;
                let update = AgentUpdate { approval_mode: Some(mode.to_string()), ..Default::default() };
                self.store.update_agent(a.id, &update).await?.ok_or_else(|| anyhow!("agent vanished"))?;
                self.emit_by_owner("agent.updated", a.id, json!({ "name": a.name, "approval_mode": mode, "by": self.caller.name })).await?;
                Ok(format!("{} now works in {mode} mode from their next run.", a.name))
            }
            "fork_agent" => {
                self.require(powers::MANAGE_AGENTS)?;
                let source = self.agent(need("agent")?).await?;
                let name = arg("name").map(|n| n.chars().take(60).collect::<String>()).unwrap_or_else(|| format!("{} Jr", source.name));
                let base = slugify(&name);
                let mut slug = base.clone();
                let mut n = 2;
                while self.store.slug_taken(&slug).await? {
                    slug = format!("{base}-{n}");
                    n += 1;
                }
                let fork = self
                    .store
                    .fork_agent(source.id, owner, &slug, &name, &self.caller.name)
                    .await?
                    .ok_or_else(|| anyhow!("{} vanished", source.name))?;
                self.emit_by_owner(
                    "agent.created",
                    fork.agent_id,
                    json!({ "name": name, "slug": slug, "created_by": self.caller.name, "forked_from": { "id": source.id, "name": source.name }, "wanted_keys": fork.wanted_keys }),
                )
                .await?;
                let keys = if fork.wanted_keys.is_empty() {
                    String::new()
                } else {
                    format!(" They want these keys granted again: {}.", fork.wanted_keys.join(", "))
                };
                Ok(format!("Made {name} ({slug}), a copy of {}.{keys}", source.name))
            }
            "make_main" => {
                self.require(powers::MANAGE_AGENTS)?;
                if !self.caller.is_main {
                    bail!("only the main agent can hand over the main role");
                }
                let a = self.managed_agent(need("agent")?).await?;
                if a.id == self.caller.id {
                    return Ok("You are already the main agent.".into());
                }
                if a.owner_user_id != Some(owner) || a.org_id != self.caller.org_id {
                    bail!("only your person's own agents in this organization can be their main agent");
                }
                if self.owner_org_role().await?.is_none_or(|r| r < OrgRole::Member) {
                    bail!("your person needs write access here to pick a main agent");
                }
                self.require_owner_asked().await?;
                self.store.set_main_agent(owner, a.id).await?;
                if !self.store.has_subscriptions(a.id).await? {
                    self.store.subscribe(a.id, DEFAULT_MAIN_SUBSCRIPTIONS).await?;
                }
                self.emit_by_owner("agent.main_changed", a.id, json!({ "name": a.name, "user_id": owner })).await?;
                Ok(format!("{} is your person's main agent now; from your next turn you are a normal agent.", a.name))
            }
            other => bail!("unknown tool {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organization_pauses_stay_locked() {
        assert!(locked_pause("Organization daily budget of $5 reached"));
        assert!(locked_pause("Organization suspended"));
        assert!(locked_pause("Owner left the organization"));
        assert!(locked_pause("Owner account disabled"));
        assert!(!locked_pause("Paused by Ada"));
        assert!(!locked_pause("Daily budget of $2.00 reached"));
        assert!(!locked_pause("Model key rejected: OPENAI_API_KEY"));
    }

    #[test]
    fn every_govern_tool_is_listed_once() {
        let mut names = GOVERN_TOOLS.to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), GOVERN_TOOLS.len());
        for name in GOVERN_TOOLS {
            assert!(!super::super::tool_allowed(true, name), "{name}");
            assert!(!super::super::control::CONTROL_TOOLS.contains(name), "{name}");
        }
    }

    #[test]
    fn govern_is_a_real_power() {
        assert!(powers::is_power(powers::GOVERN));
    }
}
