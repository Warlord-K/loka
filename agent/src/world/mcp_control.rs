use anyhow::{anyhow, bail, Result};
use loka_shared::channels::Author;
use loka_shared::milestones::{Milestone, MilestoneStore, NewMilestone};
use loka_shared::powers;
use loka_shared::runners::{RunnerStore, RunnerView, UpdateRunner};
use loka_shared::world::{KitKind, NewEvent, RetryRunError, SessionRun, WorldAgent};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{short, Server};

pub(super) const CONTROL_TOOLS: &[&str] = &[
    "start_folder_session",
    "stop_run",
    "retry_run",
    "run_history",
    "run_log",
    "set_academy",
    "uninstall",
    "omp_models",
    "remove_from_channel",
    "edit_channel_message",
    "check_provider",
    "owner_prefs",
    "rename_device",
    "dismiss_wanted_key",
    "notifications",
    "set_up_starter_team",
    "suggest_setup",
    "storage",
    "clean_up",
];

const OWNER_ONLY: &str = "only when your verified owner asks for it in this turn";

impl Server {
    pub(super) fn control_tools(&self, tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
        let s = |d: &str| json!({ "type": "string", "description": d });
        let mut tools = vec![
            tool("run_history", "List recent runs (main turns, quests, reviews, asides, folder sessions) of you or another agent of your person, newest first: run id, kind, status, when, how long, cost, and for failures the cause in plain words. Use run_log on a run id for the details.", json!({
                "agent": s("Agent name or slug, optional, defaults to you"),
                "status": s("Only runs with this status: queued, running, completed, failed, timed_out or cancelled, optional"),
                "kind": s("Only this kind: turn, task, followup, review, aside, direct, optional"),
                "limit": { "type": "integer", "description": "1 to 30, default 10" }
            }), &[]),
            tool("run_log", "Look inside one run of an agent of your person: its status, cost, tokens and duration, the failure cause and error, the start of its prompt, the end of its output and its last executor steps.", json!({
                "run": s("Run id or its first 8 characters, from run_history"),
                "steps": { "type": "integer", "description": "How many of the last executor steps to show, 1 to 40, default 15" }
            }), &["run"]),
            tool("stop_run", "Stop a running quest, follow-up, review, aside or folder session run. With step true only the step it is running right now is stopped and the work carries on. Your own runs always; other agents' runs of your person need the oversee power. Main turns cannot be stopped.", json!({
                "run": s("Run id or its first 8 characters"),
                "step": { "type": "boolean", "description": "Stop just the current step instead of the whole run, default false" }
            }), &["run"]),
            tool("retry_run", "Retry a failed or timed out run from where it stopped. Refused when newer work exists for it or it already has an active run. Same scope as stop_run.", json!({
                "run": s("Run id or its first 8 characters")
            }), &["run"]),
            tool("check_provider", "Check a model provider key your person saved (anthropic, openai, openrouter, xai, gemini, fal and so on): whether it is saved, whether its format looks right and, with live, whether the provider accepts it. The value is never shown.", json!({
                "provider": s("Provider id or name"),
                "live": { "type": "boolean", "description": "Also make one cheap call to the provider, default false" }
            }), &["provider"]),
            tool("remove_from_channel", "Take agents of your person out of a public channel you are in, or leave it yourself with leave.", json!({
                "channel": s("Channel name or id"),
                "agents": { "type": "array", "items": { "type": "string" }, "description": "Agents to remove, by name or slug" },
                "leave": { "type": "boolean", "description": "Leave the channel yourself" }
            }), &["channel"]),
            tool("edit_channel_message", "Fix one of your own messages in a channel. People see it marked edited.", json!({
                "channel": s("Channel name or id"),
                "message": s("Message id, like msg:ch123 or 123"),
                "text": s("The new text")
            }), &["channel", "message", "text"]),
        ];
        if self.caller.owner_user_id.is_some() {
            tools.push(tool("start_folder_session", &format!("Work with an agent directly in a folder your person shared under Workshop on one of their Macs: a fresh session with full tools in that folder on the Mac that holds it, no quest, worktree, review or PR. devices lists the shared folders per Mac. The agent does not move. Use it {OWNER_ONLY}. Follow up with message_session on the session id."), json!({
                "folder": s("Shared folder name or id, from devices"),
                "prompt": s("What to do in the folder"),
                "agent": s("Agent name or slug, optional, defaults to you; another agent needs assign_others"),
                "cli": s("CLI for this session, optional"),
                "model": s("Model for this session, optional; needed when cli differs from the agent's")
            }), &["folder", "prompt"]));
        }
        if self.caller.is_main && self.caller.owner_user_id.is_some() {
            tools.push(tool("owner_prefs", &format!("Read your person's Loka settings, or change them {OWNER_ONLY}: whether an @mention of them in a channel wakes you (mention_wakes_main), their status (emoji and text, or clear_status), and which Slack source feeds passive mentions (desktop, app or both). With no changes it only reads them."), json!({
                "mention_wakes_main": { "type": "boolean" },
                "status_emoji": s("One emoji, optional"),
                "status_text": s("Short status, up to 100 characters, optional"),
                "clear_status": { "type": "boolean" },
                "slack_source": s("desktop, app or both")
            }), &[]));
            tools.push(tool("set_academy", &format!("Turn Academy offers (short challenges while quests run) on or off for your person, {OWNER_ONLY}."), json!({
                "on": { "type": "boolean" }
            }), &["on"]));
        }
        if self.caller.is_main && self.caller.owner_user_id.is_some() && self.can(powers::MANAGE_AGENTS) {
            tools.push(tool("set_up_starter_team", &format!("Build your person's starter team in one step, with you as coordinator: 2 coders on their strongest model and a reviewer (no watcher agents: each agent watches what its work needs with subscribe); a roster rule naming everyone's one job, a notes skill for each agent and a nightly notes and reset. Models come from the CLIs they actually have. Safe to repeat: it only adds what is missing and never duplicates. Use preview true first to show who would be created; build it {OWNER_ONLY}. Undo with remove_agent, delete_schedule, delete_team and their rules page."), json!({
                "preview": { "type": "boolean", "description": "Only show what would be created, default false" },
                "team": { "type": "boolean", "description": "Also group the two coders in a team with a lead and its own channel, default false" }
            }), &[]));
        }
        if self.caller.is_main && self.caller.owner_user_id.is_some() {
            tools.push(tool("suggest_setup", &format!("Offer to read how your person already works with Claude Code, Codex, Cursor and omp on their own Mac (recent sessions, CLAUDE.md and AGENTS.md, MCP settings) and suggest a team, skills, rules, MCP servers, repos and starting notes. Without sources it posts a card where they tick which CLIs to read and a time range; with sources it starts reading right away, {OWNER_ONLY}. Reading happens on their Mac and only suggestions come back as a card they pick from; nothing is created until they pick it."), json!({
                "sources": { "type": "array", "items": { "type": "string", "enum": loka_shared::setup_scan::SOURCES }, "description": "CLIs to read, optional" },
                "days": { "type": "integer", "description": "How many days back to read, 1 to 365, default 30" }
            }), &[]));
        }
        if self.can(powers::MANAGE_AGENTS) {
            tools.push(tool("rename_device", &format!("Rename one of your person's devices (desktop runners), {OWNER_ONLY}."), json!({
                "device": s("Device name or id, from devices"),
                "name": s("New name, up to 80 characters")
            }), &["device", "name"]));
        }
        if self.can(powers::INSTALL) {
            tools.push(tool("uninstall", "Remove an MCP server or a skill from the world for good: every agent loses it. Only ones your person installed; ones installed for everyone are removed by an admin in the app. To take one away from a single agent use equip with remove. Notes skills are kept; use update_my_notes. Only when your person asks.", json!({
                "mcp_server": s("Installed MCP server name, optional"),
                "skill": s("Installed skill name, optional")
            }), &[]));
            tools.push(tool("omp_models", "Read the omp models.yml saved for an agent, with secret values masked, which keys it references and whether each is granted, and the models it offers. Clear it with set_omp_models clear true.", json!({
                "agent": s("Agent name or slug, optional, defaults to you")
            }), &[]));
        }
        if self.can(powers::MANAGE_POWERS) {
            tools.push(tool("dismiss_wanted_key", "Drop a key from the list of keys an agent wanted after a transfer (environment_keys shows them) once your person decided not to give it.", json!({
                "agent": s("Agent name or slug"),
                "key": s("Key name")
            }), &["agent", "key"]));
        }
        if self.caller.is_main && self.caller.owner_user_id.is_some() {
            tools.push(tool("storage", "See how much space your person's agents take in Loka (chats, run history, run step logs, activity), per agent, with when each was last active and past cleanups, plus the disk their devices spend on quest workspaces (total, free space, and how much belongs to closed quests). Read only. Use it when they ask what is using space or want to tidy up, then suggest a clean_up.", json!({}), &[]));
            tools.push(tool("clean_up", &format!("Permanently delete old data of your person's agents to free space. Kinds: run_logs (step logs, prompts and raw output of finished runs; cost and run list stay), chats (chat messages older than the cutoff), quests (closed quests with their sessions, runs and logs; lifetime cost stays), activity (old activity feed events and cleared bell items), workspaces (checkouts, build output and scratch files of closed quests on your person's devices, oldest first; branches, commits and pull requests stay, older_than_days and agent do not apply). Without confirm it only previews what would go and how much space it frees: always preview first, tell your person exactly what will be deleted, and that it cannot be undone. Pass confirm true {OWNER_ONLY}, after they agreed to that exact cleanup."), json!({
                "kind": s("run_logs, chats, quests, activity or workspaces"),
                "older_than_days": { "type": "integer", "description": "Only data older than this many days, 7 to 3650 (not used for workspaces)" },
                "agent": s("Agent name or slug, optional, defaults to all of your person's agents"),
                "confirm": { "type": "boolean", "description": "Really delete, default false (preview)" }
            }), &["kind"]));
        }
        if self.can(powers::OVERSEE) && self.caller.owner_user_id.is_some() {
            tools.push(tool("notifications", &format!("Read your person's notification bell, newest last: open asks, agent messages, mentions, reminders, failed quests and stuck agents. Items clear themselves when answered or opened; nothing is cleared unless you pass mark_seen, {OWNER_ONLY}."), json!({
                "unread_only": { "type": "boolean", "description": "Only unread ones, default false" },
                "mark_seen": { "type": "boolean", "description": "Clear everything in the bell after reading, default false" },
                "limit": { "type": "integer", "description": "1 to 60, default 20" }
            }), &[]));
        }
        tools
    }

