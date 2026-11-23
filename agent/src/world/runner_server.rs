use std::collections::{BTreeMap, HashSet};

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use loka_shared::{
    db::Database,
    guests::{asked_by_user, in_thread},
    milestones::{finished_status, MilestoneStore},
    models::AgentTask,
    runners::{
        ApplyCompletion, ClaimedRun, LeaseContext, RunnerCompletion, RunnerEvent, RunnerFile, RunnerImage,
        RunnerJob, RunnerPublish, RunnerSkill, RunnerStore, RunnerUsage, RunnerWorkspace,
    },
    world::{
        AgentSession, FollowupAdvance, InboxItem, NewEvent, NewMessage, NewRun, NewSession, ReviewerChoice, SessionRun,
        TaskResume, WorldAgent, WorldStore,
    },
};
use serde_json::{json, Map, Value};
use std::str::FromStr;
use uuid::Uuid;

use crate::prompts::{self, Action, Report};
use crate::quests::{finish_error, finish_step, followup_step, incomplete_report, salvages, Finish, FollowupStep, Started};
use loka_shared::run_causes::{Cause, Required};

pub const LOST_REPLY_GRACE_SECS: i64 = 20;

const TURN_BATCH: i64 = 20;

fn quick_answer(output: &str) -> String {
    let text = prompts::plain_dashes(&prompts::split_actions(output).0);
    if text.trim().is_empty() { "Done.".to_string() } else { text }
}

#[derive(Clone)]
pub struct RunnerCoordinator {
    db: Database,
    store: WorldStore,
    runners: RunnerStore,
    owner_name: String,
    reviews_enabled: bool,
    max_review_rounds: i32,
}

struct AttentionCard {
    choices: Vec<String>,
    drafts: Vec<prompts::Draft>,
    clarify_request: Option<Value>,
}

impl RunnerCoordinator {
    pub fn new(db: Database, store: WorldStore) -> Self {
        Self {
            runners: RunnerStore::new(store.pool().clone()),
            db,
            store,
            owner_name: std::env::var("WORLD_OWNER_NAME").unwrap_or_else(|_| "the team".into()),
            reviews_enabled: std::env::var("WORLD_REVIEWS")
                .map(|value| value != "off")
                .unwrap_or(true),
            max_review_rounds: env_i32("WORLD_MAX_REVIEW_ROUNDS", 2),
        }
    }

    pub async fn prepare_turn(&self, runner_id: Uuid) -> Result<()> {
        let Some(agent_id) = self.runners.claim_inbox_agent(runner_id).await? else {
            return Ok(());
        };
        let result = self.prepare_claimed_turn(runner_id, agent_id).await;
        self.runners
            .release_inbox_agent(runner_id, agent_id)
                .await?;
        result
    }

    pub async fn tick_schedules(&self) -> Result<()> {
        self.store.fire_due_timers().await?;
        for schedule in self.store.runner_schedules_missing_next_run().await? {
            let (next, disable) = next_fire(&schedule, Utc::now());
            self.store
                .set_schedule_next_run(schedule.id, next, disable && next.is_none())
                .await?;
        }
        for schedule in self.store.claim_due_runner_schedules().await? {
            self.store.fire_schedule(&schedule).await?;
            let after = Utc::now() + chrono::Duration::seconds(1);
            let (next, disable) = if schedule.cron_expression.is_some() {
                next_fire(&schedule, after)
            } else {
                (None, true)
            };
            self.store
                .set_schedule_next_run(schedule.id, next, disable)
                .await?;
        }
        Ok(())
    }

    pub async fn stop_run(&self, run_id: Uuid, by: &str) -> Result<bool> {
        let Some(on_runner) = self.store.request_stop(run_id, by).await? else {
            return Ok(false);
        };
        if on_runner {
            let error = format!("Stopped by {by}. Do not restart this work unless they ask.");
            if self.store.stop_runner_run(run_id, &error).await? {
                self.handle_expired(&[run_id]).await?;
            }
        }
        Ok(true)
    }

    pub async fn handle_expired(&self, run_ids: &[Uuid]) -> Result<()> {
        for run_id in run_ids {
            let Some(run) = self.store.get_run(*run_id).await? else {
                continue;
            };
            if !matches!(run.status.as_str(), "failed" | "timed_out") {
                continue;
            }
            let Some(session) = self.store.get_session(run.session_id).await? else {
                continue;
            };
            let Some(agent) = self.store.get_agent(run.agent_id).await? else {
                continue;
            };
            let error = run.error.as_deref().unwrap_or("runner lease expired");
            self.fail_run(
                run.id,
                &run.kind,
                &session,
                &agent,
                error,
                run.status == "timed_out",
            )
            .await?;
        }
        Ok(())
    }

    async fn prepare_claimed_turn(&self, _runner_id: Uuid, agent_id: Uuid) -> Result<()> {
        let agent = self
            .store
            .get_agent(agent_id)
            .await?
            .ok_or_else(|| anyhow!("agent {agent_id} no longer exists"))?;
        if let Err(error) = self.store.dispatch_guest_inbox(agent_id).await {
            tracing::warn!(agent = %agent.slug, %error, "Could not hand guest messages to an aside");
        }
        let items = self.store.take_pending_inbox(agent_id, TURN_BATCH).await?;
        if items.is_empty() {
            return Ok(());
        }
        let (reset, rest) = loka_shared::world::partition_reset_items(items);
        if !reset.is_empty() {
            let rest_ids: Vec<i64> = rest.iter().map(|item| item.id).collect();
            self.store.repend_inbox(&rest_ids).await?;
            let reset_ids: Vec<i64> = reset.iter().map(|item| item.id).collect();
            return match self.queue_reset(&agent).await {
                Ok(Some(run)) => {
                    self.runners.mark_run_inbox(run.id, &reset_ids).await?;
                    self.store.set_agent_status(agent.id, "thinking", None).await?;
                    Ok(())
                }
                Ok(None) => self.store.finish_inbox(&reset_ids, "done", None).await,
                Err(error) => {
                    self.store.requeue_inbox(&reset_ids, &loka_shared::cli_errors::chain(&error)).await?;
                    Err(error)
                }
            };
        }
        let (compact, rest) = loka_shared::context::partition_compact_items(rest);
        if !compact.is_empty() {
            let compact_ids: Vec<i64> = compact.iter().map(|item| item.id).collect();
            let rest_ids: Vec<i64> = rest.iter().map(|item| item.id).collect();
            match self.queue_compact(&agent).await {
                Ok(Some(run)) => {
                    self.store.repend_inbox(&rest_ids).await?;
                    self.runners.mark_run_inbox(run.id, &compact_ids).await?;
                    self.store.set_agent_status(agent.id, "thinking", None).await?;
                    let _ = self.store.announce_context(agent.id).await;
                    return Ok(());
                }
                Ok(None) => {
                    self.store.finish_inbox(&compact_ids, "done", None).await?;
                    let _ = self.store.announce_context(agent.id).await;
                    if rest.is_empty() {
                        return Ok(());
                    }
                }
                Err(error) => {
                    self.store.requeue_inbox(&compact_ids, &loka_shared::cli_errors::chain(&error)).await?;
                    self.store.repend_inbox(&rest_ids).await?;
                    return Err(error);
                }
            }
        }
        let ids: Vec<i64> = rest.iter().map(|item| item.id).collect();
        let result = self.queue_turn(&agent, &rest).await;
        match result {
            Ok(run) => {
                self.runners.mark_run_inbox(run.id, &ids).await?;
                self.store
                    .set_agent_status(agent.id, "thinking", None)
                    .await?;
                let typing = self.store.turn_typing_places(&agent, &rest).await;
                let _ = self.store.set_typing(&agent, &typing, true, Some(run.id), None).await;
                let _ = self.store.mark_seen(&agent, &rest).await;
                Ok(())
            }
            Err(error) => {
                self.store.requeue_inbox(&ids, &loka_shared::cli_errors::chain(&error)).await?;
                Err(error)
            }
        }
    }

    async fn queue_reset(&self, agent: &WorldAgent) -> Result<Option<SessionRun>> {
        let main = match self.store.main_session(agent.id).await? {
            Some(session) => session,
            None => {
                let key = format!("agents/{}", agent.id);
                self.store
                    .create_session(NewSession {
                        agent_id: agent.id,
                        role: "main",
                        backend: &agent.cli,
                        model: &agent.model,
                        parent_session_id: None,
                        task_id: None,
                        working_dir: Some(&key),
                        usage_baseline: json!({}),
                    })
                    .await?
            }
        };
        if main.cli_session_id.is_none() && self.store.last_run_id(main.id).await?.is_none() {
            return Ok(None);
        }
        let run = self
            .store
            .create_run(NewRun {
                session_id: main.id,
                agent_id: agent.id,
                task_id: None,
                kind: "turn",
                status: "queued",
                fork_from: None,
                prompt: prompts::RESET_PROMPT,
                timeout_secs: 600,
                request: None,
            })
            .await?;
        Ok(Some(run))
    }

    async fn queue_compact(&self, agent: &WorldAgent) -> Result<Option<SessionRun>> {
        let Some(main) = self.store.main_session(agent.id).await? else { return Ok(None) };
        if main.cli_session_id.is_none() || loka_shared::context::compact_unsupported(&main.backend).is_some() {
            return Ok(None);
        }
        let run = self
            .store
            .create_run(NewRun {
                session_id: main.id,
                agent_id: agent.id,
                task_id: None,
                kind: "turn",
                status: "queued",
                fork_from: None,
                prompt: prompts::COMPACT_PROMPT,
                timeout_secs: 600,
                request: None,
            })
            .await?;
        Ok(Some(run))
    }

    async fn after_reset(&self, agent: &WorldAgent, main: &AgentSession, run_id: Uuid, runner_id: Uuid) -> Result<()> {
        let run = self
            .store
            .get_run(run_id)
            .await?
            .ok_or_else(|| anyhow!("reset run disappeared"))?;
        let since = run.started_at.unwrap_or(run.created_at);
        if !self.store.notes_updated_since(agent.id, agent.owner_user_id, since).await? {
            self.system_note(
                agent,
                "Context reset aborted: the notes were not saved (update_my_notes was not called). Nothing was forgotten; ask again when ready.",
            )
            .await?;
            self.runners.finish_run_inbox(run_id, "done", None).await?;
            self.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await?;
            let _ = self.store.announce_context(agent.id).await;
            return Ok(());
        }
        self.reset_with_saved_notes(agent, main, run_id).await?;
        self.prepare_claimed_turn(runner_id, agent.id).await?;
        Ok(())
    }

    async fn reset_with_saved_notes(&self, agent: &WorldAgent, main: &AgentSession, run_id: Uuid) -> Result<()> {
        let notes = self
            .store
            .own_notes_skill(agent.id, agent.owner_user_id)
            .await?
            .map(|skill| skill.body)
            .unwrap_or_default();
        let fresh = self.store.restart_main_session(agent.id, &agent.cli, &agent.model, &notes).await?;
        self.store.cancel_queued_turns(main.id).await?;
        self.store.mark_context_reset(agent.id, fresh.id, main.id).await?;
        self.runners.finish_run_inbox(run_id, "done", None).await?;
        self.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await?;
        tracing::info!(agent = %agent.slug, old = %main.id, new = %fresh.id, "Main session reset with saved notes");
        let _ = self.store.announce_context(agent.id).await;
        Ok(())
    }

