use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};
use loka_shared::command_rules::RuleError;
use loka_shared::key_environments::{ChoiceError, DEFAULT};
use loka_shared::powers;
use loka_shared::world::NewEvent;
use serde_json::{json, Value};
use uuid::Uuid;

use super::Server;

pub(super) const KEY_TOOLS: &[&str] = &["key_environments", "use_key_environment", "command_rules", "change_command_rule"];

const OWNER_ONLY: &str = "only when your verified owner asks for it in this turn";
const RULE_CLIS: &str = "Rules are checked for Claude Code quests and for OpenCode quests that run outside the sandbox; Codex, Cursor and oh-my-pi run commands their own way and use their own allow lists instead";

impl Server {
    pub(super) fn key_tools(&self, tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
        let s = |d: &str| json!({ "type": "string", "description": d });
        if self.caller.owner_user_id.is_none() || !self.can(powers::GOVERN) {
            return Vec::new();
        }
        vec![
            tool("key_environments", "List your person's keys that have more than one value (environments such as staging or a second account) and which environment each of their agents uses. Shows names only, never values; values are added by your person under Settings > Keys.", json!({}), &[]),
            tool("use_key_environment", &format!("Switch one of your person's agents, or you, to the value of a key from another environment (for example the CLAUDE_CODE_OAUTH_TOKEN from staging), or back to default. Leave key empty to switch every key that has that environment. hours makes it temporary; it goes back to default by itself after that. It only picks between values your person already saved and never grants a key the agent does not have. Takes effect on the agent's next run. Use it {OWNER_ONLY}.")
                , json!({
                    "agent": s("Agent name or slug, optional, defaults to you"),
                    "key": s("Key name like ANTHROPIC_API_KEY, optional; empty switches every key that has this environment"),
                    "environment": s("Environment name like staging, or default to go back"),
                    "hours": { "type": "integer", "description": "Go back to default after this many hours, 1 to 720, optional" }
                }), &["environment"]),
            tool("command_rules", &format!("List the command rules your person set: allow rules (when there are any, an agent may only run the listed commands, plus harmless shell builtins like cd and echo) and deny rules (always blocked). A rule is a command and its first words, like git push or npm test. {RULE_CLIS}."), json!({}), &[]),
            tool("change_command_rule", &format!("Add or remove one command rule for one of your person's agents, or for all of them when agent is empty. kind allow lets an agent run that command (any allow rule turns on allow-only mode for that agent), kind deny blocks it. Adding allow rules restricts an agent that had none, so say so when you add the first one. {RULE_CLIS}. Use it {OWNER_ONLY}."), json!({
                "action": s("add or remove"),
                "kind": s("allow or deny"),
                "rule": s("The command and its first words, like git push, npm run build or terraform apply"),
                "agent": s("Agent name or slug, optional; empty means all of your person's agents")
            }), &["action", "kind", "rule"]),
        ]
    }