    pub(super) async fn require_owner_asked(&self) -> Result<()> {
        if !self.trusted_device() {
            bail!("this changes your person's own setup, so it cannot be done from a device someone else shared; ask them to do it in the app");
        }
        if self.store.owner_asked_now(self.caller.id).await? {
            return Ok(());
        }
        if let Some(quick) = self.quick_session() {
            if self.store.quick_owner_live(quick, self.caller.id).await? {
                return Ok(());
            }
        }
        bail!("this changes your person's own setup, so it only works while you answer a verified message from them; ask them to tell you directly")
    }

    async fn owner_run(&self, key: &str) -> Result<(SessionRun, WorldAgent)> {
        let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone, so there are no runs to look at"))?;
        let key = key.trim().trim_start_matches("run:");
        if key.len() < 4 {
            bail!("give at least the first 8 characters of the run id");
        }
        let found = self.store.find_owner_runs(owner, key).await?;
        let run = match found.as_slice() {
            [] => bail!("no run of your person's agents starts with {key}; run_history lists them"),
            [one] => one.clone(),
            many => bail!("{key} matches {} runs; use more characters", many.len()),
        };
        let agent = self.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("that run's agent is gone"))?;
        Ok((run, agent))
    }

    fn require_run_control(&self, agent: &WorldAgent) -> Result<()> {
        if agent.id != self.caller.id {
            self.require(powers::OVERSEE)?;
        }
        Ok(())
    }

    async fn runner_by_name(&self, owner: Uuid, device: &str) -> Result<RunnerView> {
        let wanted = device.trim().to_lowercase();
        let list = RunnerStore::new(self.store.pool().clone()).list(owner).await?;
        if let Some(r) = list.iter().find(|r| r.name.to_lowercase() == wanted || (wanted.len() >= 4 && r.id.to_string().starts_with(&wanted))) {
            return Ok(r.clone());
        }
        let partial: Vec<_> = list.iter().filter(|r| r.name.to_lowercase().contains(&wanted)).collect();
        match partial.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no device called {device}; use devices to see them"),
            _ => bail!("{device} matches more than one device; use its id from devices"),
        }
    }

    pub(super) async fn edit_milestones(&self, task_id: Uuid, args: &Value) -> Result<String> {
        let store = MilestoneStore::new(self.store.pool().clone());
        let mut notes = Vec::new();
        let current = store.for_task(task_id).await?;
        let mut drops = Vec::new();
        for change in args.get("milestones").and_then(Value::as_array).into_iter().flatten() {
            let key = change.get("milestone").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
            let Some(found) = pick_milestone(&current, &key) else {
                notes.push(format!("no milestone matches {key}"));
                continue;
            };
            if change.get("drop").and_then(Value::as_bool) == Some(true) {
                drops.push(found.id);
                continue;
            }
            let title = change.get("title").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
            let status = change.get("status").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
            if title.is_none() && status.is_none() {
                notes.push(format!("nothing to change on {}", found.title));
                continue;
            }
            match store.update(found.id, title, status).await {
                Ok(Some(updated)) if status == Some("pending") && updated.kind == "merge" => self.rearm_merge(&store, task_id).await?,
                Ok(_) => {}
                Err(e) => notes.push(format!("{}: {e}", found.title)),
            }
        }
        for id in drops {
            store.delete(id).await?;
        }
        let added: Vec<NewMilestone> = args
            .get("add_milestones")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|title| NewMilestone { title: title.to_string(), kind: "work".to_string() })
            .collect();
        if !added.is_empty() {
            store.add(task_id, &added).await?;
        }
        if args.get("check_merge").and_then(Value::as_bool) == Some(true) && !store.check_now(task_id).await? {
            notes.push("no merge milestone is waiting to be checked".into());
        }
        let agent = self.db.get_task(task_id).await?.and_then(|t| t.assigned_agent_id);
        let mut event = NewEvent::new("milestones.updated").task(Some(task_id)).payload(json!({ "task_id": task_id, "by": self.caller.name }));
        if let Some(agent) = agent {
            event = event.agent(agent);
        }
        self.store.emit(event, None).await?;
        let list = store.for_task(task_id).await?;
        let mut out = if list.is_empty() {
            "No milestones.".to_string()
        } else {
            format!(
                "Milestones: {}.",
                list.iter().enumerate().map(|(i, m)| format!("{}. {} [{}]", i + 1, m.title, m.status)).collect::<Vec<_>>().join("; ")
            )
        };
        if !notes.is_empty() {
            out.push_str(&format!(" Skipped: {}.", notes.join("; ")));
        }
        Ok(out)
    }

    async fn rearm_merge(&self, store: &MilestoneStore, task_id: Uuid) -> Result<()> {
        if let Some(task) = self.db.get_task(task_id).await? {
            if task.pr_url.is_some() && task.status == "review" {
                store.arm_merge(task.id, true).await?;
                store.check_now(task.id).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn control_call(&self, name: &str, args: &Value) -> Result<String> {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let need = |k: &str| arg(k).ok_or_else(|| anyhow!("{k} is required"));
        let flag = |k: &str| args.get(k).and_then(Value::as_bool) == Some(true);
        match name {
            "run_history" => {
                let agent = match arg("agent") {
                    Some(n) => self.managed_agent(n).await?,
                    None => self.caller.clone(),
                };
                let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10).clamp(1, 30) as usize;
                let (status, kind) = (arg("status"), arg("kind"));
                let fetch = if status.is_some() || kind.is_some() { 200 } else { limit as i64 };
                let runs: Vec<SessionRun> = self
                    .store
                    .list_runs(agent.id, fetch)
                    .await?
                    .into_iter()
                    .filter(|r| status.is_none_or(|s| r.status == s) && kind.is_none_or(|k| r.kind == k))
                    .take(limit)
                    .collect();
                if runs.is_empty() {
                    return Ok(format!("No matching runs for {}.", agent.name));
                }
                Ok(format!("Runs of {}:\n{}", agent.name, runs.iter().map(run_line).collect::<Vec<_>>().join("\n")))
            }
            "run_log" => {
                let (run, agent) = self.owner_run(need("run")?).await?;
                self.require_run_control(&agent)?;
                let steps = args.get("steps").and_then(Value::as_i64).unwrap_or(15).clamp(1, 40);
                let mut out = vec![format!("{} of {}", run_line(&run), agent.name)];
                out.push(format!(
                    "Tokens: {} in ({} cached), {} out. Time limit {} min.{}",
                    run.input_tokens,
                    run.cached_input_tokens,
                    run.output_tokens,
                    run.timeout_secs / 60,
                    run.task_id.map(|t| format!(" Quest {}.", &t.to_string()[..8])).unwrap_or_default()
                ));
                if let Some(error) = run.error.as_deref().filter(|e| !e.trim().is_empty()) {
                    out.push(format!("Error: {}", tail(&loka_shared::cli_errors::describe(error), 2000)));
                }
                if let Some(diagnosis) = self.store.run_diagnosis(run.id).await? {
                    out.push(format!(
                        "Cause: {} at the {} stage after {} steps: {}{}",
                        diagnosis.cause.as_str(),
                        diagnosis.stage,
                        diagnosis.turns,
                        diagnosis.reason,
                        if diagnosis.auto_retried { " Loka had already retried it once." } else { "" }
                    ));
                }
                out.push(format!("Prompt starts: {}", short(&run.prompt, 600)));
                if let Some(output) = run.output.as_deref().filter(|o| !o.trim().is_empty()) {
                    out.push(format!("Output ends: {}", tail(output, 1500)));
                }
                let events = self.store.run_events_tail(run.id, steps).await?;
                if !events.is_empty() {
                    out.push("Last steps:".into());
                    for e in events {
                        out.push(format!(
                            "- {} {}{}: {}",
                            e.created_at.format("%H:%M:%S"),
                            e.kind,
                            e.tool.map(|t| format!(" {t}")).unwrap_or_default(),
                            short(&e.body, 300)
                        ));
                    }
                }
                Ok(out.join("\n"))
            }
            "stop_run" => {
                let (run, agent) = self.owner_run(need("run")?).await?;
                self.require_run_control(&agent)?;
                if run.kind == "turn" {
                    bail!("main turns cannot be stopped");
                }
                if run.status != "running" {
                    bail!("that run is {}, not running", run.status);
                }
                if flag("step") {
                    let session = self.store.get_session(run.session_id).await?.ok_or_else(|| anyhow!("that session is gone"))?;
                    let text = format!("{} stopped the step you were running. Do not run it again the same way; carry on with the rest of the task, and say if you are blocked.", self.caller.name);
                    self.store.start_followup(&session, &text, &self.caller.name, true).await?;
                    return Ok(format!("Stopping the current step of {} ({}); the work carries on.", &run.id.to_string()[..8], agent.name));
                }
                let coordinator = loka_agent::runner_server::RunnerCoordinator::new(self.db.clone(), self.store.clone());
                if !coordinator.stop_run(run.id, &self.caller.name).await? {
                    bail!("that run could not be stopped; it may have just finished");
                }
                Ok(format!("Stopping {} run {} of {}.", run.kind, &run.id.to_string()[..8], agent.name))
            }
            "retry_run" => {
                let (run, agent) = self.owner_run(need("run")?).await?;
                self.require_run_control(&agent)?;
                let owner = self.owner()?;
                match self.store.retry_run(run.id, owner).await {
                    Ok(retried) => {
                        Ok(format!(
                            "Retrying {} run {} of {} ({}).{}",
                            run.kind,
                            &run.id.to_string()[..8],
                            agent.name,
                            retried.retry_mode,
                            retried.run.map(|r| format!(" New run {}.", &r.id.to_string()[..8])).unwrap_or_default()
                        ))
                    }
                    Err(RetryRunError::NotFound) => bail!("that run is gone"),
                    Err(RetryRunError::Conflict(reason)) => bail!("cannot retry: {reason}"),
                    Err(RetryRunError::Database(e)) => Err(e.into()),
                    Err(RetryRunError::Internal(e)) => Err(e),
                }
            }
            "start_folder_session" => {
                let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
                self.require_owner_asked().await?;
                let agent = match arg("agent") {
                    Some(n) => self.managed_agent(n).await?,
                    None => self.caller.clone(),
                };
                if agent.id != self.caller.id {
                    self.require(powers::ASSIGN_OTHERS)?;
                }
                let choice = loka_shared::world::AsideChoice::parse(arg("cli"), arg("model"))?;
                self.ensure_cli_allowed(Some(choice.cli.as_deref().unwrap_or(&agent.cli)), &agent).await?;
                let wanted = need("folder")?;
                let mut macs = RunnerStore::new(self.store.pool().clone()).owner_macs(owner).await?;
                if macs.is_empty() {
                    bail!("your person has no Mac paired with Loka; folder sessions run on their Macs");
                }
                macs.sort_by_key(|r| Some(r.id) != agent.runner_id);
                let (runner, (folder_id, folder_name)) = macs
                    .into_iter()
                    .find_map(|r| pick_folder(&r.capabilities, wanted).map(|f| (r, f)))
                    .ok_or_else(|| anyhow!("no Mac of your person shares a folder called {wanted}; devices lists them, and your person shares more from Workshop"))?;
                let session = self
                    .store
                    .start_direct(
                        agent.id,
                        folder_id,
                        runner.id,
                        &folder_name,
                        json!({ "kind": "agent", "agent_id": self.caller.id, "name": self.caller.name }),
                        &choice,
                    )
                    .await?;
                let run = self.store.start_followup(&session, need("prompt")?, &self.caller.name, false).await?;
                Ok(format!(
                    "Started a folder session for {} in {} on {} (session {}, run {}). Follow up with message_session.",
                    agent.name,
                    folder_name,
                    runner.name,
                    &session.id.to_string()[..8],
                    &run.id.to_string()[..8]
                ))
            }
            "set_up_starter_team" => {
                self.require(powers::MANAGE_AGENTS)?;
                if !self.caller.is_main {
                    bail!("only your person's main agent sets up the starter team");
                }
                let owner = self.owner()?;
                if flag("preview") {
                    let plan = self.store.starter_plan(owner, self.caller.org_id).await?;
                    return Ok(starter_preview(&plan));
                }
                self.require_owner_asked().await?;
                let options = loka_shared::starter_team::StarterOptions { team: flag("team"), ..Default::default() };
                Ok(self.store.build_starter_team(owner, self.caller.org_id, &options).await?.summary())
            }
            "suggest_setup" => {
                if !self.caller.is_main {
                    bail!("only your person's main agent reads their setup");
                }
                let owner = self.owner()?;
                let sources: Vec<String> = args
                    .get("sources")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                if sources.is_empty() {
                    return Ok(match self.store.post_setup_request(owner, &self.caller, false).await? {
                        Ok(_) => "Posted a card in your chat where your person ticks which CLIs to read and a time range. Their suggestions will arrive as another card.".into(),
                        Err(why) => format!("Not possible yet: {why}"),
                    });
                }
                self.require_owner_asked().await?;
                let days = args.get("days").and_then(Value::as_u64).unwrap_or(u64::from(loka_shared::setup_scan::DEFAULT_DAYS)).min(365) as u32;
                let request = loka_shared::setup_scan::ScanRequest::new(&sources, days)
                    .ok_or_else(|| anyhow!("sources must name claude, codex, cursor or omp"))?;
                Ok(match self.store.start_setup_scan(owner, &self.caller, &request).await? {
                    Ok(_) => format!("Reading {} on their Mac now; the suggestions card will show up in your chat in a few minutes.", request.names()),
                    Err(why) => format!("Not possible yet: {why}"),
                })
            }
            "set_academy" => {
                self.require_owner_asked().await?;
                let on = args.get("on").and_then(Value::as_bool).ok_or_else(|| anyhow!("on is required"))?;
                self.store.set_academy_on(self.owner()?, on).await?;
                Ok(if on { "Academy offers are on.".into() } else { "Academy offers are off; open offers were withdrawn.".into() })
            }
            "owner_prefs" => {
                let owner = self.owner()?;
                let wakes = args.get("mention_wakes_main").and_then(Value::as_bool);
                let clear = flag("clear_status");
                let (emoji, text, source) = (arg("status_emoji"), arg("status_text"), arg("slack_source"));
                let changing = wakes.is_some() || clear || emoji.is_some() || text.is_some() || source.is_some();
                if changing {
                    self.require_owner_asked().await?;
                    if emoji.is_some_and(|e| e.chars().count() > 16) || text.is_some_and(|t| t.chars().count() > 100) {
                        bail!("status emoji is at most 16 characters and text at most 100");
                    }
                    if let Some(source) = source {
                        if !loka_shared::slack::SOURCES.contains(&source) {
                            bail!("slack_source must be {}", loka_shared::slack::SOURCES.join(", "));
                        }
                        self.store.set_slack_source(owner, source).await?;
                    }
                    if let Some(on) = wakes {
                        self.store.set_mention_wakes_main(owner, on).await?;
                    }
                    if clear {
                        self.store.set_user_status(owner, None, None).await?;
                    } else if emoji.is_some() || text.is_some() {
                        self.store.set_user_status(owner, emoji, text).await?;
                    }
                }
                let (_, _, _, _, status_emoji, status_text) = self.store.me(owner).await?.ok_or_else(|| anyhow!("your person is gone"))?;
                let status = match (status_emoji, status_text) {
                    (None, None) => "none".to_string(),
                    (e, t) => format!("{} {}", e.unwrap_or_default(), t.unwrap_or_default()).trim().to_string(),
                };
                Ok(format!(
                    "{}mention_wakes_main: {}. Status: {status}. Slack source: {}.",
                    if changing { "Saved. " } else { "" },
                    self.store.mention_wakes_main(owner).await?,
                    self.store.slack_source(owner).await?
                ))
            }
            "rename_device" => {
                self.require(powers::MANAGE_AGENTS)?;
                self.require_owner_asked().await?;
                let owner = self.owner()?;
                let runner = self.runner_by_name(owner, need("device")?).await?;
                let name = need("name")?;
                if name.chars().count() > 80 {
                    bail!("device names are at most 80 characters");
                }
                let (updated, _) = RunnerStore::new(self.store.pool().clone())
                    .update(owner, runner.id, &UpdateRunner { name: Some(name.to_string()), paused: None, screen_access: None, app_control: None })
                    .await?;
                let updated = updated.ok_or_else(|| anyhow!("that device is gone"))?;
                Ok(format!("Renamed {} to {}.", runner.name, updated.name))
            }
            "uninstall" => {
                self.require(powers::INSTALL)?;
                let owner = self.owner()?;
                let (kind, id, label) = match (arg("mcp_server"), arg("skill")) {
                    (Some(n), None) => {
                        let server = self
                            .store
                            .list_mcp_servers(self.caller.org_id)
                            .await?
                            .into_iter()
                            .find(|s| s.name.eq_ignore_ascii_case(n))
                            .ok_or_else(|| anyhow!("no MCP server named {n}; check kit"))?;
                        (KitKind::McpServer, server.id, format!("MCP server {}", server.name))
                    }
                    (None, Some(n)) => {
                        let skill = self
                            .store
                            .visible_skills(self.caller.owner_user_id, self.admin_reach().await, self.caller.org_id)
                            .await?
                            .into_iter()
                            .find(|s| s.name.eq_ignore_ascii_case(n))
                            .ok_or_else(|| anyhow!("no skill named {n}; check kit"))?;
                        if skill.name.ends_with("-notes") {
                            bail!("{} is an agent's notes; they keep them with update_my_notes", skill.name);
                        }
                        (KitKind::Skill, skill.id, format!("skill {}", skill.name))
                    }
                    _ => bail!("name exactly one mcp_server or skill"),
                };
                let (scope, created_by) = self.store.kit_creator(kind, id).await?.ok_or_else(|| anyhow!("{label} is already gone"))?;
                if !uninstall_allowed(&scope, created_by, owner, false) {
                    if scope == "global" {
                        bail!("{label} is installed for everyone; only an admin can remove it in the app");
                    }
                    bail!("{label} was installed by someone else");
                }
                let removed = match kind {
                    KitKind::McpServer => self.store.delete_mcp_server(id, self.caller.org_id).await?,
                    KitKind::Skill => self.store.delete_skill(id, self.caller.org_id).await?,
                };
                if removed == 0 {
                    bail!("{label} is already gone");
                }
                Ok(format!("Removed {label}; no agent has it any more."))
            }
            "omp_models" => {
                self.require(powers::INSTALL)?;
                let target = match arg("agent") {
                    Some(n) => self.managed_agent(n).await?,
                    None => self.caller.clone(),
                };
                let Some(stored) = self.store.omp_config(target.id).await? else {
                    return Ok(format!("{} has no omp model config; it uses the default models.", target.name));
                };
                let (text, _) = loka_shared::omp_config::masked(&stored)?;
                let names = loka_shared::omp_config::secret_names(&stored).unwrap_or_default();
                let granted = self.store.environment_names_for_agent(target.id).await?;
                let keys = names
                    .iter()
                    .map(|n| format!("{n} ({})", if granted.contains(n) { "granted" } else { "not granted" }))
                    .collect::<Vec<_>>();
                let models = loka_shared::omp_config::models(&stored).into_iter().map(|m| m.id).collect::<Vec<_>>();
                Ok(format!(
                    "{}'s omp models.yml:\n{text}\nKeys: {}\nModels: {}",
                    target.name,
                    if keys.is_empty() { "none".into() } else { keys.join(", ") },
                    if models.is_empty() { "none".into() } else { models.join(", ") }
                ))
            }
            "remove_from_channel" => {
                let channel = self.store.find_channel(self.caller.org_id, need("channel")?).await?.ok_or_else(|| anyhow!("no such channel"))?;
                if channel.kind != "channel" || !self.store.is_member(channel.id, "agent", self.caller.id).await? {
                    bail!("you can only change members of public channels you are in");
                }
                let mut removed = Vec::new();
                let mut refused = Vec::new();
                for wanted in args.get("agents").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                    match self.managed_agent(wanted).await {
                        Ok(a) if a.id != self.caller.id => {
                            self.store.remove_member(channel.id, "agent", a.id).await?;
                            removed.push(a.name);
                        }
                        Ok(_) => refused.push(format!("{wanted} (that is you; use leave)")),
                        Err(e) => refused.push(format!("{wanted} ({e})")),
                    }
                }
                let leave = flag("leave");
                if leave {
                    self.store.remove_member(channel.id, "agent", self.caller.id).await?;
                }
                if removed.is_empty() && !leave && refused.is_empty() {
                    bail!("give agents or leave");
                }
                let mut out = format!("#{}:", channel.name);
                if !removed.is_empty() {
                    out.push_str(&format!(" removed {}.", removed.join(", ")));
                }
                if leave {
                    out.push_str(" You left.");
                }
                if !refused.is_empty() {
                    out.push_str(&format!(" Not removed: {}.", refused.join(", ")));
                }
                Ok(out)
            }
            "edit_channel_message" => {
                let channel = self.store.find_channel(self.caller.org_id, need("channel")?).await?.ok_or_else(|| anyhow!("no such channel"))?;
                if !self.store.is_member(channel.id, "agent", self.caller.id).await? {
                    bail!("you are not in that channel");
                }
                let id = channel_message_id(need("message")?).ok_or_else(|| anyhow!("message must look like msg:ch123 or 123"))?;
                let text = self.store.expand_message_links(&super::super::prompts::plain_dashes(need("text")?), self.caller.id).await;
                self.store
                    .edit_channel_message(channel.id, id, Author::Agent(self.caller.id), &text)
                    .await?
                    .ok_or_else(|| anyhow!("no message of yours with that id in #{}", channel.name))?;
                Ok(format!("Edited msg:ch{id} in #{}.", channel.name))
            }
            "check_provider" => {
                let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone, so there are no keys to check"))?;
                let wanted = need("provider")?.to_lowercase();
                let provider = loka_shared::providers::PROVIDERS
                    .iter()
                    .find(|p| p.id == wanted || p.label.to_lowercase() == wanted || p.env.to_lowercase() == wanted)
                    .ok_or_else(|| {
                        anyhow!(
                            "unknown provider {wanted}; known: {}",
                            loka_shared::providers::PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                        )
                    })?;
                let Some(value) = self.store.environment_value(owner, provider.env).await? else {
                    return Ok(format!("{} ({}) is not saved for your person. They add it in You > Environment.", provider.label, provider.env));
                };
                let format = match loka_shared::providers::format_problem(provider, &value) {
                    Some(problem) => format!("Format problem: {problem}."),
                    None => "Format looks right.".to_string(),
                };
                let live = if !flag("live") {
                    String::new()
                } else if !provider.testable {
                    " No live check exists for this provider.".to_string()
                } else {
                    match loka_shared::providers::live_check(provider, &value).await {
                        Ok(()) => " The provider accepted it.".to_string(),
                        Err(detail) => format!(" Live check failed: {detail}."),
                    }
                };
                Ok(format!("{} ({}) is saved. {format}{live}", provider.label, provider.env))
            }
            "dismiss_wanted_key" => {
                self.require(powers::MANAGE_POWERS)?;
                let agent = self.managed_agent(need("agent")?).await?;
                let key = need("key")?;
                if !loka_shared::environment::valid_name(key) {
                    bail!("{key} is not a key name");
                }
                if !self.store.dismiss_wanted_key(self.owner()?, agent.id, key).await? {
                    bail!("{} does not want {key}", agent.name);
                }
                Ok(format!("Dropped {key} from what {} wants.", agent.name))
            }
            "storage" | "clean_up" => {
                if !self.caller.is_main {
                    bail!("only your person's main agent can do this");
                }
                let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
                let size = loka_shared::cleanup::human_bytes;
                if name == "storage" {
                    let report = self.store.storage_report(owner, self.caller.org_id).await?;
                    let mut lines = vec![format!("Total: {} across {} agents.", size(report.total_bytes), report.agents.len())];
                    for a in report.agents.iter().take(25) {
                        lines.push(format!(
                            "- {} (@{}): {} | chats {} ({} messages), runs {} ({}), step logs {}, activity {} | last active {}",
                            a.name,
                            a.slug,
                            size(a.total_bytes),
                            size(a.message_bytes),
                            a.messages,
                            size(a.run_bytes),
                            a.runs,
                            size(a.step_bytes),
                            size(a.event_bytes),
                            a.last_active.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "never".into())
                        ));
                    }
                    for w in &report.workspaces {
                        lines.push(format!(
                            "- Quest workspaces on {}{}: {} in {} folders, {} of it from closed quests that a cleanup would remove{}",
                            w.name,
                            if w.online { "" } else { " (offline)" },
                            size(w.bytes),
                            w.tasks,
                            size(w.reclaimable_bytes),
                            match (w.free_gb, w.low) {
                                (Some(free), true) => format!(" | only {free} GB of disk free, closed workspaces are cleared automatically below 5 GB"),
                                (Some(free), false) => format!(" | {free} GB of disk free"),
                                _ => String::new(),
                            }
                        ));
                    }
                    for c in report.cleanups.iter().take(5) {
                        let age = if c.older_than > 0 { format!(" older than {} days", c.older_than) } else { String::new() };
                        lines.push(format!("Past cleanup {}: {}{age}, freed {}", c.created_at.format("%Y-%m-%d"), c.kind, size(c.bytes_freed)));
                    }
                    return Ok(lines.join("\n"));
                }
                let kind = arg("kind")
                    .and_then(loka_shared::cleanup::CleanupKind::parse)
                    .ok_or_else(|| anyhow!("kind must be run_logs, chats, quests, activity or workspaces"))?;
                let days = match args.get("older_than_days").and_then(Value::as_i64) {
                    Some(days) => days as i32,
                    None if kind == loka_shared::cleanup::CleanupKind::Workspaces => 0,
                    None => bail!("older_than_days is required"),
                };
                let agent = match arg("agent") {
                    Some(n) => Some(self.managed_agent(n).await?.id),
                    None => None,
                };
                let apply = flag("confirm");
                if apply {
                    self.require_owner_asked().await?;
                }
                let result = self
                    .store
                    .clean_up(
                        loka_shared::cleanup::CleanupRequest {
                            owner,
                            org: self.caller.org_id,
                            agent,
                            kind,
                            older_than_days: days,
                            by_agent: Some(self.caller.id),
                        },
                        apply,
                    )
                    .await?;
                if result.kind == loka_shared::cleanup::CleanupKind::Workspaces {
                    return Ok(if result.rows == 0 {
                        "Nothing to clean: no closed quest workspaces on your person's devices (open quests are never touched, and a workspace stays an hour after its last run).".to_string()
                    } else if result.applied {
                        format!("Asked the devices to remove {} closed quest workspaces, about {}. They do it within a minute; branches, commits and pull requests stay. This cannot be undone.", result.rows, size(result.bytes))
                    } else {
                        format!(
                            "Preview: {} closed quest workspaces, about {}, on your person's devices. Nothing removed yet. Tell your person that open quests are kept and that this is permanent for those folders; call again with confirm true only after they agree.",
                            result.rows,
                            size(result.bytes)
                        )
                    });
                }
                Ok(if result.rows == 0 {
                    format!("Nothing to clean: no {} older than {days} days.", result.what)
                } else if result.applied {
                    format!("Deleted {} rows of {} older than {days} days, freeing about {}. This cannot be undone.", result.rows, result.what, size(result.bytes))
                } else {
                    format!(
                        "Preview: {} rows of {} older than {days} days, about {}. Nothing deleted yet. Tell your person what this removes and that it is permanent; call again with confirm true only after they agree.",
                        result.rows,
                        result.what,
                        size(result.bytes)
                    )
                })
            }
            "notifications" => {
                self.require(powers::OVERSEE)?;
                let owner = self.caller.owner_user_id.ok_or_else(|| anyhow!("you do not work for anyone"))?;
                let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20).clamp(1, 60) as usize;
                let bell = self.store.notifications(owner, self.caller.org_id, 60, false).await?;
                let names = self.store.list_agents_in(self.caller.org_id).await?;
                let unread_only = flag("unread_only");
                let picked: Vec<_> = bell.events.iter().filter(|e| !unread_only || bell.unread_seqs.contains(&e.seq)).collect();
                let lines: Vec<String> = picked[picked.len().saturating_sub(limit)..]
                    .iter()
                    .map(|e| {
                        let who = e.agent_id.and_then(|id| names.iter().find(|a| a.id == id)).map(|a| a.name.as_str()).unwrap_or("");
                        format!(
                            "- {}{} {} {who} {}",
                            if bell.unread_seqs.contains(&e.seq) { "[unread] " } else { "" },
                            e.created_at.format("%b %d %H:%M"),
                            e.event_type,
                            short(&e.payload.to_string(), 200)
                        )
                    })
                    .collect();
                let marked = match bell.events.iter().map(|e| e.seq).max() {
                    Some(seq) if flag("mark_seen") => {
                        self.require_owner_asked().await?;
                        self.store.clear_notifications(owner, self.caller.org_id, loka_shared::world::BellClear::UpTo(seq)).await?;
                        " Cleared everything."
                    }
                    _ => "",
                };
                Ok(if lines.is_empty() {
                    format!("Nothing in the bell.{marked}")
                } else {
                    format!("{} unread.{marked}\n{}", bell.unread, lines.join("\n"))
                })
            }
            other => bail!("unknown tool {other}"),
        }
    }
}