    async fn queue_turn(&self, agent: &WorldAgent, items: &[InboxItem]) -> Result<SessionRun> {
        let main = match self.store.main_session(agent.id).await? {
            Some(session) if session.backend != agent.cli => {
                bail!("changing an active agent's CLI requires an explicit session handoff")
            }
            Some(session) => {
                if session.model != agent.model {
                    self.store
                        .set_session_model(session.id, &agent.model)
                        .await?;
                    self.store.get_session(session.id).await?.unwrap_or(session)
                } else {
                    session
                }
            }
            None => {
                let key = format!("agents/{}", agent.id);
                self.store
                    .create_session(NewSession {
                        agent_id: agent.id,
                        role: "main",
                        backend: &agent.cli,
                        model: &agent.model,
                        parent_session_id: None,
                        task_id: None,
                        working_dir: Some(&key),
                        usage_baseline: json!({}),
                    })
                    .await?
            }
        };
        let tasks = self.referenced_tasks(items).await;
        let rules = self.store.rules_for(agent.owner_user_id, agent.org_id).await?;
        let skills = self.store.skills_for(agent.id).await?;
        let skills_index = if skills.is_empty() {
            String::new()
        } else {
            format!(
                "Skills available in this job. Read the matching supplied skill before using it:\n{}",
                skills
                    .iter()
                    .map(|skill| format!("- {}: {}", skill.name, skill.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let teams = self.store.teams_of_agent(agent.id).await.unwrap_or_default();
        let rules_text = [
            prompts::rules(&rules, "talk", &self.owner_name),
            prompts::before_big_work(self.store.agent_before_big_work(agent).await?).to_string(),
            prompts::teams(&teams, agent.id, "talk"),
            skills_index,
        ]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        let system = prompts::persona(agent, &self.owner_name);
        let rules_hash = prompts::rules_hash(&format!("{system}\n{rules_text}"));
        let mut prompt = prompts::turn(items, &tasks);
        if loka_shared::quests::owner_spoke(items) {
            let note = loka_shared::quests::waiting_plans_note(&self.store.waiting_plans(agent.id).await.unwrap_or_default());
            if !note.is_empty() {
                prompt = format!("{prompt}\n\n{note}");
            }
        }
        if main.cli_session_id.is_some() {
            let last = prompts::last_replies(&self.store.last_turn_replies(agent.id, main.id).await.unwrap_or_default());
            if !last.is_empty() {
                prompt = format!("{last}\n\n{prompt}");
            }
        }
        prompt = prompts::prepend_memory_handoff(
            &prompt,
            main.summary.as_deref(),
            main.cli_session_id.is_none(),
        );
        if main.rules_hash.as_deref() != Some(rules_hash.as_str()) {
            let instructions = if main.cli_session_id.is_none() {
                rules_text.clone()
            } else {
                [system.as_str(), rules_text.as_str()]
                    .into_iter()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            prompt = format!("[instructions updated]\n{instructions}\n\n{prompt}");
        }
        let run = self
            .store
            .create_run(NewRun {
                session_id: main.id,
                agent_id: agent.id,
                task_id: None,
                kind: "turn",
                status: "queued",
                fork_from: None,
                prompt: &prompt,
                timeout_secs: 600,
                request: None,
            })
            .await?;
        self.runners.set_run_rules_hash(run.id, &rules_hash).await?;
        Ok(run)
    }

    pub async fn build_job(&self, claimed: ClaimedRun, mcp_token: String) -> Result<RunnerJob> {
        let (session, agent, runner, task, parent_cli, skills, (images, files, skipped_note), options, previous_run_id, live_sessions, keys_version) = tokio::try_join!(
            async {
                self.store
                    .get_session(claimed.session_id)
                    .await?
                    .ok_or_else(|| anyhow!("session {} disappeared", claimed.session_id))
            },
            async {
                self.store
                    .get_agent(claimed.agent_id)
                    .await?
                    .ok_or_else(|| anyhow!("agent {} disappeared", claimed.agent_id))
            },
            async {
                self.runners
                    .get_live(claimed.runner_id)
                    .await?
                    .ok_or_else(|| anyhow!("runner {} disappeared", claimed.runner_id))
            },
            async {
                match claimed.task_id {
                    Some(id) => self.db.get_task(id).await,
                    None => Ok(None),
                }
            },
            async {
                Ok::<_, anyhow::Error>(match claimed.fork_from {
                    Some(parent) => self.store.get_session(parent).await?.and_then(|session| session.cli_session_id),
                    None => None,
                })
            },
            async {
                Ok::<_, anyhow::Error>(
                    self.store
                        .skills_for(claimed.agent_id)
                        .await?
                        .into_iter()
                        .map(|skill| RunnerSkill { name: skill.name, body: skill.body })
                        .collect::<Vec<_>>(),
                )
            },
            async {
                if claimed.kind == "turn" {
                    self.run_uploads(claimed.run_id).await
                } else {
                    Ok((Vec::new(), Vec::new(), None))
                }
            },
            async {
                Ok::<_, anyhow::Error>(match (claimed.kind.as_str(), claimed.task_id) {
                    ("turn", _) => self.turn_options(claimed.run_id, &claimed.backend).await?,
                    (kind @ ("task" | "followup" | "review"), Some(task_id)) => {
                        let quest = self.store.quest_options(task_id).await?;
                        TurnOptions {
                            model: None,
                            effort: if kind == "review" { None } else { quest.effort_for(&claimed.backend) },
                            fast: quest.fast_for(&claimed.backend),
                        }
                    }
                    _ => TurnOptions::default(),
                })
            },
            self.store.previous_run_id(claimed.session_id, claimed.run_id),
            self.runners.live_cli_sessions(claimed.runner_id),
            self.store.key_version(claimed.agent_id),
        )?;
        if agent.owner_user_id != Some(claimed.owner_user_id) {
            bail!("runner owner no longer owns this agent");
        }
        if runner.owner_user_id != claimed.owner_user_id {
            bail!("runs only go to their owner's own device");
        }
        let quick_source = if claimed.kind == "quick" && claimed.fork_from.is_none() && claimed.cli_session_id.is_none() {
            self.store.quick_fork_source(&session).await?
        } else {
            None
        };
        let quick_fork = quick_source.as_ref().map(|(_, cli)| cli.clone());
        if let Some(parent) = claimed.fork_from.filter(|_| parent_cli.is_some()).or(quick_source.as_ref().map(|(id, _)| *id)) {
            self.store.take_fork_baseline(claimed.session_id, parent).await?;
        }
        let parent_cli = parent_cli.or(quick_fork.clone());
        let workspace = workspace_for(
            &session,
            task.as_ref(),
            &claimed.kind,
            claimed.fork_from.is_some(),
        )?;
        let rewind = if claimed.kind == "turn" {
            loka_shared::world::Rewind::from(session.rewind.as_ref())
        } else {
            loka_shared::world::Rewind::None
        };
        let rewinding = !matches!(rewind, loka_shared::world::Rewind::None);
        let rewind_to = rewind.anchor().map(String::from);
        let mut cli_session_id = match &rewind {
            loka_shared::world::Rewind::At { from, .. } => Some(from.clone()),
            _ => parent_cli.or(claimed.cli_session_id.clone()),
        };
        let fork = claimed.fork_from.is_some() || quick_fork.is_some();
        let resume = cli_session_id.is_some();
        if cli_session_id.is_none() && claimed.backend == "claude" {
            cli_session_id = Some(Uuid::new_v4().to_string());
        }
        let on_mac = runner.platform == "macos"
            || runner.capabilities.get("platform").and_then(Value::as_str) == Some("macos");
        let (agent_keys, omp, provider_keys, publish) = tokio::try_join!(
            self.store.keys_for_agent(agent.id),
            async {
                if claimed.kind == "publish" {
                    Ok(None)
                } else {
                    self.store.resolved_omp_config(agent.id, &claimed.backend).await
                }
            },
            async {
                if claimed.kind == "publish" || on_mac {
                    Ok(BTreeMap::new())
                } else {
                    self.store.provider_keys_for_agent(agent.id).await
                }
            },
            async {
                if claimed.kind == "publish" {
                    let task = task.as_ref().ok_or_else(|| anyhow!("publish run has no task"))?;
                    Ok(Some(self.publish_payload(task, &session).await?))
                } else {
                    Ok(None)
                }
            },
        )?;
        let all_environment = agent_keys.values;
        let gets_environment = matches!(claimed.kind.as_str(), "task" | "followup" | "direct" | "quick");
        let environment = if gets_environment {
            self.store.without_mcp_secrets(agent.id, all_environment.clone()).await?
        } else {
            BTreeMap::new()
        };
        let all_vault_refs = agent_keys.vault_refs;
        let vault_refs = if gets_environment {
            self.store.without_mcp_secrets(agent.id, all_vault_refs.clone()).await?
        } else {
            BTreeMap::new()
        };
        if let Some(name) = vault_refs.keys().next() {
            if !on_mac {
                bail!("Vault reference for {name} could not be read: vault references are read only on your own Mac, and this agent runs on a device that is not one. Move the agent to your Mac or store the value itself.");
            }
            if runner.capabilities.get("vault_refs").and_then(Value::as_bool) != Some(true) {
                bail!("Vault reference for {name} could not be read: this Mac runs an older Loka app that cannot read vault references. Update the Loka app on it.");
            }
        }
        let mcp_servers = if claimed.kind == "aside" {
            json!({})
        } else {
            self.mcp_servers(agent.id, claimed.kind == "review", &all_environment, &all_vault_refs)
                .await?
        };
        let system_prompt = match claimed.kind.as_str() {
            "turn" => Some(prompts::persona(&agent, &self.owner_name)),
            "direct" => Some(direct_system(&agent, &self.owner_name, session.title.as_deref(), &self.work_rules(&agent).await?)),
            "quick" => Some(prompts::quick_system(
                &agent,
                &loka_shared::quick::asker_name(&session),
                loka_shared::quick::thread_root(&session).is_some(),
                &self.quick_rules(&agent).await?,
            )),
            _ => None,
        };
        let setup_scan = loka_shared::setup_scan::requested(session.asked_by.as_ref());
        if setup_scan.is_some()
            && (!on_mac
                || claimed.kind != "aside"
                || runner.capabilities.get("setup_scan").and_then(Value::as_bool) != Some(true))
        {
            bail!("setup suggestions only run on their owner's own Mac");
        }
        let owner_aside = claimed.kind == "aside"
            && !loka_shared::world::toolless_aside(session.asked_by.as_ref());
        let (lean, max_turns) = match claimed.kind.as_str() {
            "turn" => (true, 8),
            "aside" if owner_aside => (true, 8),
            "aside" => (true, 3),
            "quick" => (false, loka_shared::quick::MAX_TURNS),
            "review" => (false, 60),
            "publish" => (true, 1),
            _ => (false, 200),
        };
        let previous_run_id = if rewinding { None } else { previous_run_id };
        let approval = if claimed.kind == "aside" { "full".to_string() } else { agent.approval_mode.clone() };
        let live_sessions = Some(live_sessions);
        let keys_version = Some(keys_version);
        let command_rules = if matches!(claimed.kind.as_str(), "task" | "followup" | "direct" | "review" | "quick") {
            Some(self.store.command_policy_for_agent(agent.id).await?).filter(|policy| !policy.is_empty())
        } else {
            None
        };
        let approved_actions = if !lean && matches!(claimed.kind.as_str(), "task" | "followup" | "direct" | "review" | "quick") {
            loka_shared::approvals::from_rules(&self.store.rules_for(agent.owner_user_id, agent.org_id).await?)
        } else {
            Vec::new()
        };
        let (omp_models, omp_secrets) = match omp {
            Some(omp) => (Some(omp.models_yml), omp.secrets),
            None => (None, Vec::new()),
        };
        Ok(RunnerJob {
            lease_id: claimed.lease_id,
            run_id: claimed.run_id,
            session_id: claimed.session_id,
            agent_id: claimed.agent_id,
            runner_id: claimed.runner_id,
            kind: claimed.kind,
            cli: claimed.backend,
            model: options.model.unwrap_or(claimed.model),
            prompt: match skipped_note {
                Some(note) => format!("{}\n\n{note}", claimed.prompt),
                None => claimed.prompt,
            },
            system_prompt,
            workspace,
            cli_session_id,
            resume,
            fork,
            lean,
            max_turns,
            timeout_secs: claimed.timeout_secs.max(1) as u64,
            effort: options.effort,
            fast: options.fast,
            mcp_token,
            previous_run_id,
            mcp_servers,
            skills,
            environment,
            vault_refs,
            provider_keys,
            images,
            files,
            publish,
            rewind_to,
            approval: Some(approval),
            private: agent.visibility == "private",
            live_sessions,
            world_tools: owner_aside,
            omp_models,
            omp_secrets,
            command_rules,
            approved_actions,
            owner_id: agent.owner_user_id,
            keys_version,
            metered: self.store.metered(agent.id).await?,
            setup_scan,
            task_id: claimed.task_id,
        })
    }

    pub async fn complete(
        &self,
        lease: &LeaseContext,
        completion: &RunnerCompletion,
    ) -> Result<()> {
        let session = self
            .store
            .get_session(lease.session_id)
            .await?
            .ok_or_else(|| anyhow!("session disappeared"))?;
        let agent = self
            .store
            .get_agent(lease.agent_id)
            .await?
            .ok_or_else(|| anyhow!("agent disappeared"))?;
        let expected = session
            .working_dir
            .as_deref()
            .ok_or_else(|| anyhow!("session has no quest folder"))?;
        if expected != completion.workspace_key {
            bail!("completion workspace does not match the leased session")
        }
        let environment = self.store.secrets_for_agent(agent.id).await?;
        let redactor = loka_shared::redaction::Redactor::new(environment.values());
        let output = redactor.redact(&completion.output);
        let error = completion
            .error
            .as_deref()
            .map(|value| redactor.redact(value));
        if error.is_some() || completion.exit_code != 0 && output.trim().is_empty() {
            let message = error
                .unwrap_or_else(|| format!("runner process exited with {}", completion.exit_code));
            if self.runners.requeue_for_space(lease, &message).await? {
                tracing::info!(run = %lease.run_id, kind = %lease.kind, "Run refused for lack of disk space goes back to the queue to wait for room");
                return Ok(());
            }
            if lease.kind == "turn"
                && !completion.retryable
                && session.rewind.as_ref().is_some_and(|r| r.get("fresh").is_none())
            {
                self.store.fall_back_to_replay(session.id).await?;
            }
            if self
                .runners
                .apply_failure(lease, &message, completion.session_id.as_deref())
                .await?
                == ApplyCompletion::Stopped
            {
                bail!("runner lease stopped before completion was applied")
            }
            if self
                .runners
                .lease_for_completion(lease.runner_id, lease.lease_id)
                .await?
                .is_none()
            {
                bail!("runner lease stopped during completion")
            }
            self.fail_run(
                lease.run_id,
                &lease.kind,
                &session,
                &agent,
                &message,
                false,
            )
                .await?;
            return Ok(());
        }
        let compaction = self.is_compaction(lease.run_id, &lease.kind).await?;
        let reset = self.is_reset(lease.run_id, &lease.kind).await?;
        let context_tokens = match completion.usage.context_tokens {
            _ if lease.kind == "publish" => None,
            _ if compaction => Some(0),
            measured => Some(loka_shared::context::used(measured, completion.usage.input_tokens, completion.usage.cached_input_tokens)),
        };
        let reported = loka_shared::run_recording::accounts_spend(&lease.kind).then(|| reported(completion));
        let applied = self
            .runners
            .apply_success(lease, &output, completion.session_id.as_deref(), reported.as_ref(), context_tokens)
            .await?;
        if applied == ApplyCompletion::Stopped {
            bail!("runner lease stopped before completion was applied")
        }
        if self
            .runners
            .lease_for_completion(lease.runner_id, lease.lease_id)
            .await?
            .is_none()
        {
            bail!("runner lease stopped during completion")
        }
        if !prompts::contract_open(&lease.kind, &session, &output) {
            self.store.announce_completed(lease.run_id).await?;
        }
        if lease.kind == "turn" {
            self.store
                .set_run_checkpoint(lease.run_id, completion.checkpoint.as_deref())
                .await?;
            if session.rewind.is_some() {
                self.store.clear_session_rewind(session.id).await?;
            }
        }
        let settled = async {
            match lease.kind.as_str() {
                "turn" if reset => self.after_reset(&agent, &session, lease.run_id, lease.runner_id).await?,
                "turn" if compaction => self.after_compaction(&agent, &session, lease.run_id, lease.runner_id).await?,
                "turn" => {
                    let replied = self.after_turn(&agent, &session, lease.run_id, &output).await;
                    self.stop_typing(&agent, lease.run_id).await;
                    replied?;
                    let _ = self.store.announce_context(agent.id).await;
                    if !self.compact_if_large(&agent, &session, lease.run_id, completion).await? {
                        if let Err(e) = self.store.auto_context_cleanup(agent.id, lease.run_id).await {
                            tracing::warn!(agent = %agent.slug, error = %e, "Automatic context cleanup check failed");
                        }
                    }
                }
                "task" => {
                    self.after_task(&agent, &session, lease.run_id, &output, completion.work_summary.as_deref())
                        .await?
                }
                "followup" => {
                    self.after_followup(
                        &agent,
                        &session,
                        lease.run_id,
                        &output,
                        completion.work_summary.as_deref(),
                        loka_shared::runners::PrSync::from_report(completion.publish_pr_state.as_deref()),
                    )
                        .await?
                }
                "aside" => self.after_aside(&agent, &session, &output).await?,
                "quick" => self.after_quick(&agent, &session, lease.run_id, &output).await?,
                "direct" => {
                    self.store
                        .set_session_status(session.id, "active", Some(&tail(&prompts::plain_dashes(&output), 2000)))
                        .await?;
                }
                "review" => {
                    self.after_review(&agent, &session, lease.run_id, &output)
                        .await?
                }
                "publish" => self.after_publish(&agent, &session, completion).await?,
                other => bail!("unsupported runner job kind {other}"),
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let announced = self.store.announce_completed(lease.run_id).await;
        settled.and(announced.map(|_| ()))
    }

    pub async fn keep_late_spend(&self, runner_id: Uuid, lease_id: Uuid, completion: &RunnerCompletion) -> Result<bool> {
        let Some(lease) = self.runners.late_lease(runner_id, lease_id).await? else {
            return Ok(false);
        };
        if !loka_shared::run_recording::accounts_spend(&lease.kind) {
            return Ok(false);
        }
        self.runners.keep_late_spend(&lease, &reported(completion)).await
    }

    pub async fn redact_completion(
        &self,
        lease: &LeaseContext,
        completion: &RunnerCompletion,
    ) -> Result<RunnerCompletion> {
        let environment = self.store.secrets_for_agent(lease.agent_id).await?;
        let redactor = loka_shared::redaction::Redactor::new(environment.values());
        let mut completion = completion.clone();
        completion.output = redactor.redact(&completion.output);
        completion.error = completion.error.as_deref().map(|value| redactor.redact(value));
        Ok(completion)
    }

    pub async fn mcp(&self, lease: &LeaseContext, message: &Value) -> Result<Option<Value>> {
        let read_only = match lease.kind.as_str() {
            "turn" | "aside" | "quick" => false,
            "task" | "followup" => true,
            _ => bail!("the Loka MCP server is only available to main turns, asides and task sessions"),
        };
        let caller = self
            .store
            .get_agent(lease.agent_id)
            .await?
            .ok_or_else(|| anyhow!("leased agent disappeared"))?;
        if caller.owner_user_id != Some(lease.owner_user_id) {
            bail!("leased agent owner changed")
        }
        let quest = if read_only { self.store.get_session(lease.session_id).await?.and_then(|s| s.task_id) } else { None };
        let session = quest.is_some().then_some(lease.session_id);
        let quick = (lease.kind == "quick").then_some(lease.session_id);
        let server = crate::mcp::Server::new(self.store.clone(), self.db.clone(), caller).in_quest(quest, session).in_quick(quick).in_aside(lease.kind == "aside");
        let server = if read_only { server.read_only() } else { server };
        let server = if self.store.runner_owner(lease.runner_id).await? == Some(lease.owner_user_id) {
            server
        } else {
            server.on_shared_device()
        };
        Ok(server.jsonrpc(message).await)
    }

    pub async fn ingest_events(&self, lease: &LeaseContext, events: Vec<RunnerEvent>) -> Result<()> {
        let (usage, shown): (Vec<_>, Vec<_>) = events
            .into_iter()
            .partition(|event| event.kind == loka_shared::runners::USAGE_EVENT);
        if !usage.is_empty() {
            if let Err(error) = self.record_live_usage(lease, &usage).await {
                tracing::warn!(error = %format!("{error:#}"), run = %lease.run_id, "Could not record live usage");
            }
        }
        let events = self.redact_events(lease, &shown).await?;
        if events.is_empty() {
            return Ok(());
        }
        self.runners.record_events(lease, &events).await?;
        let Some(text) = events
            .iter()
            .rev()
            .find_map(|event| loka_shared::progress::from_event(&event.kind, event.tool.as_deref(), &event.text))
        else {
            return Ok(());
        };
        match lease.kind.as_str() {
            "turn" => {
                let _ = self.store.emit_progress(lease.agent_id, lease.run_id, "turn", &text).await;
            }
            kind if !loka_shared::run_recording::shows_activity(kind) => {}
            _ => {
                if let Some(run) = self.store.get_run(lease.run_id).await? {
                    let _ = self.store.set_run_activity(&run, &text).await;
                }
            }
        }
        Ok(())
    }

    pub async fn redact_events(
        &self,
        lease: &LeaseContext,
        events: &[RunnerEvent],
    ) -> Result<Vec<RunnerEvent>> {
        let environment = self.store.secrets_for_agent(lease.agent_id).await?;
        let redactor = loka_shared::redaction::Redactor::new(environment.values());
        events
            .iter()
            .map(|event| {
                if !matches!(event.kind.as_str(), "tool" | "result" | "text" | "error" | loka_shared::runners::USAGE_EVENT) {
                    bail!("invalid runner event kind")
                }
                Ok(RunnerEvent {
                    kind: event.kind.clone(),
                    tool: event
                        .tool
                        .as_ref()
                        .map(|tool| redactor.redact(tool)),
                    text: redactor.redact(&event.text),
                })
            })
            .collect()
    }

    async fn is_compaction(&self, run_id: Uuid, kind: &str) -> Result<bool> {
        if kind != "turn" {
            return Ok(false);
        }
        Ok(self
            .store
            .get_run(run_id)
            .await?
            .is_some_and(|run| run.prompt == prompts::COMPACT_PROMPT))
    }

    async fn is_reset(&self, run_id: Uuid, kind: &str) -> Result<bool> {
        if kind != "turn" {
            return Ok(false);
        }
        Ok(self
            .store
            .get_run(run_id)
            .await?
            .is_some_and(|run| run.prompt == prompts::RESET_PROMPT))
    }

    async fn compact_if_large(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        run_id: Uuid,
        completion: &RunnerCompletion,
    ) -> Result<bool> {
        let used = completion.usage.context_tokens as i64;
        let Some(limit) = prompts::compact_threshold() else { return Ok(false) };
        if used <= limit || !prompts::compacts(&main.backend) || completion.session_id.is_none() {
            return Ok(false);
        }
        if !self.store.should_compact(main.id, run_id, used, limit).await? {
            return Ok(false);
        }
        tracing::info!(agent = %agent.slug, tokens = used, "Queueing main session compaction");
        self.store
            .create_run(NewRun {
                session_id: main.id,
                agent_id: agent.id,
                task_id: None,
                kind: "turn",
                status: "queued",
                fork_from: None,
                prompt: prompts::COMPACT_PROMPT,
                timeout_secs: 600,
                request: None,
            })
            .await?;
        Ok(true)
    }

    async fn after_compaction(&self, agent: &WorldAgent, main: &AgentSession, run_id: Uuid, runner_id: Uuid) -> Result<()> {
        self.store.set_session_rules_hash(main.id, "").await?;
        self.store
            .emit(
                NewEvent::new("session.compacted")
                    .agent(agent.id)
                    .session(main.id)
                    .payload(json!({ "tokens_before": main.context_tokens })),
                None,
            )
            .await?;
        if !self.run_inbox_items(run_id).await?.is_empty() {
            self.runners.finish_run_inbox(run_id, "done", None).await?;
            self.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await?;
            let _ = self.store.announce_context(agent.id).await;
            self.prepare_claimed_turn(runner_id, agent.id).await?;
        } else {
            let _ = self.store.announce_context(agent.id).await;
        }
        Ok(())
    }

    async fn after_turn(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        run_id: Uuid,
        output: &str,
    ) -> Result<()> {
        if let Some(rules_hash) = self.runners.run_rules_hash(run_id).await? {
            self.store
                .set_session_rules_hash(main.id, &rules_hash)
                .await?;
        }
        let items = self.run_inbox_items(run_id).await?;
        let tasks = self.referenced_tasks(&items).await;
        let thread = self.store.reply_thread(agent, &items).await?;
        let root = loka_shared::replies::reply_root(&items);
        let (text, actions) = prompts::split_actions(output);
        let mut problems = prompts::action_problems(output);
        let mut replied = false;
        let mut started = HashSet::new();
        for action in actions {
            match action {
                Action::Reply { text, next } if !text.trim().is_empty() => {
                    self.reply(agent, main, run_id, &text, &next, thread, root).await?;
                    replied = true;
                }
                Action::StartTask { task_id, brief, review, reviewer, time_limit_minutes, milestones, quest } => {
                    let task_id = match crate::quests::start_target(&self.store, agent, &task_id).await {
                        Ok(id) => id,
                        Err(problem) => {
                            problems.push(problem);
                            continue;
                        }
                    };
                    let choice = reviewer.as_ref().and_then(ReviewerChoice::from_value);
                    if started.insert(task_id) {
                        if let Err(error) = crate::quests::apply_choice(&self.store, task_id, &quest).await {
                            problems.push(format!("Could not set the quest options on task {task_id}: {error}"));
                            continue;
                        }
                        crate::quests::seed_milestones(&self.store, agent.id, task_id, &milestones).await;
                        match self
                            .start_task(agent, main, task_id, brief.as_deref(), review.unwrap_or(false) || choice.is_some(), choice.as_ref())
                            .await
                        {
                            Err(error) => problems.push(format!("Could not start task {task_id}: {error}")),
                            Ok(outcome) => {
                                tracing::info!(agent = %agent.slug, task = %task_id, resumed = outcome.resumed, session = %outcome.session_id, run = %outcome.run_id, "Start queued a run");
                                if let Some(minutes) = time_limit_minutes {
                                    if let Err(error) = self.store.set_task_time_limit(task_id, Some(minutes)).await {
                                        problems.push(format!("Could not set the time limit on task {task_id}: {error}"));
                                    }
                                }
                            }
                        }
                    }
                }
                Action::Status { text } => {
                    self.store
                        .set_agent_status(agent.id, "thinking", Some(&truncate(&text, 120)))
                        .await?;
                }
                Action::Escalate { text } if !text.trim().is_empty() => {
                    self.attention(
                        agent,
                        main,
                        run_id,
                        &text,
                        AttentionCard { choices: Vec::new(), drafts: Vec::new(), clarify_request: None },
                    )
                        .await?;
                    replied = true;
                }
                Action::Ask { text, choices, clarify_request, clarify_source } if !text.trim().is_empty() => {
                    let choices = choices
                        .into_iter()
                        .map(|choice| prompts::plain_dashes(choice.trim()))
                        .filter(|choice| !choice.is_empty())
                        .take(4)
                        .collect();
                    self.attention(
                        agent,
                        main,
                        run_id,
                        &text,
                        AttentionCard {
                            choices,
                            drafts: Vec::new(),
                            clarify_request: clarify_request
                                .then(|| prompts::clarification_origin(&items, &tasks, clarify_source.as_deref()))
                                .flatten(),
                        },
                    )
                    .await?;
                    replied = true;
                }
                Action::Drafts { text, items } if !text.trim().is_empty() => {
                    let drafts = prompts::clean_drafts(&items);
                    let body = if drafts.is_empty() { text } else { prompts::drafts_body(&text, &drafts) };
                    self.attention(
                        agent,
                        main,
                        run_id,
                        &body,
                        AttentionCard { choices: Vec::new(), drafts, clarify_request: None },
                    )
                    .await?;
                    replied = true;
                }
                _ => {}
            }
        }
        let had_message = items.iter().any(|item| {
            item.kind == "message"
                && item.payload.get("author_kind").and_then(Value::as_str) != Some("agent")
        });
        if had_message && !replied && !text.trim().is_empty() {
            self.reply(agent, main, run_id, &text, &[], thread, root).await?;
            replied = true;
        }
        for item in items.iter().filter(|item| {
            item.kind == "task.assigned"
                && item.payload.get("subscribed").and_then(Value::as_bool) != Some(true)
        }) {
            let Some(task_id) = item
                .payload
                .get("task_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
            else {
                continue;
            };
            let resume = item.payload.get("resume").and_then(Value::as_bool) == Some(true);
            if started.contains(&task_id)
                || (replied && !resume)
                || self.store.quest_has_parts(task_id).await.unwrap_or(false)
                || crate::quests::assigned_elsewhere(&self.store, task_id, agent.id).await
            {
                continue;
            }
            let note = item.payload.get("brief").and_then(Value::as_str).filter(|note| !note.trim().is_empty());
            let brief = note.or((!text.trim().is_empty()).then_some(text.as_str()));
            if let Err(error) = self.start_task(agent, main, task_id, brief, false, None).await {
                problems.push(format!("Could not start task {task_id}: {error}"));
            }
        }
        crate::quests::tell_agent(&self.store, agent, run_id, &items, &problems).await?;
        self.runners.finish_run_inbox(run_id, "done", None).await?;
        if let Some(current) = self
            .store
            .get_agent(agent.id)
            .await?
            .filter(|agent| agent.status != "waiting")
        {
            self.store
                .set_agent_status(agent.id, "idle", current.status_detail.as_deref())
                .await?;
        }
        Ok(())
    }

    async fn start_task(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        task_id: Uuid,
        brief: Option<&str>,
        review: bool,
        reviewer: Option<&ReviewerChoice>,
    ) -> Result<Started> {
        let task = self
            .db
            .get_task(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;
        if task.assigned_agent_id != Some(agent.id) {
            bail!("task is not assigned to this agent")
        }
        let resume = self
            .store
            .resume_task_session(agent.id, task.id, &agent.name, brief)
            .await?;
        if let Some(problem) = resume.problem() {
            bail!(problem)
        }
        if let TaskResume::Resumed { session_id, run_id } = resume {
            self.db.update_status(task.id, "in_progress").await?;
            self.store.announce_resumed(agent.id, task.id, session_id, run_id, brief).await?;
            return Ok(Started { resumed: true, session_id, run_id });
        }
        let suffix = task
            .repo
            .as_deref()
            .and_then(|repo| repo.rsplit('/').next())
            .map(safe_component)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "scratch".into());
        let workspace = format!("tasks/{}/{}", task.id, suffix);
        let options = self.store.quest_options(task.id).await?;
        let dirs: Vec<String> = (1..=options.attempts.max(1)).map(|i| crate::quests::attempt_dir(&workspace, i)).collect();
        let rules = self.work_rules(agent).await?;
        let opened = crate::quests::open_sessions(
            &self.store,
            crate::quests::QuestStart { agent, main, task: &task, brief, review, reviewer, rules: &rules, options: &options },
            &dirs,
        )
        .await?;
        let (session, run) = opened.first().ok_or_else(|| anyhow!("no task session opened"))?;
        self.db.update_status(task.id, "in_progress").await?;
        self.store
            .emit(
                NewEvent::new("task.started")
                    .agent(agent.id)
                    .session(session.id)
                    .run(run.id)
                    .task(Some(task.id))
                    .payload(crate::quests::started_payload(&task, &options, opened.len(), run.id)),
                None,
            )
            .await?;
        crate::quests::label_attempts(&self.store, &task, &opened).await?;
        self.store
            .post_quest_started(agent.id, task.id, session.id, &task.title, brief.filter(|b| !b.trim().is_empty()))
            .await?;
        Ok(Started { resumed: false, session_id: session.id, run_id: run.id })
    }

    async fn after_task(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        output: &str,
        work: Option<&str>,
    ) -> Result<()> {
        let run = self.store.get_run(run_id).await?.ok_or_else(|| anyhow!("completed task run disappeared"))?;
        if crate::quests::prework_step(&self.store, agent, session, &run, output).await? {
            return Ok(());
        }
        let parsed = prompts::parse_report(output);
        let missing = parsed.as_ref().and_then(|report| report.status.as_deref()).is_none();
        let mut report = parsed.unwrap_or_default();
        self.store
            .set_session_status(session.id, "active", Some(&serde_json::to_string(&report)?))
            .await?;
        if missing {
            if self.missing_report(session, run_id, "task", output, work).await? {
                return Ok(());
            }
            report = self.session_report(session.id).await?;
        }
        let run = self
            .store
            .get_run(run_id)
            .await?
            .ok_or_else(|| anyhow!("completed task run disappeared"))?;
        if !self
            .store
            .complete_task_run(session.id, run.created_at)
            .await?
        {
            return Ok(());
        }
        let session = self
            .store
            .get_session(session.id)
            .await?
            .unwrap_or_else(|| session.clone());
        if session.racing() {
            return self.attempt_done(agent, &session, run_id, incomplete_report(&report).is_none()).await;
        }
        let has_repo = self.quest_has_repo(&session).await?;
        match finish_step(&report, self.reviews_enabled && session.wants_review(), None) {
            Finish::Fail(_) if salvages(&report, has_repo) => self.queue_publish(agent, &session, run_id).await,
            Finish::Fail(error) => self.finalize_task(agent, &session, None, Some(error), None, None).await,
            Finish::Review => self.queue_review(agent, &session, run_id).await,
            Finish::Publish if has_repo => self.queue_publish(agent, &session, run_id).await,
            Finish::Publish => self.finalize_task(agent, &session, None, None, None, None).await,
        }
    }

    async fn quest_has_repo(&self, session: &AgentSession) -> Result<bool> {
        Ok(match session.task_id {
            Some(task_id) => self.db.get_task(task_id).await?.and_then(|task| task.repo).is_some(),
            None => false,
        })
    }

    async fn missing_report(
        &self,
        session: &AgentSession,
        run_id: Uuid,
        kind: &str,
        output: &str,
        work: Option<&str>,
    ) -> Result<bool> {
        let diagnosis = self.store.record_run_end(run_id, kind, None, output, false).await?;
        if diagnosis.cause == Cause::NeverStarted {
            let retried = self.store.auto_retry(run_id, &diagnosis).await?.is_some();
            self.store.announce_completed(run_id).await?;
            return Ok(retried);
        }
        if self.store.settle_missing(run_id, session.id, Required::Report, output).await?.handled() {
            return Ok(true);
        }
        let report = Report::synthesized(output, work);
        self.store
            .set_session_status(session.id, "active", Some(&serde_json::to_string(&report)?))
            .await?;
        Ok(false)
    }

    async fn session_report(&self, session_id: Uuid) -> Result<Report> {
        Ok(self
            .store
            .get_session(session_id)
            .await?
            .and_then(|session| prompts::stored_report(session.summary.as_deref()))
            .unwrap_or_default())
    }

    async fn attempt_done(&self, agent: &WorldAgent, session: &AgentSession, run_id: Uuid, ok: bool) -> Result<()> {
        let outcome = self.store.finish_attempt(session.id, ok).await?;
        self.attempts_outcome(agent, session, run_id, outcome, None).await
    }

    async fn attempts_outcome(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        outcome: loka_shared::quests::AttemptsOutcome,
        error: Option<&str>,
    ) -> Result<()> {
        use loka_shared::quests::AttemptsOutcome;
        let task_id = session.task_id.ok_or_else(|| anyhow!("attempt without a quest"))?;
        match outcome {
            AttemptsOutcome::Settled | AttemptsOutcome::Waiting => Ok(()),
            AttemptsOutcome::Winner(id) => {
                let winner = crate::quests::settle(&self.store, agent, task_id, id, "The only attempt that finished").await?;
                self.ship_winner(agent, &winner, None, run_id).await
            }
            AttemptsOutcome::Compare(ids) => {
                let task = self.db.get_task(task_id).await?.ok_or_else(|| anyhow!("task disappeared"))?;
                let candidates = crate::quests::candidates(&self.store, &ids).await?;
                let rules = self.work_rules(agent).await?;
                crate::quests::queue_compare(&self.store, agent, &task, task.target_branch_or_default(), &rules, &candidates).await?;
                Ok(())
            }
            AttemptsOutcome::NoneFinished => {
                let last = crate::quests::settle(&self.store, agent, task_id, session.id, "No attempt finished").await?;
                match error {
                    Some(error) => {
                        self.db.set_error(task_id, &format!("Every attempt failed. Last error: {error}")).await?;
                        Ok(())
                    }
                    None => {
                        let report = prompts::stored_report(last.summary.as_deref())
                            .unwrap_or_default();
                        let error = incomplete_report(&report).unwrap_or_else(|| "No attempt finished".into());
                        self.finalize_task(agent, &last, None, Some(error), None, None).await
                    }
                }
            }
        }
    }

    async fn after_compare(&self, agent: &WorldAgent, review: &AgentSession, ids: &[Uuid], output: Option<&str>, run_id: Uuid) -> Result<()> {
        let task_id = review.task_id.ok_or_else(|| anyhow!("compare without a quest"))?;
        let candidates = crate::quests::candidates(&self.store, ids).await?;
        if candidates.iter().all(|c| !c.racing() && c.attempt_state.as_deref() != Some("finished")) {
            return Ok(());
        }
        let (winner, verdict) = crate::quests::pick(output, &candidates);
        let why = verdict.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let winner = crate::quests::settle(&self.store, agent, task_id, winner, &why).await?;
        self.ship_winner(agent, &winner, Some(verdict), run_id).await
    }

    async fn ship_winner(&self, agent: &WorldAgent, winner: &AgentSession, verdict: Option<Value>, source_run_id: Uuid) -> Result<()> {
        let has_repo = match winner.task_id {
            Some(task_id) => self.db.get_task(task_id).await?.and_then(|task| task.repo).is_some(),
            None => false,
        };
        if has_repo {
            self.queue_publish(agent, winner, source_run_id).await
        } else {
            self.finalize_task(agent, winner, verdict, None, None, None).await
        }
    }

    async fn queue_review(
        &self,
        agent: &WorldAgent,
        task_session: &AgentSession,
        source_run_id: Uuid,
    ) -> Result<()> {
        let task_id = task_session
            .task_id
            .ok_or_else(|| anyhow!("task session has no task"))?;
        if let Some(url) = self.store.quest_merged_pr(task_id).await? {
            return self.settle_merged(agent, task_session, task_id, &url).await;
        }
        let pending = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_sessions s
               JOIN session_runs r ON r.session_id = s.id
               JOIN session_runs source ON source.id = $2
               WHERE s.parent_session_id = $1 AND s.role = 'review'
                 AND r.created_at > source.created_at)",
        )
        .bind(task_session.id)
        .bind(source_run_id)
        .fetch_one(self.store.pool())
        .await?;
        if pending {
            return Ok(());
        }
        let task = self
            .db
            .get_task(task_id)
            .await?
            .ok_or_else(|| anyhow!("task disappeared"))?;
        let (backend, model) = task_session.review_target(agent.reviewer.as_ref());
        let review = self
            .store
            .create_session(NewSession {
                agent_id: agent.id,
                role: "review",
                backend: &backend,
                model: &model,
                parent_session_id: Some(task_session.id),
                task_id: Some(task.id),
                working_dir: task_session.working_dir.as_deref(),
                usage_baseline: json!({}),
            })
            .await?;
        let rules = self.work_rules(agent).await?;
        let mut prompt = prompts::review(&task, task.target_branch_or_default());
        let author_output = match self.store.last_run_id(task_session.id).await? {
            Some(id) => self.store.get_run(id).await?.and_then(|r| r.output),
            None => None,
        };
        prompt.push_str(&prompts::executor_checks(author_output.as_deref()));
        if !rules.is_empty() {
            prompt.push_str(&format!(
                "\n\nThe author was bound by these rules. Any violation is a finding:\n{rules}"
            ));
        }
        let run = self.store
            .create_run(NewRun {
                session_id: review.id,
                agent_id: agent.id,
                task_id: Some(task.id),
                kind: "review",
                status: "queued",
                fork_from: None,
                prompt: &prompt,
                timeout_secs: task_timeout(),
                request: None,
            })
            .await?;
        self.store
            .set_run_activity(&run, &format!("Getting \"{}\" reviewed", task.title))
            .await?;
        Ok(())
    }

    async fn after_review(
        &self,
        agent: &WorldAgent,
        review: &AgentSession,
        run_id: Uuid,
        output: &str,
    ) -> Result<()> {
        if prompts::contract_open("review", review, output)
            && self.store.settle_missing(run_id, review.id, Required::Verdict, output).await?.handled()
        {
            return Ok(());
        }
        self.settle_review(agent, review, run_id, output).await
    }

    async fn settle_review(
        &self,
        agent: &WorldAgent,
        review: &AgentSession,
        run_id: Uuid,
        output: &str,
    ) -> Result<()> {
        self.store
            .set_session_status(review.id, "completed", Some(&tail(output, 2000)))
            .await?;
        let manual = review
            .asked_by
            .as_ref()
            .and_then(|value| value.get("manual"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let already_finished = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM world_events
               WHERE run_id = $1 AND type = 'review.finished')",
        )
        .bind(run_id)
        .fetch_one(self.store.pool())
        .await?;
        if manual && already_finished {
            return Ok(());
        }
        let verdict = prompts::readable_verdict(output);
        let verdict_value = review_value(output);
        if manual {
            let requester = review
                .asked_by
                .as_ref()
                .and_then(|value| value.get("agent_id"))
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                .unwrap_or(agent.id);
            self.store
                .emit(
                    NewEvent::new("review.finished")
                        .agent(agent.id)
                        .session(review.id)
                        .run(run_id)
                        .task(review.task_id)
                        .payload(json!({
                            "task_session_id": review.parent_session_id,
                            "task_id": review.task_id,
                            "review": verdict_value,
                            "reviewer": review.asked_by.as_ref().and_then(|value| value.get("reviewer")).cloned()
                        })),
                    Some((requester, "review.finished")),
                )
                .await?;
            return Ok(());
        }
        if let Some(ids) = loka_shared::quests::compare_candidates(review) {
            return self.after_compare(agent, review, &ids, Some(output), run_id).await;
        }
        let task_session_id = review
            .parent_session_id
            .ok_or_else(|| anyhow!("review has no task session"))?;
        let task_session = self
            .store
            .get_session(task_session_id)
            .await?
            .ok_or_else(|| anyhow!("task session disappeared"))?;
        let wants_changes = verdict
            .as_ref()
            .is_some_and(|verdict| verdict.verdict == "changes" && !verdict.findings.is_empty());
        if wants_changes && task_session.review_rounds < self.max_review_rounds {
            let fix_queued = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(
                   SELECT 1 FROM session_runs fix, session_runs review_run
                   WHERE review_run.id = $2 AND fix.session_id = $1 AND fix.kind = 'task'
                     AND fix.created_at > review_run.created_at AND fix.status IN ('queued', 'running', 'completed'))",
            )
            .bind(task_session.id)
            .bind(run_id)
            .fetch_one(self.store.pool())
            .await?;
            if fix_queued {
                return Ok(());
            }
            let round = self.store.bump_review_round(task_session.id).await?;
            let task_id = task_session
                .task_id
                .ok_or_else(|| anyhow!("task session has no task"))?;
            let task = self
                .db
                .get_task(task_id)
                .await?
                .ok_or_else(|| anyhow!("task disappeared"))?;
            let findings = serde_json::to_string_pretty(&verdict_value)?;
            let workspace = task_session
                .working_dir
                .clone()
                .ok_or_else(|| anyhow!("task has no quest folder"))?;
            let prompt = with_rules(
                prompts::task(&task, &workspace, Some(&findings)),
                &self.work_rules(agent).await?,
            );
            let run = self.store
                .create_run(NewRun {
                    session_id: task_session.id,
                    agent_id: agent.id,
                    task_id: Some(task.id),
                    kind: "task",
                    status: "queued",
                    fork_from: None,
                    prompt: &prompt,
                    timeout_secs: task_timeout(),
                    request: Some(loka_shared::run_request::RunRequest::fix(&findings)),
                })
                .await?;
            self.store
                .set_session_status(task_session.id, "active", None)
                .await?;
            self.store
                .set_run_activity(
                    &run,
                    &format!("Fixing review notes on \"{}\" (round {round})", task.title),
                )
                .await?;
            return Ok(());
        }
        let task = match task_session.task_id {
            Some(task_id) => self.db.get_task(task_id).await?,
            None => None,
        };
        if task.as_ref().and_then(|task| task.repo.as_ref()).is_some() {
            self.queue_publish(agent, &task_session, run_id).await
        } else {
            self.finalize_task(agent, &task_session, Some(verdict_value), None, None, None)
                .await
        }
    }

    async fn queue_publish(
        &self,
        agent: &WorldAgent,
        task_session: &AgentSession,
        source_run_id: Uuid,
    ) -> Result<()> {
        let task_id = task_session
            .task_id
            .ok_or_else(|| anyhow!("task session has no task"))?;
        let pending = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM session_runs publish
               JOIN session_runs source ON source.id = $2
               WHERE publish.session_id = $1 AND publish.kind = 'publish'
                 AND publish.created_at > source.created_at)",
        )
        .bind(task_session.id)
        .bind(source_run_id)
        .fetch_one(self.store.pool())
        .await?;
        if pending {
            return Ok(());
        }
        let run = self.store
            .create_run(NewRun {
                session_id: task_session.id,
                agent_id: agent.id,
                task_id: Some(task_id),
                kind: "publish",
                status: "queued",
                fork_from: None,
                prompt: "Publish the reviewed workspace using the structured publish instructions.",
                timeout_secs: task_timeout(),
                request: None,
            })
            .await?;
        self.store
            .set_session_status(task_session.id, "active", None)
            .await?;
        self.store
            .set_run_activity(&run, "Publishing the work")
            .await?;
        Ok(())
    }

    async fn after_publish(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        completion: &RunnerCompletion,
    ) -> Result<()> {
        let task_id = session
            .task_id
            .ok_or_else(|| anyhow!("publish session has no task"))?;
        if completion.pr_url.is_some() != completion.pr_number.is_some() {
            bail!("publish completion must include both pr_url and pr_number")
        }
        let repo = self.db.get_task(task_id).await?.and_then(|task| task.repo);
        let mut pr = completion.pr_url.clone().zip(completion.pr_number);
        if let Some((url, number)) = &pr {
            if !repo.as_deref().is_some_and(|repo| loka_shared::runners::pr_url_matches(url, repo, *number)) {
                bail!("publish completion reported a pull request outside this quest's repository")
            }
        }
        if pr.is_none() && may_attach_reported_pr(completion) {
            pr = prompts::stored_report(session.summary.as_deref())
                .zip(repo)
                .and_then(|(report, repo)| report.reported_pr(&repo))
                .map(|(url, number)| (url, i64::from(number)));
        }
        if let Some((url, number)) = &pr {
            let number =
                i32::try_from(*number).map_err(|_| anyhow!("pull request number is too large"))?;
            self.db.set_pr(task_id, url, number).await?;
        }
        let (pr_url, pr_number) = pr.unzip();
        match completion.publish_pr_state.as_deref() {
            Some("merged") => {
                self.settle_merged(agent, session, task_id, pr_url.as_deref().unwrap_or_default())
                    .await
            }
            Some("closed") => {
                let store = MilestoneStore::new(self.store.pool().clone());
                store.settle_merge(task_id, false).await?;
                self.finalize_task(
                    agent,
                    session,
                    None,
                    Some("The pull request was closed without merging".into()),
                    pr_url,
                    pr_number,
                )
                .await
            }
            _ => {
                let verdict = self
                    .store
                    .latest_review_output(session.id)
                    .await?
                    .map(|output| review_value(&output));
                let report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
                let error = finish_error(&report, verdict.as_ref());
                self.finalize_task(agent, session, verdict, error, pr_url, pr_number)
                    .await
            }
        }
    }

    async fn review_failed(
        &self,
        agent: &WorldAgent,
        task_session: &AgentSession,
        run_id: Uuid,
        error: &str,
        stopped: bool,
    ) -> Result<()> {
        if let Some(task_id) = task_session.task_id {
            if let Some(url) = self.store.quest_merged_pr(task_id).await? {
                return self.settle_merged(agent, task_session, task_id, &url).await;
            }
            MilestoneStore::new(self.store.pool().clone()).check_now(task_id).await?;
            let published = self.db.get_task(task_id).await?.is_some_and(|task| task.repo.is_some() && task.pr_url.is_some());
            if stopped && published {
                return self.queue_publish(agent, task_session, run_id).await;
            }
        }
        self.finalize_task(
            agent,
            task_session,
            Some(json!({ "verdict": "review failed", "summary": error })),
            Some(error.to_string()),
            None,
            None,
        )
        .await
    }

    async fn settle_merged(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        task_id: Uuid,
        pr_url: &str,
    ) -> Result<()> {
        let store = MilestoneStore::new(self.store.pool().clone());
        store.settle_merge(task_id, true).await?;
        if store.open_count(task_id).await? == 0 {
            self.db.update_status(task_id, "done").await?;
        }
        self.store
            .set_session_status(session.id, "completed", None)
            .await?;
        let announced = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM world_events WHERE task_id = $1 AND type = 'task.merged')",
        )
        .bind(task_id)
        .fetch_one(self.store.pool())
        .await?;
        if announced {
            return Ok(());
        }
        let title = self
            .db
            .get_task(task_id)
            .await?
            .map(|task| task.title)
            .unwrap_or_else(|| "A quest".into());
        let main = self.store.main_session(agent.id).await?;
        self.store
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: "The pull request was merged.",
                session_id: main.map(|main| main.id),
                run_id: None,
                meta: json!({
                    "kind": "quest",
                    "title": title,
                    "status": "done",
                    "pr_url": pr_url,
                    "task_id": task_id,
                    "session_id": session.id,
                    "merged": true
                }),
            })
            .await?;
        self.store
            .emit(
                NewEvent::new("task.merged")
                    .agent(agent.id)
                    .session(session.id)
                    .task(Some(task_id))
                    .payload(json!({ "task_id": task_id, "title": title, "pr_url": pr_url, "status": "done" })),
                Some((agent.id, "task.merged")),
            )
            .await?;
        Ok(())
    }

    async fn after_followup(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        output: &str,
        work: Option<&str>,
        pr_sync: loka_shared::runners::PrSync,
    ) -> Result<()> {
        let run = self.store.get_run(run_id).await?.ok_or_else(|| anyhow!("completed follow-up disappeared"))?;
        if crate::quests::prework_step(&self.store, agent, session, &run, output).await? {
            return Ok(());
        }
        if let Some(report) = prompts::parse_report(output) {
            self.store
                .set_session_status(
                    session.id,
                    &session.status,
                    Some(&serde_json::to_string(&report)?),
                )
                .await?;
        }
        self.emit_run_event_once(
            "session.replied",
            run_id,
            session,
            agent.id,
            json!({
                "session_id": session.id,
                "task_id": session.task_id,
                "reply": prompts::plain_dashes(&prompts::clip_reply(output))
            }),
            true,
        )
        .await?;
        if session.role == "task" {
            let FollowupAdvance::Advance { was_completed } = self.store.complete_followup(session.id).await? else {
                return Ok(());
            };
            let session = self
                .store
                .get_session(session.id)
                .await?
                .unwrap_or_else(|| session.clone());
            let mut report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
            let nudge_available = if !was_completed && report.status.is_none() {
                self.store
                    .set_session_status(session.id, "active", None)
                    .await?;
                let handled = self.missing_report(&session, run_id, "followup", output, work).await?;
                report = self.session_report(session.id).await?;
                handled
            } else {
                false
            };
            let has_repo = self.quest_has_repo(&session).await?;
            let verdict = crate::quests::review_on_record(&self.store, session.id, run_id, pr_sync)
                .await?
                .map(|output| review_value(&output));
            let step = followup_step(
                was_completed,
                &report,
                self.reviews_enabled,
                session.wants_review(),
                nudge_available,
                has_repo,
                pr_sync,
                verdict.as_ref(),
            );
            if session.racing() && !matches!(step, FollowupStep::Nudge | FollowupStep::Leave) {
                return self.attempt_done(agent, &session, run_id, !matches!(step, FollowupStep::Fail(_) | FollowupStep::Salvage)).await;
            }
            match step {
                FollowupStep::Nudge | FollowupStep::Leave => {}
                FollowupStep::Salvage => {
                    self.queue_publish(agent, &session, run_id).await?;
                }
                FollowupStep::Fail(error) => {
                    self.finalize_task(agent, &session, None, Some(error), None, None)
                        .await?;
                }
                FollowupStep::Review => {
                    self.queue_review(agent, &session, run_id).await?;
                }
                FollowupStep::Settle => {
                    if pr_sync == loka_shared::runners::PrSync::Merged {
                        if let Some(task_id) = session.task_id {
                            if let Some(url) = self.db.get_task(task_id).await?.and_then(|task| task.pr_url) {
                                return self.settle_merged(agent, &session, task_id, &url).await;
                            }
                        }
                    }
                    if has_repo {
                        self.queue_publish(agent, &session, run_id).await?;
                    } else {
                        self.finalize_task(agent, &session, verdict, None, None, None)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn after_quick(&self, agent: &WorldAgent, session: &AgentSession, run_id: Uuid, output: &str) -> Result<()> {
        self.store.record_quick_reply(agent, session, run_id, &quick_answer(output), false).await?;
        Ok(())
    }

    pub async fn deliver_lost_quick_replies(&self, session: Option<Uuid>) -> Result<usize> {
        let mut delivered = 0;
        for run in self.store.quick_runs_without_reply(session, LOST_REPLY_GRACE_SECS).await? {
            let (Some(agent), Some(session)) = (self.store.get_agent(run.agent_id).await?, self.store.get_session(run.session_id).await?) else {
                continue;
            };
            let ended_well = run.status == "completed";
            let text = if ended_well {
                quick_answer(run.output.as_deref().unwrap_or_default())
            } else {
                let reason = run.error.as_deref().filter(|e| !e.trim().is_empty()).unwrap_or("the run ended before it could reply");
                format!("I could not answer that: {reason}")
            };
            if self.store.record_quick_reply(&agent, &session, run.id, &text, !ended_well).await?.is_some() {
                tracing::warn!(run = %run.id, status = %run.status, "delivered a quick chat reply that never became a message");
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    async fn quick_rules(&self, agent: &WorldAgent) -> Result<String> {
        let rules: Vec<_> = self.store.rules_for(agent.owner_user_id, agent.org_id).await?.into_iter().filter(|r| r.scope == "talk").collect();
        let talk = prompts::rules(&rules, "talk", &self.owner_name);
        Ok([talk, self.work_rules(agent).await?].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n"))
    }

    async fn after_aside(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        output: &str,
    ) -> Result<()> {
        let text = prompts::plain_dashes(&prompts::split_actions(output).0);
        self.record_aside(agent, session, text, "completed").await
    }

    async fn after_aside_failure(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        error: &str,
    ) -> Result<()> {
        self.record_aside(
            agent,
            session,
            format!("(could not answer: {error})"),
            "failed",
        )
        .await
    }

    async fn record_aside(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        text: String,
        status: &str,
    ) -> Result<()> {
        let answered = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM world_events
               WHERE session_id = $1 AND type = 'aside.answered')",
        )
        .bind(session.id)
        .fetch_one(self.store.pool())
        .await?;
        if answered {
            return Ok(());
        }
        self.store
            .set_session_status(session.id, status, Some(&tail(&text, 2000)))
            .await?;
        if self.store.deliver_loka_answer(agent, session, &text, status == "completed").await? {
            return Ok(());
        }
        let asked_by = session.asked_by.clone().unwrap_or(Value::Null);
        let thread = asked_by_user(&asked_by);
        let message_id = match sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_messages
             WHERE agent_id = $1 AND session_id = $2 AND author_kind = 'agent'
             ORDER BY created_at LIMIT 1",
        )
        .bind(agent.id)
        .bind(session.id)
        .fetch_optional(self.store.pool())
        .await?
        {
            Some(id) => id,
            None => {
                self.store
                    .insert_message(NewMessage {
                        agent_id: agent.id,
                        author_kind: "agent",
                        author_user_id: None,
                        author_agent_id: Some(agent.id),
                        body: &text,
                        session_id: Some(session.id),
                        run_id: None,
                        meta: in_thread(Value::Null, thread),
                    })
                    .await?
                    .id
            }
        };
        let asker = asked_by
            .get("agent_id")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Uuid>().ok());
        self.store
            .emit(
                NewEvent::new("aside.answered")
                    .agent(agent.id)
                    .session(session.id)
                    .payload(in_thread(
                        json!({
                            "aside_session_id": session.id,
                            "agent": agent.name,
                            "agent_slug": agent.slug,
                            "question": asked_by.get("question"),
                            "answer": text,
                            "asked_by": asked_by,
                            "message_id": message_id
                        }),
                        thread,
                    )),
                asker.map(|agent_id| (agent_id, "aside.answered")),
            )
            .await?;
        self.store.deliver_guest_answer(agent, session, &text, status == "completed").await?;
        Ok(())
    }

    async fn finalize_task(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        verdict: Option<Value>,
        error: Option<String>,
        pr_url: Option<String>,
        _pr_number: Option<i64>,
    ) -> Result<()> {
        let finished = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM world_events
               WHERE session_id = $1 AND type = 'task.finished'
                 AND created_at > COALESCE(
                   (SELECT MAX(created_at) FROM session_runs WHERE session_id = $1 AND kind IN ('task', 'followup')),
                   '-infinity'))",
        )
        .bind(session.id)
        .fetch_one(self.store.pool())
        .await?;
        if finished {
            return Ok(());
        }
        let parsed: Report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
        let mut watching = false;
        let report: Value = session
            .summary
            .as_deref()
            .and_then(|summary| serde_json::from_str(summary).ok())
            .unwrap_or_else(|| json!({ "summary": session.summary }));
        if self
            .store
            .finished_report_unchanged(session.id, &report, pr_url.as_deref(), error.as_deref())
            .await?
        {
            self.store
                .set_session_status(
                    session.id,
                    if error.is_some() { "failed" } else { "completed" },
                    None,
                )
                .await?;
            return Ok(());
        }
        let diagnosis = self.store.latest_diagnosis(session.id).await?;
        let error = match (&error, &diagnosis) {
            (Some(_), Some(diagnosis)) if parsed.status.is_none() => Some(diagnosis.reason.clone()),
            _ => error,
        };
        let diagnosis = diagnosis.filter(|_| error.is_some());
        let mut task_status = "failed";
        let mut changes_requested = false;
        let quest = match session.task_id {
            Some(task_id) => self.db.get_task(task_id).await?,
            None => None,
        };
        if session.task_id.is_some() {
            if let Some(task) = quest.clone() {
                if let Some(summary) = report.get("summary").and_then(Value::as_str) {
                    self.db.set_result_content(task.id, summary).await?;
                }
                let milestones = MilestoneStore::new(self.store.pool().clone());
                milestones
                    .complete_work(task.id, &parsed.milestone_positions(), &parsed.milestone_titles(), error.is_none())
                    .await?;
                if error.is_none() {
                    watching = milestones.arm_merge(task.id, pr_url.is_some()).await?;
                }
                let review_wants_changes = verdict
                    .as_ref()
                    .and_then(|value| value.get("verdict"))
                    .and_then(Value::as_str)
                    == Some("changes");
                changes_requested = review_wants_changes;
                let already_merged = error.is_none() && milestones.merge_done(task.id).await?;
                task_status = finished_status(error.is_some(), already_merged, pr_url.is_some(), review_wants_changes);
                if let Some(error) = &error {
                    self.db.set_error(task.id, error).await?;
                } else {
                    self.db.update_status(task.id, task_status).await?;
                }
            }
        }
        self.store
            .set_session_status(
                session.id,
                if error.is_some() {
                    "failed"
                } else {
                    "completed"
                },
                None,
            )
            .await?;
        let verdict_label = verdict
            .as_ref()
            .and_then(|value| value.get("verdict"))
            .and_then(Value::as_str)
            .map(String::from);
        let wrap = crate::quests::wrapup(&self.store, agent, session, quest.as_ref(), &parsed, error.as_deref(), changes_requested, None).await;
        if let Some(task_id) = session.task_id {
            let title = self
                .db
                .get_task(task_id)
                .await?
                .map(|task| task.title)
                .unwrap_or_else(|| "A quest".into());
            let summary = report.get("summary").and_then(Value::as_str).unwrap_or("");
            let main = self.store.main_session(agent.id).await?;
            let has_card = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_messages
                   WHERE agent_id = $1 AND meta->>'kind' = 'quest'
                     AND meta->>'session_id' = $2
                     AND COALESCE(meta->>'status', '') <> 'started'
                     AND created_at > COALESCE(
                       (SELECT MAX(created_at) FROM session_runs WHERE session_id = $3 AND kind IN ('task', 'followup')),
                       '-infinity'))",
            )
            .bind(agent.id)
            .bind(session.id.to_string())
            .bind(session.id)
            .fetch_one(self.store.pool())
            .await?;
            if !has_card {
                self.store
                    .insert_message(NewMessage {
                        agent_id: agent.id,
                        author_kind: "system",
                        author_user_id: None,
                        author_agent_id: Some(agent.id),
                        body: &prompts::plain_dashes(summary),
                        session_id: main.map(|session| session.id),
                        run_id: None,
                        meta: json!({
                            "kind": "quest",
                            "title": title,
                            "status": task_status,
                            "pr_url": pr_url,
                            "verdict": verdict_label,
                            "error": error,
                            "task_id": task_id,
                            "session_id": session.id,
                            "watching": watching,
                            "pace": wrap.pace,
                            "timing": wrap.timing,
                            "cause": diagnosis.as_ref().map(|d| d.cause.as_str()),
                            "reason": diagnosis.as_ref().map(|d| d.reason.as_str()),
                            "synthesized": parsed.synthesized
                        }),
                    })
                    .await?;
            }
        }
        let mut payload = json!({
            "task_id": session.task_id,
            "session_id": session.id,
            "report": report,
            "verdict": verdict_label,
            "review": verdict,
            "pr_url": pr_url,
            "status": task_status,
            "error": error,
            "pace": wrap.pace,
            "timing": wrap.timing
        });
        if let Some(diagnosis) = &diagnosis {
            diagnosis.merge_into(&mut payload);
        }
        self.store
            .emit(
                NewEvent::new("task.finished")
                    .agent(agent.id)
                    .session(session.id)
                    .task(session.task_id)
                    .payload(payload),
                wrap.wake.then_some((agent.id, "task.finished")),
            )
            .await?;
        Ok(())
    }

    async fn fail_run(
        &self,
        run_id: Uuid,
        kind: &str,
        session: &AgentSession,
        agent: &WorldAgent,
        error: &str,
        timed_out: bool,
    ) -> Result<()> {
        if self.is_compaction(run_id, kind).await? {
            tracing::warn!(agent = %agent.slug, %error, "Main session compaction failed");
            if !self.run_inbox_items(run_id).await?.is_empty() {
                self.system_note(
                    agent,
                    &format!("Compacting the context did not work: {}. Try again, or reset the context instead.", loka_shared::cli_errors::error_text(error)),
                )
                .await?;
                self.runners.finish_run_inbox(run_id, "done", None).await?;
                self.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await?;
            }
            let _ = self.store.announce_context(agent.id).await;
            return Ok(());
        }
        if self.is_reset(run_id, kind).await? {
            if loka_shared::cli_errors::is_context_too_long(error) {
                self.system_note(agent, loka_shared::context::RESET_KEPT_NOTES).await?;
                return self.reset_with_saved_notes(agent, session, run_id).await;
            }
            self.system_note(
                agent,
                &format!("Context reset aborted: {}. Nothing was forgotten; ask again when ready.", loka_shared::cli_errors::error_text(error)),
            )
            .await?;
            self.runners.finish_run_inbox(run_id, "done", None).await?;
            self.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await?;
            let _ = self.store.announce_context(agent.id).await;
            return Ok(());
        }
        let diagnosis = if matches!(kind, "task" | "followup" | "review" | "publish") {
            let diagnosis = self.store.record_run_end(run_id, kind, Some(error), "", timed_out).await?;
            if self.store.auto_retry(run_id, &diagnosis).await?.is_some() {
                return Ok(());
            }
            Some(diagnosis)
        } else {
            None
        };
        if kind == "turn" {
            self.stop_typing(agent, run_id).await;
        }
        if loka_shared::run_causes::update_pause(error) {
            let resumed = match kind {
                "turn" => {
                    self.runners.requeue_run_inbox(run_id, error).await?;
                    true
                }
                "direct" | "aside" => self.store.resume_after_update(run_id).await?,
                _ => false,
            };
            if resumed {
                self.store.set_agent_status(agent.id, "idle", None).await?;
                return Ok(());
            }
        }
        let error = &loka_shared::cli_errors::describe(error);
        let payload = || {
            let mut payload = loka_shared::cli_errors::failure_payload(session.task_id, error, session.id, kind);
            if let Some(diagnosis) = &diagnosis {
                diagnosis.merge_into(&mut payload);
            }
            payload
        };
        self.store
            .fail_queued_followups(session.id, &format!("Parent run failed: {error}"))
            .await?;
        if kind == "turn" {
            self.runners.requeue_run_inbox(run_id, error).await?;
            self.emit_run_event_once(
                if timed_out {
                    "run.timed_out"
                } else {
                    "run.failed"
                },
                run_id,
                session,
                agent.id,
                loka_shared::cli_errors::failure_payload(session.task_id, error, session.id, kind),
                false,
            )
            .await?;
            self.store
                .set_agent_status(agent.id, "error", Some(&loka_shared::cli_errors::status_detail(error)))
                .await?;
            self.store.pause_for_rejected_key(agent, error).await?;
            return Ok(());
        }
        if kind == "aside" {
            self.after_aside_failure(agent, session, error).await?;
            return Ok(());
        }
        if kind == "quick" {
            self.store
                .record_quick_reply(agent, session, run_id, &format!("I could not answer that: {error}"), true)
                .await?;
            return Ok(());
        }
        if kind == "review"
            && loka_shared::quests::compare_candidates(session).is_none()
            && self
                .store
                .get_run(run_id)
                .await?
                .is_some_and(|run| prompts::failed_nudge_settles(&run.prompt, error))
        {
            let output = prompts::failed_nudge_output(error);
            return self.settle_review(agent, session, run_id, &output).await;
        }
        if kind == "review" {
            self.store
                .set_session_status(session.id, "failed", Some(error))
                .await?;
            if let Some(ids) = loka_shared::quests::compare_candidates(session) {
                return self.after_compare(agent, session, &ids, None, run_id).await;
            }
            let manual = session
                .asked_by
                .as_ref()
                .and_then(|asked_by| asked_by.get("manual"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !manual {
                if let Some(parent) = session.parent_session_id {
                    if let Some(task_session) = self.store.get_session(parent).await? {
                        let stopped = diagnosis.as_ref().is_some_and(|d| d.cause == loka_shared::run_causes::Cause::Stopped);
                        return self.review_failed(agent, &task_session, run_id, error, stopped).await;
                    }
                }
            }
        }
        let task_failed = kind == "task"
            || kind == "publish"
            || kind == "followup" && session.role == "task" && session.status == "active";
        if task_failed && kind != "publish" && session.racing() {
            self.store.set_session_status(session.id, "failed", Some(error)).await?;
            let outcome = self.store.finish_attempt(session.id, false).await?;
            let last = outcome == loka_shared::quests::AttemptsOutcome::NoneFinished;
            self.emit_run_event_once(
                if timed_out { "run.timed_out" } else { "run.failed" },
                run_id,
                session,
                agent.id,
                payload(),
                last,
            )
            .await?;
            return self.attempts_outcome(agent, session, run_id, outcome, Some(error)).await;
        }
        if task_failed {
            self.store
                .set_session_status(session.id, "failed", Some(error))
                .await?;
            if let Some(task_id) = session.task_id {
                self.db.set_error(task_id, error).await?;
            }
        }
        self.emit_run_event_once(
            if timed_out {
                "run.timed_out"
            } else {
                "run.failed"
            },
            run_id,
            session,
            agent.id,
            payload(),
            true,
        )
        .await?;
        self.store
            .set_agent_status(agent.id, "error", Some(&loka_shared::cli_errors::status_detail(error)))
            .await?;
        Ok(())
    }

    async fn emit_run_event_once(
        &self,
        event_type: &str,
        run_id: Uuid,
        session: &AgentSession,
        agent_id: Uuid,
        payload: Value,
        wake: bool,
    ) -> Result<()> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM world_events WHERE run_id = $1 AND type = $2)",
        )
        .bind(run_id)
        .bind(event_type)
        .fetch_one(self.store.pool())
        .await?;
        if !exists {
            self.store
                .emit(
                    NewEvent::new(event_type)
                        .agent(agent_id)
                        .session(session.id)
                        .run(run_id)
                        .task(session.task_id)
                        .payload(payload),
                    wake.then_some((agent_id, event_type)),
                )
                .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn reply(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        run_id: Uuid,
        text: &str,
        next: &[String],
        thread: Option<Uuid>,
        root: Option<Uuid>,
    ) -> Result<()> {
        let text = self.store.expand_message_links(&prompts::plain_dashes(text), agent.id).await;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND run_id = $2 AND author_kind = 'agent' AND body = $3)",
        )
        .bind(agent.id)
        .bind(run_id)
        .bind(&text)
        .fetch_one(self.store.pool())
        .await?;
        if exists {
            return Ok(());
        }
        let message = self
            .store
            .insert_reply(NewMessage {
                agent_id: agent.id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: &text,
                session_id: Some(main.id),
                run_id: Some(run_id),
                meta: prompts::with_next(in_thread(Value::Null, thread), next),
            }, root)
            .await?;
        self.store
            .emit(
                NewEvent::new("message.created")
                    .agent(agent.id)
                    .session(main.id)
                    .run(run_id)
                    .payload(in_thread(
                        json!({
                            "message_id": message.id,
                            "author": agent.name,
                            "author_kind": "agent",
                            "body": text,
                            "reply_to": message.reply_to
                        }),
                        thread,
                    )),
                None,
            )
            .await?;
        Ok(())
    }

    async fn attention(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        run_id: Uuid,
        text: &str,
        card: AttentionCard,
    ) -> Result<()> {
        let AttentionCard { choices, drafts, clarify_request } = card;
        let clarification = clarify_request.is_some();
        let text = self.store.expand_message_links(&prompts::plain_dashes(text), agent.id).await;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND run_id = $2 AND author_kind = 'agent'
                 AND body = $3 AND meta->>'kind' = 'ask')",
        )
        .bind(agent.id)
        .bind(run_id)
        .bind(&text)
        .fetch_one(self.store.pool())
        .await?;
        if exists {
            return Ok(());
        }
        let message = self
            .store
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: &text,
                session_id: Some(main.id),
                run_id: Some(run_id),
                meta: if drafts.is_empty() {
                    json!({ "kind": "ask", "choices": choices, "clarify_request": clarify_request })
                } else {
                    json!({ "kind": "ask", "choices": [], "drafts": drafts })
                },
            })
            .await?;
        self.store
            .emit(
                NewEvent::new("message.created")
                    .agent(agent.id)
                    .session(main.id)
                    .run(run_id)
                    .payload(json!({
                        "message_id": message.id,
                        "author": agent.name,
                        "author_kind": "agent",
                        "body": text,
                        "ask": true
                    })),
                None,
            )
            .await?;
        self.store
            .emit(
                NewEvent::new("attention.requested")
                    .agent(agent.id)
                    .session(main.id)
                    .run(run_id)
                    .payload(json!({
                        "text": text,
                        "agent": agent.name,
                        "message_id": message.id,
                        "choices": choices,
                        "clarification": clarification
                    })),
                None,
            )
            .await?;
        self.store
            .set_agent_status(agent.id, "waiting", Some(&truncate(&text, 120)))
            .await?;
        Ok(())
    }

    async fn system_note(&self, agent: &WorldAgent, text: &str) -> Result<()> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND author_kind = 'system' AND body = $2)",
        )
        .bind(agent.id)
        .bind(text)
        .fetch_one(self.store.pool())
        .await?;
        if exists {
            return Ok(());
        }
        self.store
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: None,
                body: text,
                session_id: None,
                run_id: None,
                meta: Value::Null,
            })
            .await?;
        Ok(())
    }

    async fn work_rules(&self, agent: &WorldAgent) -> Result<String> {
        let rules = prompts::rules(
            &self.store.rules_for(agent.owner_user_id, agent.org_id).await?,
            "work",
            &self.owner_name,
        );
        let teams = prompts::teams(&self.store.teams_of_agent(agent.id).await?, agent.id, "work");
        Ok([rules, teams].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n"))
    }

    async fn stop_typing(&self, agent: &WorldAgent, run_id: Uuid) {
        let items = self.run_inbox_items(run_id).await.unwrap_or_default();
        let typing = self.store.turn_typing_places(agent, &items).await;
        let _ = self.store.set_typing(agent, &typing, false, Some(run_id), None).await;
    }

    async fn run_inbox_items(&self, run_id: Uuid) -> Result<Vec<InboxItem>> {
        Ok(sqlx::query_as::<_, InboxItem>(
            "SELECT i.id, i.agent_id, i.kind, i.event_seq, i.payload, i.status, i.attempts, i.created_at
             FROM agent_inbox i JOIN runner_run_inbox x ON x.inbox_id = i.id
             WHERE x.run_id = $1 ORDER BY i.id",
        )
        .bind(run_id)
        .fetch_all(self.store.pool())
        .await?)
    }

    async fn referenced_tasks(&self, items: &[InboxItem]) -> Vec<AgentTask> {
        let mut tasks = Vec::new();
        for item in items {
            let Some(id) = item
                .payload
                .get("task_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
            else {
                continue;
            };
            if tasks.iter().any(|task: &AgentTask| task.id == id) {
                continue;
            }
            if let Ok(Some(task)) = self.db.get_task(id).await {
                tasks.push(task);
            }
        }
        tasks
    }

    async fn run_uploads(&self, run_id: Uuid) -> Result<(Vec<RunnerImage>, Vec<RunnerFile>, Option<String>)> {
        let payloads = sqlx::query_scalar::<_, Value>(
            "SELECT i.payload FROM agent_inbox i
             JOIN runner_run_inbox x ON x.inbox_id = i.id WHERE x.run_id = $1 ORDER BY i.id",
        )
        .bind(run_id)
        .fetch_all(self.store.pool())
        .await?;
        let attached = prompts::attached_ids(&payloads);
        let file_ids: Vec<Uuid> = attached.files.iter().map(|(id, _)| *id).collect();
        let infos = self.store.upload_infos(&file_ids).await?;
        let mut total = 0usize;
        let mut wanted = Vec::new();
        let mut skipped = Vec::new();
        for (id, name) in attached.files {
            let Some((_, mime, size)) = infos.iter().find(|(known, _, _)| *known == id) else {
                continue;
            };
            let file_name = loka_shared::media::attachment_file_name(id, &name, mime);
            if loka_shared::media::fits_caps(&mut total, usize::try_from(*size).unwrap_or(usize::MAX)) {
                wanted.push((id, file_name, mime.clone()));
            } else {
                skipped.push(file_name);
            }
        }
        let (image_uploads, file_uploads) = tokio::try_join!(
            futures_util::future::try_join_all(attached.images.iter().map(|id| self.store.get_upload(*id, None))),
            futures_util::future::try_join_all(wanted.iter().map(|(id, _, _)| self.store.get_upload(*id, None))),
        )?;
        let images = attached
            .images
            .into_iter()
            .zip(image_uploads)
            .filter_map(|(id, upload)| {
                let (mime_type, bytes) = upload.filter(|(mime, _)| prompts::is_image(mime))?;
                Some(RunnerImage {
                    id,
                    name: format!("{id}.{}", image_extension(&mime_type)),
                    mime_type,
                    data_base64: STANDARD.encode(bytes),
                })
            })
            .collect();
        let files = wanted
            .into_iter()
            .zip(file_uploads)
            .filter_map(|((id, name, _), upload)| {
                let (mime_type, bytes) = upload?;
                Some(RunnerFile { id, name, mime_type, data_base64: STANDARD.encode(bytes) })
            })
            .collect();
        Ok((images, files, loka_shared::media::attached_files_note(&[], &skipped)))
    }

    async fn turn_options(&self, run_id: Uuid, backend: &str) -> Result<TurnOptions> {
        let payloads = sqlx::query_scalar::<_, Value>(
            "SELECT i.payload FROM agent_inbox i
             JOIN runner_run_inbox x ON x.inbox_id = i.id WHERE x.run_id = $1 ORDER BY i.id DESC",
        )
        .bind(run_id)
        .fetch_all(self.store.pool())
        .await?;
        let mut options = payloads
            .iter()
            .find_map(|payload| payload.get("options").filter(|value| value.is_object()))
            .map(TurnOptions::from_value)
            .unwrap_or_default();
        if backend == "claude" && !options.fast {
            let items = self.run_inbox_items(run_id).await?;
            options.fast = self.store.any_urgent(&loka_shared::quests::inbox_task_ids(&items)).await?;
        }
        Ok(options)
    }

    async fn mcp_servers(
        &self,
        agent_id: Uuid,
        keyless: bool,
        environment: &BTreeMap<String, String>,
        vault_refs: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let mut servers = Map::new();
        for entry in self.store.mcp_servers_for(agent_id).await? {
            let definition = loka_shared::environment::runnable_definition(entry.definition);
            let missing = match loka_shared::environment::secret_reference_names(&definition) {
                Ok(references) if keyless && !references.is_empty() => continue,
                Ok(_) => loka_shared::environment::missing_secrets(&definition, environment).unwrap_or_default(),
                Err(error) => {
                    tracing::warn!(%agent_id, server = %entry.name, %error, "Skipping an MCP server with an unreadable definition");
                    continue;
                }
            };
            let vault_keys: Vec<&str> = missing.iter().map(String::as_str).filter(|name| vault_refs.contains_key(*name)).collect();
            if !vault_keys.is_empty() {
                tracing::warn!(%agent_id, server = %entry.name, keys = %vault_keys.join(", "), "Skipping an MCP server whose keys are vault references: MCP servers cannot read vault references, so store the value itself for these keys");
                continue;
            }
            if !missing.is_empty() {
                tracing::warn!(%agent_id, server = %entry.name, keys = %missing.join(", "), "Skipping an MCP server whose keys are not granted");
                continue;
            }
            match resolve_definition(definition, environment) {
                Ok(definition) => {
                    servers.insert(entry.name, definition);
                }
                Err(error) => tracing::warn!(%agent_id, server = %entry.name, %error, "Skipping an MCP server that could not be prepared"),
            }
        }
        Ok(Value::Object(servers))
    }

    async fn publish_payload(
        &self,
        task: &AgentTask,
        session: &AgentSession,
    ) -> Result<RunnerPublish> {
        let report = prompts::stored_report(session.summary.as_deref())
            .unwrap_or_default();
        let repo = task
            .repo
            .clone()
            .ok_or_else(|| anyhow!("publish task has no repository"))?;
        let branch = task.agent_branch_name();
        let base_branch = task.target_branch_or_default().to_string();
        let commit_message = report
            .commit_message
            .unwrap_or_else(|| format!("feat: {}", task.title));
        let pr_title = report.pr_title.unwrap_or_else(|| task.title.clone());
        let pr_body = report.pr_body.or(report.summary).unwrap_or_default();
        Ok(RunnerPublish {
            repo,
            branch,
            base_branch,
            commit_message,
            pr_title,
            pr_body,
            pr_url: task.pr_url.clone(),
            pr_number: task.pr_number.map(i64::from),
            stack_onto_pr: parent_quest_pr(&self.db, task).await,
        })
    }

    pub async fn record_live_usage(&self, lease: &LeaseContext, events: &[RunnerEvent]) -> Result<loka_shared::withdraw::StoppedWork> {
        let Some(usage) = events
            .iter()
            .rev()
            .filter(|event| event.kind == loka_shared::runners::USAGE_EVENT)
            .find_map(|event| serde_json::from_str::<RunnerUsage>(&event.text).ok())
            .filter(RunnerUsage::plausible)
        else {
            return Ok(Default::default());
        };
        let Some(session) = self.store.get_session(lease.session_id).await? else {
            return Ok(Default::default());
        };
        let spent = loka_shared::run_recording::account(
            &session.backend,
            &session.model,
            &session.usage_baseline,
            &loka_shared::run_recording::Reported { cost_usd: 0.0, usage },
        );
        let stopped = self
            .store
            .record_live_usage(lease.run_id, spent.cost_usd, spent.input_tokens, spent.cached_input_tokens, spent.output_tokens)
            .await?;
        if !stopped.on_runners.is_empty() {
            self.handle_expired(&stopped.on_runners).await?;
        }
        Ok(stopped)
    }
}

fn reported(completion: &RunnerCompletion) -> loka_shared::run_recording::Reported {
    loka_shared::run_recording::Reported { cost_usd: completion.cost_usd, usage: completion.usage }
}

#[derive(Default)]
struct TurnOptions {
    model: Option<String>,
    effort: Option<String>,
    fast: bool,
}

impl TurnOptions {
    fn from_value(value: &Value) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty() && text.len() <= 80 && !text.starts_with('-'))
                .map(String::from)
        };
        Self {
            model: text("model"),
            effort: text("effort").filter(|effort| {
                matches!(
                    effort.as_str(),
                    "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                )
            }),
            fast: value.get("fast").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

fn workspace_for(
    session: &AgentSession,
    task: Option<&AgentTask>,
    kind: &str,
    fork: bool,
) -> Result<RunnerWorkspace> {
    let key = session
        .working_dir
        .clone()
        .ok_or_else(|| anyhow!("session has no workspace key"))?;
    if key.starts_with('/') || key.split('/').any(|part| part == ".." || part.is_empty()) {
        bail!("invalid runner workspace key")
    }
    Ok(RunnerWorkspace {
        key,
        repo: task.and_then(|task| task.repo.clone()),
        branch: task.map(AgentTask::agent_branch_name),
        base_branch: task.map(|task| task.target_branch_or_default().to_string()),
        create: session.role == "main" && session.cli_session_id.is_none()
            || session.role == "task" && kind == "task" && (fork || session.attempt.is_some() && session.cli_session_id.is_none()),
    })
}

fn resolve_definition(
    mut definition: Value,
    environment: &BTreeMap<String, String>,
) -> Result<Value> {
    let object = definition
        .as_object_mut()
        .ok_or_else(|| anyhow!("MCP definition must be an object"))?;
    for (field, value) in object {
        if matches!(field.as_str(), "env" | "headers") {
            let values = value
                .as_object_mut()
                .ok_or_else(|| anyhow!("MCP {field} must be an object"))?;
            for value in values.values_mut() {
                let text = value
                    .as_str()
                    .ok_or_else(|| anyhow!("MCP {field} values must be strings"))?;
                *value = Value::String(resolve_text(text, environment)?);
            }
        }
    }
    Ok(definition)
}

fn resolve_text(text: &str, environment: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("${secret:") {
        output.push_str(&remaining[..start]);
        let reference = &remaining[start + 9..];
        let end = reference
            .find('}')
            .ok_or_else(|| anyhow!("invalid secret reference"))?;
        let name = &reference[..end];
        let value = environment
            .get(name)
            .ok_or_else(|| anyhow!("environment key {name} is not granted"))?;
        output.push_str(value);
        remaining = &reference[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn image_extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

fn env_i32(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn next_fire(
    schedule: &loka_shared::world::AgentSchedule,
    after: DateTime<Utc>,
) -> (Option<DateTime<Utc>>, bool) {
    if let Some(expression) = &schedule.cron_expression {
        return match CronSchedule::from_str(&loka_shared::cron_util::normalize(expression)) {
            Ok(cron) => (cron.after(&after).next(), false),
            Err(_) => (None, true),
        };
    }
    match schedule.run_at {
        Some(at) => (Some(at), false),
        None => (None, true),
    }
}

fn task_timeout() -> i32 {
    env_i32("WORLD_TASK_TIMEOUT_SECS", 3600)
}

fn with_rules(prompt: String, rules: &str) -> String {
    if rules.is_empty() {
        prompt
    } else {
        format!("{prompt}\n\n{rules}")
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn direct_system(agent: &WorldAgent, owner: &str, folder: Option<&str>, rules: &str) -> String {
    let persona = agent
        .persona
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or("A capable, friendly engineer who takes ownership of their work.");
    let addendum = agent
        .system_prompt_addendum
        .as_deref()
        .map(|a| format!("\nExtra instructions:\n{a}"))
        .unwrap_or_default();
    let folder = folder.filter(|f| !f.trim().is_empty()).unwrap_or("this folder");
    format!(
        "You are {name}, a resident of the Loka. {persona}{addendum}\n\nRight now you are pairing live with {owner} in their own copy of {folder} on their Mac, like a coding assistant in their editor. This is not a quest: there is no worktree, no review and no report block. Answer questions directly and make the edits they ask for in place. Keep replies short and to the point. Do not create branches, commit, push or open pull requests unless they ask you to. When they or their rules allow force-pushing, rebasing or rewriting history, do it (prefer --force-with-lease); that is the approval. The files are theirs and may have uncommitted work, so never discard or reset changes you did not make. TMPDIR is a folder private to this chat: keep scratch files there, and start any test server (PGlite, Postgres) on a free port you pick with its data inside it, never reusing a server you find already running.{rules}",
        name = agent.name,
        rules = if rules.is_empty() { String::new() } else { format!("\n\n{rules}") }
    )
}

fn tail(text: &str, max: usize) -> String {
    let length = text.chars().count();
    text.chars().skip(length.saturating_sub(max)).collect()
}

fn review_value(output: &str) -> Value {
    match prompts::readable_verdict(output) {
        Some(verdict) => json!({ "verdict": verdict.verdict, "findings": verdict.findings, "summary": tail(output, 800) }),
        None if prompts::parse_verdict(output).is_some_and(|v| v.verdict == "pick") => {
            json!({ "verdict": "pick", "summary": tail(output, 800) })
        }
        None => prompts::no_verdict(tail(output, 800)),
    }
}

fn may_attach_reported_pr(completion: &RunnerCompletion) -> bool {
    completion.publish_pr_state.as_deref() != Some(loka_shared::runners::PUBLISH_ELSEWHERE)
}

pub async fn parent_quest_pr(db: &Database, task: &AgentTask) -> Option<i64> {
    let parent = db.get_task(task.parent_task_id?).await.ok()??;
    if parent.repo.is_none() || parent.repo != task.repo || parent.pr_url.is_none() {
        return None;
    }
    parent.pr_number.map(i64::from)
}

#[cfg(test)]
mod tests {
    use super::{followup_step, incomplete_report, may_attach_reported_pr, prompts, review_value, FollowupStep};
    use loka_shared::runners::PrSync;

    #[tokio::test]
    async fn a_reset_whose_notes_turn_is_too_long_still_resets_and_keeps_the_notes() -> anyhow::Result<()> {
        use super::RunnerCoordinator;
        use loka_shared::{db::Database, world::WorldStore};
        use uuid::Uuid;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = Database::connect(&url).await?;
        let pool = db.pool().clone();
        let store = WorldStore::new(pool.clone());
        let coordinator = RunnerCoordinator::new(db, store.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("reset-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Tenzing', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("reset-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 190000) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .fetch_one(&pool)
        .await?;
        let notes = "Tenzing keeps the summit log.";
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("reset-{tag}-notes"), "notes", notes)
            .await?;
        assert!(store.request_context_reset(agent_id, serde_json::json!({"by": "owner"})).await?);
        let inbox: i64 = sqlx::query_scalar("UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1 RETURNING id")
            .bind(agent_id)
            .fetch_one(&pool)
            .await?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at)
             VALUES ($1, $2, 'turn', 'running', $3, NOW()) RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .bind(crate::prompts::RESET_PROMPT)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2)")
            .bind(run_id)
            .bind(inbox)
            .execute(&pool)
            .await?;
        let main = store.get_session(main_id).await?.expect("main");
        let agent = store.get_agent(agent_id).await?.expect("agent");

        coordinator
            .fail_run(run_id, "turn", &main, &agent, "prompt is too long: 215000 tokens > 200000 maximum", false)
            .await?;

        let old_status: String = sqlx::query_scalar("SELECT status FROM agent_sessions WHERE id = $1")
            .bind(main_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(old_status, "archived");
        let fresh = store.main_session(agent_id).await?.expect("a fresh main session");
        assert_ne!(fresh.id, main_id);
        assert!(fresh.summary.as_deref().unwrap_or_default().contains(notes));
        let kept = store.own_notes_skill(agent_id, Some(owner)).await?.expect("notes skill");
        assert_eq!(kept.body, notes);
        let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(inbox)
            .fetch_one(&pool)
            .await?;
        assert_eq!(status, "done");
        let noted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agent_messages WHERE agent_id = $1 AND author_kind = 'system' AND body = $2)",
        )
        .bind(agent_id)
        .bind(loka_shared::context::RESET_KEPT_NOTES)
        .fetch_one(&pool)
        .await?;
        assert!(noted);

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("reset-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn the_first_turn_of_a_forked_or_new_agent_runs_on_the_runner_and_the_local_worker() -> anyhow::Result<()> {
        use loka_shared::runners::{CreateRunner, RunnerStore};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            eprintln!("SKIPPED: set STEER_TEST_DATABASE_URL to a migrated database");
            return Ok(());
        };
        let pool = &harness.pool;
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("first-turn-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let source: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Source', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("source-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'remember the launch plan')")
            .bind(source)
            .bind(owner)
            .execute(pool)
            .await?;
        let fork = harness.store.fork_agent(source, owner, &format!("fork-{tag}"), "Fork", "Ana").await?.expect("fork").agent_id;
        let fresh: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Fresh', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("fresh-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let message = |agent: Uuid| {
            sqlx::query("INSERT INTO agent_inbox (agent_id, kind, payload) VALUES ($1, 'message', $2)")
                .bind(agent)
                .bind(serde_json::json!({ "body": "hello", "author_kind": "user", "from_owner": true }))
                .execute(pool)
        };

        for agent in [fork, fresh] {
            message(agent).await?;
            let mut taken = Vec::new();
            while let Some(claimed) = harness.store.claim_agent_inbox("local-worker").await? {
                taken.push(claimed);
                if claimed == agent {
                    break;
                }
            }
            assert_eq!(taken.last(), Some(&agent), "the local worker takes the first message of an agent that has no device");
            for claimed in taken {
                harness.store.release_agent_inbox(claimed, "local-worker").await?;
            }
            sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agent).execute(pool).await?;
        }

        let runners = RunnerStore::new(pool.clone());
        let runner = runners.register(owner, &CreateRunner { name: "First turn Mac".into(), platform: "macos".into() }).await?.runner.id;
        runners.heartbeat(runner, &serde_json::json!({ "backends": { "claude": true } })).await?;
        for (agent, handoff) in [(fork, true), (fresh, false)] {
            message(agent).await?;
            runners.adopt_stranded(runner, Some(agent)).await?;
            harness.coordinator.prepare_turn(runner).await?;
            let claim = runners
                .claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &["turn".to_string()])
                .await?
                .expect("the runner claims the first turn");
            assert_eq!(claim.agent_id, agent);
            let job = harness.coordinator.build_job(claim, format!("mcp-{}", Uuid::new_v4())).await?;
            assert_eq!(job.prompt.contains("[memory handoff]"), handoff, "only a fork starts from a remembered past");
            if handoff {
                assert!(job.prompt.contains("remember the launch plan"), "the fork's first turn carries what its source knew");
            }
            assert!(job.prompt.contains("hello"), "the message itself reaches the CLI");
        }
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![source, fork, fresh]).execute(pool).await?;
        Ok(())
    }

    struct Harness {
        pool: sqlx::PgPool,
        store: loka_shared::world::WorldStore,
        coordinator: super::RunnerCoordinator,
    }

    struct MergedQuest {
        agent: loka_shared::world::WorldAgent,
        task: uuid::Uuid,
        task_session: uuid::Uuid,
    }

    async fn harness() -> anyhow::Result<Option<Harness>> {
        use loka_shared::{db::Database, world::WorldStore};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        let db = Database::connect(&url).await?;
        let pool = db.pool().clone();
        let store = WorldStore::new(pool.clone());
        let mut coordinator = super::RunnerCoordinator::new(db, store.clone());
        coordinator.reviews_enabled = true;
        Ok(Some(Harness { pool, store, coordinator }))
    }

    async fn merged_quest(harness: &Harness, merged: bool) -> anyhow::Result<MergedQuest> {
        use uuid::Uuid;
        let pool = &harness.pool;
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("merged-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("merged-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, pr_url) VALUES ($1, 'ship it', 'in_progress', $2, 'https://github.com/acme/app/pull/9') RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1 AND kind = 'merge'").bind(task).execute(pool).await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', $2)")
            .bind(task)
            .bind(if merged { "done" } else { "active" })
            .execute(pool)
            .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, working_dir)
             VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, 'active', $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("cli-{tag}"))
        .bind(task)
        .bind(format!("tasks/{task}/app"))
        .fetch_one(pool)
        .await?;
        let agent = harness.store.get_agent(agent_id).await?.expect("agent");
        Ok(MergedQuest { agent, task, task_session })
    }

    async fn quest_status(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<String> {
        Ok(sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(quest.task).fetch_one(&harness.pool).await?)
    }

    async fn review_sessions(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE task_id = $1 AND role = 'review'")
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?)
    }

    async fn approved_earlier(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, created_at)
             VALUES ($1, $2, $3, 'task', 'completed', 'work', NOW() - INTERVAL '3 minutes')",
        )
        .bind(quest.task_session)
        .bind(quest.agent.id)
        .bind(quest.task)
        .execute(&harness.pool)
        .await?;
        let review: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, status)
             VALUES ($1, 'review', 'claude', 'sonnet', $2, $3, 'completed') RETURNING id",
        )
        .bind(quest.agent.id)
        .bind(quest.task_session)
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, created_at)
             VALUES ($1, $2, $3, 'review', 'completed', 'review', $4, NOW() - INTERVAL '2 minutes')",
        )
        .bind(review)
        .bind(quest.agent.id)
        .bind(quest.task)
        .bind("Looks right.\n<!-- verdict {\"verdict\": \"approve\", \"summary\": \"ok\"} verdict -->")
        .execute(&harness.pool)
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_followup_reviews_only_work_that_has_no_review_on_record() -> anyhow::Result<()> {
        use loka_shared::runners::PrSync;
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let output = "Merged it.\n<!-- report {\"status\": \"done\", \"summary\": \"merged\"} report -->";
        for (sync, reviewed, reviews, status) in [
            (PrSync::Merged, false, 0, "done"),
            (PrSync::Unchanged, true, 1, "done"),
            (PrSync::Unchanged, false, 1, "in_progress"),
            (PrSync::Changed, true, 2, "in_progress"),
            (PrSync::Changed, false, 1, "in_progress"),
        ] {
            let quest = merged_quest(&harness, false).await?;
            if reviewed {
                approved_earlier(&harness, &quest).await?;
            }
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'followup', 'completed', 'merge it') RETURNING id",
            )
            .bind(quest.task_session)
            .bind(quest.agent.id)
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            let session = harness.store.get_session(quest.task_session).await?.expect("task session");
            harness.coordinator.after_followup(&quest.agent, &session, run, output, None, sync).await?;
            assert_eq!(review_sessions(&harness, &quest).await?, reviews, "{sync:?} reviewed={reviewed}");
            assert_eq!(quest_status(&harness, &quest).await?, status, "{sync:?} reviewed={reviewed}");
            let finished: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT payload->>'verdict', payload->>'error' FROM world_events WHERE task_id = $1 AND type = 'task.finished'",
            )
            .bind(quest.task)
            .fetch_optional(&harness.pool)
            .await?;
            if sync == PrSync::Unchanged && reviewed {
                assert_eq!(finished, Some((Some("approve".into()), None)), "unchanged reviewed work finishes with its verdict");
            } else {
                assert_eq!(finished, None, "{sync:?} reviewed={reviewed}: nothing finishes before its review");
            }
            if sync == PrSync::Merged {
                let merged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.merged'")
                    .bind(quest.task)
                    .fetch_one(&harness.pool)
                    .await?;
                assert_eq!(merged, 1);
            }
        }

        let quest = merged_quest(&harness, false).await?;
        approved_earlier(&harness, &quest).await?;
        for answer in ["Nothing left.", "Still nothing."] {
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'followup', 'completed', $4) RETURNING id",
            )
            .bind(quest.task_session)
            .bind(quest.agent.id)
            .bind(quest.task)
            .bind(answer)
            .fetch_one(&harness.pool)
            .await?;
            let session = harness.store.get_session(quest.task_session).await?.expect("task session");
            harness.coordinator.after_followup(&quest.agent, &session, run, output, None, PrSync::Unchanged).await?;
        }
        assert_eq!(review_sessions(&harness, &quest).await?, 1, "follow-ups that change nothing keep the earlier review");
        assert_eq!(quest_status(&harness, &quest).await?, "done");
        Ok(())
    }

    #[tokio::test]
    async fn a_followup_that_changed_nothing_publishes_reviewed_work_and_reviews_the_rest() -> anyhow::Result<()> {
        use loka_shared::runners::PrSync;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        for reviewed in [false, true] {
            let quest = unpublished_quest(&harness, "claude").await?;
            sqlx::query("UPDATE agent_sessions SET review = TRUE, status = 'completed' WHERE id = $1")
                .bind(quest.task_session)
                .execute(&harness.pool)
                .await?;
            sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1").bind(quest.task).execute(&harness.pool).await?;
            if reviewed {
                approved_earlier(&harness, &quest).await?;
            }
            let followup = harness.store.start_followup(&task_session(&harness, &quest).await?, "Anything left?", "Abhi", false).await?;
            finish_run(&harness, followup.id).await?;
            harness
                .coordinator
                .after_followup(&quest.agent, &task_session(&harness, &quest).await?, followup.id, done_report("claude"), None, PrSync::Unchanged)
                .await?;
            let publishes = quest_runs(&harness, &quest, "publish").await?;
            if !reviewed {
                assert!(publishes.is_empty(), "work that was never reviewed is never published unreviewed");
                assert_eq!(review_sessions(&harness, &quest).await?, 1, "it gets its review instead");
                assert_ne!(quest_status(&harness, &quest).await?, "failed");
                continue;
            }
            assert_eq!(publishes.len(), 1, "reviewed work that a follow-up left alone publishes");
            assert_eq!(review_sessions(&harness, &quest).await?, 1, "without a second review");
            finish_run(&harness, publishes[0].0).await?;
            let completion: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
                "output": "Nothing to publish",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": null,
                "pr_number": null,
                "publish_pr_state": null,
            }))?;
            harness.coordinator.after_publish(&quest.agent, &task_session(&harness, &quest).await?, &completion).await?;
            let finished: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
                "SELECT payload->>'status', payload->>'verdict', payload->>'error' FROM world_events WHERE task_id = $1 AND type = 'task.finished'",
            )
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            assert_eq!(finished, (Some("done".into()), Some("approve".into()), None), "the quest finishes with the earlier verdict");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_first_run_with_review_on_is_reviewed_before_anything_publishes() -> anyhow::Result<()> {
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = unpublished_quest(&harness, "claude").await?;
        sqlx::query("UPDATE agent_sessions SET review = TRUE WHERE id = $1").bind(quest.task_session).execute(&harness.pool).await?;
        let run = completed_run(&harness, &quest, "task").await?;
        harness.coordinator.after_task(&quest.agent, &task_session(&harness, &quest).await?, run, done_report("claude"), None).await?;
        assert_eq!(review_sessions(&harness, &quest).await?, 1);
        assert!(quest_runs(&harness, &quest, "publish").await?.is_empty());
        assert_eq!(quest_status(&harness, &quest).await?, "in_progress");
        Ok(())
    }

    async fn quest_runs(harness: &Harness, quest: &MergedQuest, kind: &str) -> anyhow::Result<Vec<(uuid::Uuid, String)>> {
        Ok(sqlx::query_as("SELECT id, prompt FROM session_runs WHERE task_id = $1 AND kind = $2 ORDER BY created_at, id")
            .bind(quest.task)
            .bind(kind)
            .fetch_all(&harness.pool)
            .await?)
    }

    async fn finish_run(harness: &Harness, run: uuid::Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(run)
            .execute(&harness.pool)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_report_that_never_comes_saves_the_work_and_the_quest_stays_failed() -> anyhow::Result<()> {
        use loka_shared::runners::{PrSync, RunnerCompletion};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        sqlx::query("UPDATE agent_tasks SET repo = 'acme/app', pr_url = NULL, pr_number = NULL WHERE id = $1")
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
        let task_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'task', 'completed', 'do it') RETURNING id",
        )
        .bind(quest.task_session)
        .bind(quest.agent.id)
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        let work = Some("Commits ahead of main:\nabc fix");
        let coordinator = &harness.coordinator;
        let session = |harness: &Harness| {
            let store = harness.store.clone();
            let id = quest.task_session;
            async move { store.get_session(id).await.map(|session| session.expect("task session")) }
        };

        coordinator.after_task(&quest.agent, &session(&harness).await?, task_run, "All done. No commit or push performed.", work).await?;
        let reminders = quest_runs(&harness, &quest, "followup").await?;
        assert_eq!(reminders.len(), 1, "the first miss gets its reminder");
        assert_eq!(reminders[0].1, loka_shared::world::REPORT_NUDGE);

        finish_run(&harness, reminders[0].0).await?;
        coordinator.after_followup(&quest.agent, &session(&harness).await?, reminders[0].0, "Still done.", work, PrSync::Changed).await?;
        let reminders = quest_runs(&harness, &quest, "followup").await?;
        assert_eq!(reminders.len(), 2, "a reminder that still has no report gets one more");
        assert_eq!(reminders[1].1, loka_shared::world::REPORT_NUDGE_LAST);
        assert!(quest_runs(&harness, &quest, "publish").await?.is_empty(), "nothing is published while a reminder is still pending");

        finish_run(&harness, reminders[1].0).await?;
        coordinator.after_followup(&quest.agent, &session(&harness).await?, reminders[1].0, "Done, really.", work, PrSync::Changed).await?;
        assert_eq!(quest_runs(&harness, &quest, "followup").await?.len(), 2, "two reminders end the chase");
        assert_eq!(quest_runs(&harness, &quest, "publish").await?.len(), 1, "the work is published without a report");
        assert_eq!(quest_status(&harness, &quest).await?, "in_progress", "the quest settles only when the publish run reports back");
        assert_eq!(review_sessions(&harness, &quest).await?, 0, "unreported work is never reviewed");
        let stored = prompts::stored_report(session(&harness).await?.summary.as_deref()).expect("a stored report");
        assert!(stored.synthesized);
        assert_eq!(stored.status.as_deref(), Some("blocked"));

        let published = RunnerCompletion {
            output: String::new(),
            session_id: None,
            cost_usd: 0.0,
            exit_code: 0,
            usage: Default::default(),
            error: None,
            workspace_key: "key".into(),
            pr_url: Some("https://github.com/acme/app/pull/9".into()),
            pr_number: Some(9),
            publish_pr_state: None,
            checkpoint: None,
            steered: Vec::new(),
            retryable: false,
            work_summary: None,
        };
        coordinator.after_publish(&quest.agent, &session(&harness).await?, &published).await?;
        let (status, error, pr): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, error_message, pr_url FROM agent_tasks WHERE id = $1").bind(quest.task).fetch_one(&harness.pool).await?;
        assert_eq!(status, "failed", "a quest without its report never counts as done");
        assert_eq!(error.as_deref(), Some(prompts::SYNTHESIZED_ERROR));
        assert_eq!(pr.as_deref(), Some("https://github.com/acme/app/pull/9"), "the saved work is attached to the quest");
        let finished: Vec<(Option<String>, bool)> = sqlx::query_as(
            "SELECT payload->>'pr_url', COALESCE((payload#>>'{report,synthesized}')::boolean, false) FROM world_events WHERE task_id = $1 AND type = 'task.finished'",
        )
        .bind(quest.task)
        .fetch_all(&harness.pool)
        .await?;
        assert_eq!(finished, [(Some("https://github.com/acme/app/pull/9".to_string()), true)]);

        coordinator.after_followup(&quest.agent, &session(&harness).await?, reminders[1].0, "Done, really.", work, PrSync::Changed).await?;
        coordinator.after_publish(&quest.agent, &session(&harness).await?, &published).await?;
        assert_eq!(quest_runs(&harness, &quest, "publish").await?.len(), 1, "a stale re-finish never publishes again");
        let finished: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.finished'")
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(finished, 1, "a stale re-finish never announces the quest twice");
        Ok(())
    }

    #[tokio::test]
    async fn a_report_that_never_comes_on_a_quest_without_a_repo_just_fails_it() -> anyhow::Result<()> {
        use loka_shared::runners::PrSync;
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        sqlx::query("UPDATE agent_tasks SET pr_url = NULL, pr_number = NULL WHERE id = $1").bind(quest.task).execute(&harness.pool).await?;
        let mut nudge = Uuid::nil();
        for (kind, prompt) in [("task", "do it"), ("followup", loka_shared::world::REPORT_NUDGE), ("followup", loka_shared::world::REPORT_NUDGE_LAST)] {
            nudge = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, $4, 'completed', $5) RETURNING id",
            )
            .bind(quest.task_session)
            .bind(quest.agent.id)
            .bind(quest.task)
            .bind(kind)
            .bind(prompt)
            .fetch_one(&harness.pool)
            .await?;
        }
        let session = harness.store.get_session(quest.task_session).await?.expect("task session");
        harness.coordinator.after_followup(&quest.agent, &session, nudge, "Done.", None, PrSync::Changed).await?;
        assert_eq!(quest_runs(&harness, &quest, "publish").await?.len(), 0);
        assert_eq!(quest_status(&harness, &quest).await?, "failed");
        Ok(())
    }

    #[tokio::test]
    async fn a_run_refused_for_lack_of_disk_waits_instead_of_failing_its_quest() -> anyhow::Result<()> {
        use loka_shared::runners::{BeginCompletion, CreateRunner, RunnerStore, WORK_RUN_KINDS};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let runners = RunnerStore::new(harness.pool.clone());
        for (kind, role) in [("task", "task"), ("followup", "task"), ("publish", "task"), ("review", "review")] {
            assert!(WORK_RUN_KINDS.contains(&kind));
            let quest = merged_quest(&harness, false).await?;
            let owner = quest.agent.owner_user_id.expect("owner");
            sqlx::query("UPDATE users SET world_role = 'write' WHERE id = $1").bind(owner).execute(&harness.pool).await?;
            let runner = runners.register(owner, &CreateRunner { name: "Full Mac".into(), platform: "macos".into() }).await?.runner.id;
            sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(quest.agent.id).bind(runner).execute(&harness.pool).await?;
            sqlx::query("UPDATE agent_sessions SET runner_id = $2 WHERE agent_id = $1").bind(quest.agent.id).bind(runner).execute(&harness.pool).await?;
            let session = if role == "review" {
                sqlx::query_scalar(
                    "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, status, working_dir, cli_session_id, runner_id)
                     VALUES ($1, 'review', 'claude', 'sonnet', $2, $3, 'active', $4, 'cli-review', $5) RETURNING id",
                )
                .bind(quest.agent.id)
                .bind(quest.task_session)
                .bind(quest.task)
                .bind(format!("tasks/{}/app", quest.task))
                .bind(runner)
                .fetch_one(&harness.pool)
                .await?
            } else {
                quest.task_session
            };
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, $4, 'queued', 'go') RETURNING id",
            )
            .bind(session)
            .bind(quest.agent.id)
            .bind(quest.task)
            .bind(kind)
            .fetch_one(&harness.pool)
            .await?;
            let claim = runners
                .claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &WORK_RUN_KINDS.map(String::from))
                .await?
                .expect("claimed");
            assert_eq!(claim.run_id, run);
            let refused: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
                "output": "",
                "session_id": null,
                "cost_usd": 0.0,
                "exit_code": 1,
                "usage": {},
                "error": "No space left on device: this device has only 1.0 GB free, so Loka did not start this run.",
                "workspace_key": format!("tasks/{}/app", quest.task),
            }))?;
            let hash = vec![7u8; 32];
            assert_eq!(runners.begin_completion(runner, claim.lease_id, &hash, &refused).await?, BeginCompletion::Begun);
            let lease = runners.lease_for_completion(runner, claim.lease_id).await?.expect("lease");
            harness.coordinator.complete(&lease, &refused).await?;
            assert!(runners.finish_completion(runner, claim.lease_id).await?, "{kind}");

            let (status, waiting): (String, Option<String>) =
                sqlx::query_as("SELECT status, waiting_for FROM session_runs WHERE id = $1").bind(run).fetch_one(&harness.pool).await?;
            assert_eq!((status.as_str(), waiting.as_deref()), ("queued", Some("disk")), "{kind} waits");
            assert_eq!(quest_status(&harness, &quest).await?, "in_progress", "{kind} does not fail the quest");
            let failed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type IN ('run.failed', 'task.finished')")
                .bind(quest.task)
                .fetch_one(&harness.pool)
                .await?;
            assert_eq!(failed, 0, "{kind} announces no failure");
            let session_status: String = sqlx::query_scalar("SELECT status FROM agent_sessions WHERE id = $1").bind(session).fetch_one(&harness.pool).await?;
            assert_eq!(session_status, "active", "{kind}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_review_stopped_after_the_pr_merged_settles_the_quest_as_done() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        for (merged, status) in [(true, "done"), (false, "failed")] {
            let quest = merged_quest(&harness, merged).await?;
            let task_session = harness.store.get_session(quest.task_session).await?.expect("task session");
            harness.coordinator.review_failed(&quest.agent, &task_session, Uuid::new_v4(), "Stopped by Bun", false).await?;
            assert_eq!(quest_status(&harness, &quest).await?, status, "merged: {merged}");
            let nudged: bool = sqlx::query_scalar(
                "SELECT COALESCE(next_check_at IS NOT NULL AND next_check_at <= NOW(), FALSE) FROM task_milestones WHERE task_id = $1 AND kind = 'merge'",
            )
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            assert_eq!(nudged, !merged, "merged: {merged}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_stopped_review_of_a_published_quest_never_fails_it() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let stopped = "Stopped by Tara. Do not restart this work unless they ask.";
        let crashed = "timed out after 3600s";
        for (error, merge_done, repo, status, publishes) in [
            (stopped, true, true, "done", 0),
            (stopped, false, true, "in_progress", 1),
            (stopped, false, false, "failed", 0),
            (crashed, false, true, "failed", 0),
        ] {
            let quest = merged_quest(&harness, merge_done).await?;
            if repo {
                sqlx::query("UPDATE agent_tasks SET repo = 'acme/app', pr_number = 9 WHERE id = $1")
                    .bind(quest.task)
                    .execute(&harness.pool)
                    .await?;
            }
            let review: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, status)
                 VALUES ($1, 'review', 'claude', 'sonnet', $2, $3, 'active') RETURNING id",
            )
            .bind(quest.agent.id)
            .bind(quest.task_session)
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'review', 'running', 'review it') RETURNING id",
            )
            .bind(review)
            .bind(quest.agent.id)
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            let session = harness.store.get_session(review).await?.expect("review session");
            harness.coordinator.fail_run(run, "review", &session, &quest.agent, error, false).await?;
            assert_eq!(quest_status(&harness, &quest).await?, status, "{error} merged: {merge_done} repo: {repo}");
            let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE session_id = $1 AND kind = 'publish'")
                .bind(quest.task_session)
                .fetch_one(&harness.pool)
                .await?;
            assert_eq!(queued, publishes, "{error} merged: {merge_done} repo: {repo}");
        }
        Ok(())
    }

    #[test]
    fn a_publish_that_found_another_branchs_pr_never_falls_back_to_the_report() {
        let completion = |state: Option<&str>| -> loka_shared::runners::RunnerCompletion {
            serde_json::from_value(serde_json::json!({
                "output": "Nothing to publish",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": null,
                "pr_number": null,
                "publish_pr_state": state,
            }))
            .unwrap()
        };
        assert!(!may_attach_reported_pr(&completion(Some(loka_shared::runners::PUBLISH_ELSEWHERE))));
        assert!(may_attach_reported_pr(&completion(None)));
        assert!(may_attach_reported_pr(&completion(Some("merged"))));
    }
    use crate::prompts::Report;

    fn empty_report() -> Report {
        Report::default()
    }

    fn null_report() -> Report {
        serde_json::from_str(r#"{"status":null,"summary":null}"#).unwrap()
    }

    #[test]
    fn review_off_empty_or_null_report_does_not_open_review() {
        for report in [empty_report(), null_report()] {
            assert!(matches!(
                followup_step(false, &report, true, false, true, false, PrSync::Changed, None),
                FollowupStep::Nudge
            ));
            assert!(matches!(
                followup_step(false, &report, true, true, true, false, PrSync::Changed, None),
                FollowupStep::Nudge
            ));
            let step = followup_step(false, &report, true, false, false, false, PrSync::Changed, None);
            let FollowupStep::Fail(error) = step else {
                panic!("empty report was not failed");
            };
            assert_eq!(error, "Task did not provide a valid completion report");
            assert!(matches!(
                followup_step(true, &report, true, false, true, false, PrSync::Changed, None),
                FollowupStep::Leave
            ));
        }
        let done = Report {
            status: Some("done".to_string()),
            ..Report::default()
        };
        assert!(incomplete_report(&done).is_none());
        assert!(matches!(
            followup_step(false, &done, true, false, false, false, PrSync::Changed, None),
            FollowupStep::Settle
        ));
        assert!(matches!(
            followup_step(false, &done, true, true, false, false, PrSync::Changed, None),
            FollowupStep::Review
        ));
    }

    #[test]
    fn a_report_in_a_follow_up_reply_settles_the_quest() {
        let reply = "Fixed the review comments.\n<!-- report {\"status\":\"done\",\"summary\":\"fixed\",\"milestones_done\":[\"Tests pass\"]} report -->";
        let report = crate::prompts::parse_report(reply).expect("follow-up report");
        let stored = serde_json::to_string(&report).unwrap();
        let report = crate::prompts::stored_report(Some(&stored)).unwrap();
        assert!(incomplete_report(&report).is_none());
        assert!(matches!(followup_step(false, &report, true, false, true, false, PrSync::Changed, None), FollowupStep::Settle));
        assert!(matches!(followup_step(true, &report, true, true, false, false, PrSync::Changed, None), FollowupStep::Review));
    }

    #[test]
    fn a_report_that_never_came_still_saves_the_work() {
        let last_words = Report::synthesized("No commit or push performed.", Some("Commits ahead of main:\nabc fix"));
        assert!(super::salvages(&last_words, true));
        assert!(!super::salvages(&last_words, false), "a quest without a repo has nothing to publish");
        let blocked = Report { status: Some("blocked".to_string()), summary: Some("needs a key".to_string()), ..Report::default() };
        assert!(!super::salvages(&blocked, true), "a report the agent wrote itself is never published for it");
        assert!(!super::salvages(&Report::default(), true));
        assert!(matches!(followup_step(false, &last_words, true, true, false, true, PrSync::Changed, None), FollowupStep::Salvage), "never reviewed, never claimed done");
        assert!(matches!(followup_step(false, &last_words, true, false, false, true, PrSync::Changed, None), FollowupStep::Salvage));
        assert!(matches!(followup_step(false, &last_words, true, false, true, true, PrSync::Changed, None), FollowupStep::Salvage), "the nudge was spent when the report is synthesized");
        assert!(matches!(followup_step(false, &last_words, true, false, false, false, PrSync::Changed, None), FollowupStep::Fail(_)));
        assert!(matches!(followup_step(true, &last_words, true, false, false, true, PrSync::Changed, None), FollowupStep::Leave));
        assert_eq!(incomplete_report(&last_words).as_deref(), Some(crate::prompts::SYNTHESIZED_ERROR));
    }

    #[test]
    fn a_missing_verdict_carries_its_cause_in_plain_words() {
        let changes = review_value("<!-- verdict {\"verdict\":\"changes\",\"findings\":[{\"issue\":\"x\"}]} verdict -->");
        assert_eq!(changes["verdict"], "changes");
        assert_eq!(changes["findings"].as_array().map(Vec::len), Some(1));
        assert!(changes.get("note").is_none());
        assert!(changes.get("cause").is_none());
        let unreadable = review_value("Waiting on the test run.");
        assert_eq!(unreadable["verdict"], crate::prompts::NO_VERDICT);
        assert_eq!(unreadable["note"], crate::prompts::UNREADABLE_VERDICT_NOTE);
        assert_eq!((unreadable["cause"].as_str(), unreadable["reason"].as_str()), (Some("no_verdict"), Some(loka_shared::run_causes::MISSED_VERDICT)));
    }

    #[tokio::test]
    async fn a_quick_run_that_ended_without_a_message_gets_its_reply_delivered_once() -> anyhow::Result<()> {
        use super::RunnerCoordinator;
        use loka_shared::{db::Database, quick::Opening, world::{AsideChoice, WorldStore}};
        use uuid::Uuid;
        let Ok(url) = std::env::var("QUICK_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = Database::connect(&url).await?;
        let pool = db.pool().clone();
        let store = WorldStore::new(pool.clone());
        let coordinator = RunnerCoordinator::new(db, store.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Sam', 'write') RETURNING id")
            .bind(format!("lostq-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Quill', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("lostq-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status) VALUES ($1, 'main', 'claude', 'haiku', 'main-cli', 'active')")
            .bind(agent_id)
            .execute(&pool)
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let choice = AsideChoice::default();
        let open = || store.open_quick(Opening { agent_id, user_id: owner, asker: "Sam", text: "hi", choice: &choice, thread_root: None });
        let end = |run: Uuid, status: &'static str, output: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query("UPDATE session_runs SET status = $2, output = $3, error = CASE WHEN $2 = 'completed' THEN NULL ELSE 'The device went offline' END, finished_at = NOW() - INTERVAL '2 minutes' WHERE id = $1")
                    .bind(run)
                    .bind(status)
                    .bind(output)
                    .execute(&pool)
                    .await
            }
        };

        let answered = open().await?;
        store.post_quick_message(&agent, &answered, Some(owner), "what now?", None, serde_json::json!({})).await?;
        let run = store.queue_quick(&answered, "what now?").await?;
        end(run.id, "completed", "Take the short route \u{2014} it is faster.\n<!-- actions []  actions -->").await?;
        assert_eq!(coordinator.deliver_lost_quick_replies(Some(answered.id)).await?, 1);
        assert_eq!(coordinator.deliver_lost_quick_replies(None).await?, 0, "a reply is delivered once, by the read or the sweep");
        let rows = store.quick_messages(answered.id, 10).await?;
        let reply = rows.last().expect("reply");
        assert_eq!((reply.author_kind.as_str(), reply.body.as_str(), reply.run_id), ("agent", "Take the short route - it is faster.", Some(run.id)));
        assert!(!store.session_busy(answered.id).await?);

        let broken = open().await?;
        store.post_quick_message(&agent, &broken, Some(owner), "and now?", None, serde_json::json!({})).await?;
        let run = store.queue_quick(&broken, "and now?").await?;
        end(run.id, "failed", "").await?;
        assert_eq!(coordinator.deliver_lost_quick_replies(Some(broken.id)).await?, 1);
        let note = store.quick_messages(broken.id, 10).await?.pop().expect("note");
        assert_eq!(note.body, "I could not answer that: The device went offline");
        assert_eq!(note.meta["failed"], true, "a failure is marked so clients can say so");

        let empty = open().await?;
        let run = store.queue_quick(&empty, "ok").await?;
        end(run.id, "completed", "").await?;
        assert_eq!(coordinator.deliver_lost_quick_replies(Some(empty.id)).await?, 1);
        assert_eq!(store.quick_messages(empty.id, 10).await?.pop().expect("done").body, "Done.");
        Ok(())
    }

    async fn unpublished_quest(harness: &Harness, cli: &str) -> anyhow::Result<MergedQuest> {
        let quest = merged_quest(harness, false).await?;
        sqlx::query("UPDATE agent_tasks SET repo = 'acme/ghar-mail', pr_url = NULL, pr_number = NULL WHERE id = $1")
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET backend = $2, review = FALSE WHERE id = $1")
            .bind(quest.task_session)
            .bind(cli)
            .execute(&harness.pool)
            .await?;
        Ok(quest)
    }

    async fn completed_run(harness: &Harness, quest: &MergedQuest, kind: &str) -> anyhow::Result<uuid::Uuid> {
        Ok(sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, started_at, finished_at)
             VALUES ($1, $2, $3, $4, 'completed', 'work', NOW(), NOW()) RETURNING id",
        )
        .bind(quest.task_session)
        .bind(quest.agent.id)
        .bind(quest.task)
        .bind(kind)
        .fetch_one(&harness.pool)
        .await?)
    }

    async fn task_session(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<loka_shared::world::AgentSession> {
        Ok(harness.store.get_session(quest.task_session).await?.expect("task session"))
    }

    async fn session_status(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<String> {
        Ok(task_session(harness, quest).await?.status)
    }

    fn done_report(cli: &str) -> &'static str {
        match cli {
            "codex" => "Ready for Loka to publish.\n<!-- report\n{\"status\":\"done\",\"summary\":\"Ready to publish\",\"pr_title\":\"fix: mail\"}\n-->",
            "cursor" => "<!-- report {\"status\": \"done\", \"summary\": \"Ready to publish\"} report -->",
            "omp" => "Done.\n\n<!-- report\n{\"status\": \"done\", \"summary\": \"Ready to publish\"}\nreport -->\n",
            _ => "Done.\n<!-- report\n{\"status\": \"done\", \"summary\": \"Ready to publish\", \"pr_title\": \"fix: mail\"}\nreport -->",
        }
    }

    const WAITING_ON_PUBLISH: &str =
        "<!-- report {\"status\": \"blocked\", \"summary\": \"The commit is ready, waiting for Loka to push it and open the PR\"} report -->";

    async fn block_first_run(harness: &Harness, quest: &MergedQuest) -> anyhow::Result<()> {
        let run = completed_run(harness, quest, "task").await?;
        harness.coordinator.after_task(&quest.agent, &task_session(harness, quest).await?, run, WAITING_ON_PUBLISH, None).await?;
        assert_eq!(quest_status(harness, quest).await?, "failed");
        assert_eq!(session_status(harness, quest).await?, "failed");
        Ok(())
    }

    async fn answer_followup(harness: &Harness, quest: &MergedQuest, run: uuid::Uuid, output: &str) -> anyhow::Result<()> {
        finish_run(harness, run).await?;
        harness
            .coordinator
            .after_followup(&quest.agent, &task_session(harness, quest).await?, run, output, None, loka_shared::runners::PrSync::Changed)
            .await
    }

    #[tokio::test]
    async fn a_done_report_publishes_from_every_starting_state() -> anyhow::Result<()> {
        use loka_shared::world::{TaskResume, FOLLOWUP_ASK, REOPENED_ASK};
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        for cli in ["claude", "codex", "cursor", "omp", "opencode"] {
            let first = unpublished_quest(&harness, cli).await?;
            let run = completed_run(&harness, &first, "task").await?;
            harness.coordinator.after_task(&first.agent, &task_session(&harness, &first).await?, run, done_report(cli), None).await?;
            assert_eq!(quest_runs(&harness, &first, "publish").await?.len(), 1, "{cli}: a first run that reports done publishes");

            let blocked = unpublished_quest(&harness, cli).await?;
            block_first_run(&harness, &blocked).await?;
            assert!(quest_runs(&harness, &blocked, "publish").await?.is_empty(), "{cli}: a blocked report publishes nothing");
            let followup = harness.store.start_followup(&task_session(&harness, &blocked).await?, "Report done so Loka publishes.", "Abhi", false).await?;
            assert!(followup.prompt.ends_with(REOPENED_ASK), "{cli}: the follow-up says the quest is open again");
            assert_eq!(session_status(&harness, &blocked).await?, "active", "{cli}");
            let (status, error): (String, Option<String>) =
                sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1").bind(blocked.task).fetch_one(&harness.pool).await?;
            assert_eq!((status.as_str(), error), ("in_progress", None), "{cli}: the quest is back in progress");
            answer_followup(&harness, &blocked, followup.id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &blocked, "publish").await?.len(), 1, "{cli}: a done follow-up on a blocked quest publishes");

            let failed = unpublished_quest(&harness, cli).await?;
            let run = completed_run(&harness, &failed, "task").await?;
            harness.coordinator.fail_run(run, "task", &task_session(&harness, &failed).await?, &failed.agent, "Timed out after 3600 seconds", true).await?;
            assert_eq!(quest_status(&harness, &failed).await?, "failed");
            let followup = harness.store.start_followup(&task_session(&harness, &failed).await?, "Finish it and report done.", "Abhi", false).await?;
            answer_followup(&harness, &failed, followup.id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &failed, "publish").await?.len(), 1, "{cli}: a done follow-up on a failed quest publishes");

            let resumed = unpublished_quest(&harness, cli).await?;
            block_first_run(&harness, &resumed).await?;
            let TaskResume::Resumed { run_id, .. } = harness.store.resume_task_session(resumed.agent.id, resumed.task, "Fea", Some("Pick it up")).await? else {
                panic!("{cli}: the quest resumes in its own session");
            };
            assert_eq!(quest_status(&harness, &resumed).await?, "in_progress", "{cli}: a resumed quest is back in progress");
            answer_followup(&harness, &resumed, run_id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &resumed, "publish").await?.len(), 1, "{cli}: a resumed quest that reports done publishes");

            let feedback = unpublished_quest(&harness, cli).await?;
            block_first_run(&harness, &feedback).await?;
            let merge: uuid::Uuid = sqlx::query_scalar("SELECT id FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
                .bind(feedback.task)
                .fetch_one(&harness.pool)
                .await?;
            let run_id = loka_shared::milestones::MilestoneStore::new(harness.pool.clone())
                .reopen_for_feedback(feedback.task, merge, "Address the review comment.")
                .await?
                .expect("PR feedback reopens the quest session");
            assert_eq!(quest_status(&harness, &feedback).await?, "in_progress", "{cli}: PR feedback puts a failed quest back in progress");
            answer_followup(&harness, &feedback, run_id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &feedback, "publish").await?.len(), 1, "{cli}: a done answer to PR feedback publishes");

            let in_review = unpublished_quest(&harness, cli).await?;
            sqlx::query("UPDATE agent_tasks SET status = 'review' WHERE id = $1").bind(in_review.task).execute(&harness.pool).await?;
            sqlx::query("UPDATE agent_sessions SET status = 'completed' WHERE id = $1").bind(in_review.task_session).execute(&harness.pool).await?;
            let followup = harness.store.start_followup(&task_session(&harness, &in_review).await?, "Fix the comment.", "Abhi", false).await?;
            assert!(followup.prompt.ends_with(FOLLOWUP_ASK), "{cli}: a finished quest is not reopened");
            assert_eq!(quest_status(&harness, &in_review).await?, "review");
            answer_followup(&harness, &in_review, followup.id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &in_review, "publish").await?.len(), 1, "{cli}: a done follow-up on a quest in review publishes");

            let done = unpublished_quest(&harness, cli).await?;
            sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(done.task).execute(&harness.pool).await?;
            sqlx::query("UPDATE agent_sessions SET status = 'completed' WHERE id = $1").bind(done.task_session).execute(&harness.pool).await?;
            let first = harness.store.start_followup(&task_session(&harness, &done).await?, "One more tweak.", "Abhi", false).await?;
            let second = harness.store.start_followup(&task_session(&harness, &done).await?, "And the docs.", "Abhi", false).await?;
            answer_followup(&harness, &done, first.id, done_report(cli)).await?;
            assert!(quest_runs(&harness, &done, "publish").await?.is_empty(), "{cli}: a follow-up still waiting holds the publish");
            answer_followup(&harness, &done, second.id, done_report(cli)).await?;
            assert_eq!(quest_runs(&harness, &done, "publish").await?.len(), 1, "{cli}: a done follow-up on a done quest publishes");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_reopened_quest_that_stays_blocked_says_so_and_a_cancelled_one_stays_closed() -> anyhow::Result<()> {
        use loka_shared::world::FOLLOWUP_ASK;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = unpublished_quest(&harness, "codex").await?;
        block_first_run(&harness, &quest).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &quest).await?, "Try again.", "Abhi", false).await?;
        let still = "<!-- report {\"status\": \"blocked\", \"summary\": \"The staging database is down\"} report -->";
        answer_followup(&harness, &quest, followup.id, still).await?;
        assert_eq!(quest_status(&harness, &quest).await?, "failed");
        assert_eq!(session_status(&harness, &quest).await?, "failed");
        assert!(quest_runs(&harness, &quest, "publish").await?.is_empty());
        let finished: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT payload#>>'{report,summary}' FROM world_events WHERE task_id = $1 AND type = 'task.finished' ORDER BY seq",
        )
        .bind(quest.task)
        .fetch_all(&harness.pool)
        .await?;
        assert_eq!(finished.last().cloned().flatten().as_deref(), Some("The staging database is down"), "the new reason reaches the main session");
        assert_eq!(finished.len(), 2);

        let same = unpublished_quest(&harness, "codex").await?;
        block_first_run(&harness, &same).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &same).await?, "Report done so Loka publishes.", "Abhi", false).await?;
        answer_followup(&harness, &same, followup.id, WAITING_ON_PUBLISH).await?;
        let (status, error): (String, Option<String>) =
            sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1").bind(same.task).fetch_one(&harness.pool).await?;
        assert_eq!(status, "failed", "the same blocked report fails the reopened quest again");
        assert!(error.is_some(), "and says why");
        let finished: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.finished'")
            .bind(same.task)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(finished, 2, "the main session hears about the repeated block");

        let timed_out = unpublished_quest(&harness, "claude").await?;
        let run = completed_run(&harness, &timed_out, "task").await?;
        harness.coordinator.fail_run(run, "task", &task_session(&harness, &timed_out).await?, &timed_out.agent, "Timed out after 3600 seconds", true).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &timed_out).await?, "Keep going.", "Abhi", false).await?;
        assert_eq!(quest_status(&harness, &timed_out).await?, "in_progress");
        harness
            .coordinator
            .fail_run(followup.id, "followup", &task_session(&harness, &timed_out).await?, &timed_out.agent, "Timed out after 3600 seconds", true)
            .await?;
        let (status, error): (String, Option<String>) =
            sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1").bind(timed_out.task).fetch_one(&harness.pool).await?;
        assert_eq!(status, "failed", "a second timeout fails the reopened quest again");
        assert!(error.is_some_and(|error| error.contains("Timed out")));

        let done = unpublished_quest(&harness, "claude").await?;
        block_first_run(&harness, &done).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done', error_message = NULL WHERE id = $1").bind(done.task).execute(&harness.pool).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &done).await?, "One more tweak.", "Abhi", false).await?;
        assert!(followup.prompt.ends_with(FOLLOWUP_ASK), "a quest that is not failed is never told it ended unfinished");
        assert_eq!(session_status(&harness, &done).await?, "active", "its follow-up still advances");
        assert_eq!(quest_status(&harness, &done).await?, "done");

        let cancelled = unpublished_quest(&harness, "claude").await?;
        block_first_run(&harness, &cancelled).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'cancelled' WHERE id = $1").bind(cancelled.task).execute(&harness.pool).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &cancelled).await?, "What did you change?", "Abhi", false).await?;
        assert!(followup.prompt.ends_with(FOLLOWUP_ASK));
        assert_eq!(session_status(&harness, &cancelled).await?, "failed", "a cancelled quest is never reopened");
        assert_eq!(quest_status(&harness, &cancelled).await?, "cancelled");
        Ok(())
    }
}