    async fn agent_names(&self, owner: Uuid) -> Result<BTreeMap<Uuid, String>> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM agent_profiles WHERE owner_user_id = $1")
            .bind(owner)
            .fetch_all(self.store.pool())
            .await?;
        Ok(rows.into_iter().collect())
    }

    pub(super) async fn key_call(&self, name: &str, args: &Value) -> Result<String> {
        self.require(powers::GOVERN)?;
        let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        match name {
            "key_environments" => {
                let variants = self.store.list_environment_variants(owner).await?;
                if variants.is_empty() {
                    return Ok("Your person has no key with more than one value. They can add one under Settings > Keys: open a key and add a value for another environment.".into());
                }
                let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for variant in variants {
                    by_key.entry(variant.name).or_default().push(variant.environment);
                }
                let mut out = String::from("Keys with more than one value:\n");
                for (key, environments) in &by_key {
                    out.push_str(&format!("- {key}: default, {}\n", environments.join(", ")));
                }
                let names = self.agent_names(owner).await?;
                let choices = self.store.list_environment_choices(owner).await?;
                if choices.is_empty() {
                    out.push_str("Every agent uses the default values.");
                } else {
                    out.push_str("Agents not on default:\n");
                    for choice in choices {
                        let until = choice.expires_at.map(|t| format!(" until {}", t.format("%Y-%m-%d %H:%M UTC"))).unwrap_or_default();
                        let who = names.get(&choice.agent_id).cloned().unwrap_or_else(|| "an agent".into());
                        out.push_str(&format!("- {who}: {} from {}{until}\n", choice.name, choice.environment));
                    }
                }
                Ok(out)
            }
            "use_key_environment" => {
                let agent = match arg("agent") {
                    Some(name) => self.managed_agent(name).await?,
                    None => self.caller.clone(),
                };
                if agent.owner_user_id != Some(owner) {
                    bail!("you can only switch keys for your own person's agents");
                }
                let environment = arg("environment").ok_or_else(|| anyhow!("environment is required"))?;
                let hours = args.get("hours").and_then(Value::as_i64);
                self.require_owner_asked().await?;
                let by = format!("agent:{}", self.caller.name);
                match self.store.choose_environment(owner, agent.id, arg("key"), environment, hours, &by).await? {
                    Ok(names) if names.is_empty() => Ok(format!("{} already uses the default values.", agent.name)),
                    Ok(names) => {
                        let target = loka_shared::key_environments::normalize_environment(environment);
                        for key in &names {
                            self.store.release_key_pauses(owner, Some(agent.id), Some(key)).await?;
                        }
                        self.store
                            .emit(
                                NewEvent::new("agent.keys_switched")
                                    .agent(agent.id)
                                    .actor(Some(owner))
                                    .payload(json!({ "keys": names, "environment": target, "hours": hours, "by": self.caller.name, "quiet": true })),
                                None,
                            )
                            .await?;
                        let when = hours.map(|h| format!(" for {h} hours, then back to default")).unwrap_or_default();
                        Ok(if target == DEFAULT {
                            format!("{} is back on the default value of {}. It applies from their next run.", agent.name, names.join(", "))
                        } else {
                            format!("{} now uses {} from {target}{when}. It applies from their next run.", agent.name, names.join(", "))
                        })
                    }
                    Err(ChoiceError::NoSuchEnvironment) => bail!("your person has not saved a value for that key in that environment; they add it under Settings > Keys"),
                    Err(error) => bail!("{error}"),
                }
            }
            "command_rules" => {
                let rules = self.store.list_command_rules(owner).await?;
                if rules.is_empty() {
                    return Ok(format!("No command rules: agents run commands as their CLI allows. {RULE_CLIS}."));
                }
                let names = self.agent_names(owner).await?;
                let lines: Vec<String> = rules
                    .iter()
                    .map(|r| {
                        let scope = r.agent_id.and_then(|id| names.get(&id).cloned()).unwrap_or_else(|| "all agents".into());
                        format!("- {} `{}` ({scope})", r.kind, r.rule)
                    })
                    .collect();
                Ok(format!("{}\n{RULE_CLIS}.", lines.join("\n")))
            }
            "change_command_rule" => {
                let action = arg("action").ok_or_else(|| anyhow!("action is required"))?;
                let kind = arg("kind").ok_or_else(|| anyhow!("kind is required"))?;
                let rule = arg("rule").ok_or_else(|| anyhow!("rule is required"))?;
                if !matches!(kind, "allow" | "deny") {
                    bail!("kind is allow or deny");
                }
                let agent = match arg("agent") {
                    Some(name) => {
                        let agent = self.managed_agent(name).await?;
                        if agent.owner_user_id != Some(owner) {
                            bail!("you can only set rules for your own person's agents");
                        }
                        Some(agent)
                    }
                    None => None,
                };
                self.require_owner_asked().await?;
                let scope = agent.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| "all agents".into());
                match action {
                    "add" => match self.store.add_command_rule(owner, agent.as_ref().map(|a| a.id), kind, rule, &format!("agent:{}", self.caller.name)).await? {
                        Ok(saved) => Ok(format!("Added {} rule `{}` for {scope}. It applies from the next run. {RULE_CLIS}.", saved.kind, saved.rule)),
                        Err(RuleError::Invalid(reason)) => bail!("{reason}"),
                        Err(error) => bail!("{error}"),
                    },
                    "remove" => {
                        let words = loka_shared::command_rules::parse_rule(rule).map_err(|reason| anyhow!(reason))?;
                        let normalized = loka_shared::command_rules::normalized(&words);
                        let found = self
                            .store
                            .list_command_rules(owner)
                            .await?
                            .into_iter()
                            .find(|r| r.kind == kind && r.rule == normalized && r.agent_id == agent.as_ref().map(|a| a.id));
                        match found {
                            Some(found) if self.store.remove_command_rule(owner, found.id).await? => {
                                Ok(format!("Removed {kind} rule `{normalized}` for {scope}. It applies from the next run."))
                            }
                            _ => bail!("there is no {kind} rule `{normalized}` for {scope}; command_rules lists them"),
                        }
                    }
                    _ => bail!("action is add or remove"),
                }
            }
            other => bail!("unknown tool {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_tools_are_write_tools_on_owner_devices_only() {
        for name in KEY_TOOLS {
            assert!(!super::super::tool_allowed(true, name), "{name}");
        }
        assert!(super::super::OWNER_DEVICE_POWERS.contains(&powers::GOVERN));
    }
}