pub(super) fn folder_lines(capabilities: &Value) -> Vec<String> {
    capabilities
        .get("folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let name = f.get("name").and_then(Value::as_str)?;
            let id = f.get("id").and_then(Value::as_str)?;
            let path = f.get("path").and_then(Value::as_str).map(|p| format!(" at {p}")).unwrap_or_default();
            Some(format!("{name} ({}){path}", id.get(..8).unwrap_or(id)))
        })
        .collect()
}

fn pick_folder(capabilities: &Value, wanted: &str) -> Option<(Uuid, String)> {
    let wanted = wanted.trim().to_lowercase();
    let folders = loka_shared::runners::shared_folders(capabilities);
    let by_id = folders.iter().find(|(id, _)| wanted.len() >= 4 && id.to_string().starts_with(&wanted));
    let by_name = folders.iter().find(|(_, name)| name.to_lowercase() == wanted);
    by_id.or(by_name).cloned().or_else(|| {
        let partial: Vec<_> = folders.iter().filter(|(_, name)| name.to_lowercase().contains(&wanted)).collect();
        match partial.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    })
}

fn pick_milestone<'a>(list: &'a [Milestone], key: &str) -> Option<&'a Milestone> {
    let key = key.trim().trim_matches('"');
    if let Ok(position) = key.parse::<usize>() {
        return position.checked_sub(1).and_then(|i| list.get(i));
    }
    let lower = key.to_lowercase();
    list.iter()
        .find(|m| m.title.to_lowercase() == lower)
        .or_else(|| {
            let partial: Vec<_> = list.iter().filter(|m| m.title.to_lowercase().contains(&lower)).collect();
            match partial.as_slice() {
                [one] => Some(*one),
                _ => None,
            }
        })
        .filter(|_| !lower.is_empty())
}

fn uninstall_allowed(scope: &str, created_by: Option<Uuid>, owner: Uuid, admin: bool) -> bool {
    admin || (scope != "global" && created_by == Some(owner))
}

fn channel_message_id(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = t.strip_prefix("msg:").unwrap_or(t);
    t.strip_prefix("ch").unwrap_or(t).parse().ok().filter(|id| *id > 0)
}

fn duration_label(run: &SessionRun) -> String {
    let (Some(start), end) = (run.started_at, run.finished_at) else {
        return "not started".into();
    };
    let secs = (end.unwrap_or_else(chrono::Utc::now) - start).num_seconds().max(0);
    let text = if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    };
    if end.is_some() { text } else { format!("{text} so far") }
}

fn run_line(run: &SessionRun) -> String {
    let cause = match (run.status.as_str(), run.error.as_deref()) {
        ("failed" | "timed_out" | "cancelled", Some(e)) if !e.trim().is_empty() => {
            let described = loka_shared::cli_errors::describe(e);
            let summary = described.split(loka_shared::cli_errors::FRIENDLY_MARK).next().unwrap_or(&described).to_string();
            format!(" :: {}", short(&summary, 200))
        }
        _ => String::new(),
    };
    format!(
        "- {} {} [{}] {} {} ${:.2}{}{}",
        &run.id.to_string()[..8],
        run.kind,
        run.status,
        run.created_at.format("%b %d %H:%M"),
        duration_label(run),
        run.cost_usd,
        run.task_id.map(|t| format!(" quest {}", &t.to_string()[..8])).unwrap_or_default(),
        cause
    )
}

fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    format!("…{}", text.chars().skip(count - max).collect::<String>())
}

fn starter_preview(plan: &loka_shared::starter_team::Plan) -> String {
    let mut lines = Vec::new();
    let missing: Vec<_> = plan.seats.iter().filter(|s| !s.exists).collect();
    if missing.is_empty() && plan.complete {
        lines.push("The starter team is already in place:".to_string());
    } else {
        lines.push(format!("Would create {} agent{}:", missing.len(), if missing.len() == 1 { "" } else { "s" }));
    }
    for seat in &plan.seats {
        let state = if seat.exists { " (already here)" } else { "" };
        lines.push(format!("- {} {} ({}, {} {}){state}: {}.", seat.emoji, seat.name, seat.job_label, seat.cli, seat.model, seat.duty));
    }
    if let Some(coordinator) = &plan.coordinator {
        lines.push(format!("{coordinator} stays the coordinator. Everyone gets a notes skill and a nightly notes and reset, and a roster rule names each job."));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn run(status: &str, error: Option<&str>) -> SessionRun {
        let now = Utc::now();
        SessionRun {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            task_id: None,
            kind: "task".into(),
            status: status.into(),
            fork_from: None,
            prompt: String::new(),
            output: None,
            error: error.map(String::from),
            timeout_secs: 3600,
            cost_usd: 1.234,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            created_at: now,
            started_at: Some(now - Duration::seconds(125)),
            finished_at: Some(now),
        }
    }

    #[test]
    fn run_lines_show_cost_duration_and_failure_cause() {
        let ok = run_line(&run("completed", None));
        assert!(ok.contains("[completed]") && ok.contains("2m05s") && ok.contains("$1.23"), "{ok}");
        assert!(!ok.contains("::"));
        let failed = run_line(&run("failed", Some("You've hit your usage limit")));
        assert!(failed.contains(":: "), "{failed}");
        assert!(!failed.contains(loka_shared::cli_errors::FRIENDLY_MARK.trim()), "{failed}");
    }

    #[test]
    fn picks_shared_folders_by_name_or_id() {
        let id = Uuid::new_v4();
        let caps = json!({ "folders": [{ "id": id.to_string(), "name": "Notes", "path": "~/Notes" }, { "id": Uuid::new_v4().to_string(), "name": "Code" }] });
        assert_eq!(pick_folder(&caps, "notes").map(|f| f.0), Some(id));
        assert_eq!(pick_folder(&caps, &id.to_string()[..8]).map(|f| f.0), Some(id));
        assert_eq!(pick_folder(&caps, "not").map(|f| f.0), Some(id));
        assert!(pick_folder(&caps, "missing").is_none());
        assert!(pick_folder(&json!({}), "notes").is_none());
        assert_eq!(folder_lines(&caps)[0], format!("Notes ({}) at ~/Notes", &id.to_string()[..8]));
    }

    #[test]
    fn parses_channel_message_refs() {
        assert_eq!(channel_message_id("msg:ch42"), Some(42));
        assert_eq!(channel_message_id("ch7"), Some(7));
        assert_eq!(channel_message_id("9"), Some(9));
        assert_eq!(channel_message_id("msg:abc"), None);
        assert_eq!(channel_message_id("0"), None);
    }

    fn milestone(title: &str, position: i32) -> Milestone {
        let now = Utc::now();
        Milestone {
            id: Uuid::new_v4(),
            task_id: Uuid::nil(),
            position,
            title: title.into(),
            kind: "work".into(),
            status: "pending".into(),
            detail: None,
            data: json!({}),
            next_check_at: None,
            done_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn picks_milestones_by_position_or_title() {
        let list = vec![milestone("Write tests", 0), milestone("Ship it", 1), milestone("PR merged", 1000)];
        assert_eq!(pick_milestone(&list, "2").map(|m| m.title.as_str()), Some("Ship it"));
        assert_eq!(pick_milestone(&list, "3").map(|m| m.title.as_str()), Some("PR merged"));
        assert!(pick_milestone(&list, "0").is_none());
        assert!(pick_milestone(&list, "9").is_none());
        assert_eq!(pick_milestone(&list, "write TESTS").map(|m| m.position), Some(0));
        assert_eq!(pick_milestone(&list, "merged").map(|m| m.position), Some(1000));
        assert!(pick_milestone(&list, "i").is_none());
        assert!(pick_milestone(&list, "").is_none());
    }

    #[test]
    fn uninstall_needs_own_install_or_admin() {
        let owner = Uuid::new_v4();
        assert!(uninstall_allowed("agent", Some(owner), owner, false));
        assert!(!uninstall_allowed("agent", Some(Uuid::new_v4()), owner, false));
        assert!(!uninstall_allowed("global", Some(owner), owner, false));
        assert!(uninstall_allowed("global", None, owner, true));
    }

    #[test]
    fn tails_keep_the_end() {
        assert_eq!(tail("abcdef", 3), "…def");
        assert_eq!(tail("abc", 3), "abc");
    }
}

