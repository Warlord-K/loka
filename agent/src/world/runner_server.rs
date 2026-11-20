use std::collections::{BTreeMap, HashSet};

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use loka_shared::{
    db::Database,
    guests::{asked_by_user, in_thread},
    milestones::MilestoneStore,
    models::AgentTask,
    runners::{
        ApplyCompletion, ClaimedRun, LeaseContext, RunnerCompletion, RunnerEvent, RunnerImage,
        RunnerJob, RunnerPublish, RunnerSkill, RunnerStore, RunnerUsage, RunnerWorkspace,
    },
    world::{
        resume_prompt, AgentSession, InboxItem, NewEvent, NewMessage, NewRun, NewSession, ReviewerChoice, SessionRun,
        TaskResume, WorldAgent, WorldStore,
    },
};
use serde_json::{json, Map, Value};
use std::str::FromStr;
use uuid::Uuid;

use crate::prompts::{self, Action, Report};
use loka_shared::run_causes::Cause;

const TURN_BATCH: i64 = 20;

#[derive(Clone)]
pub struct RunnerCoordinator {
    db: Database,
    store: WorldStore,
    runners: RunnerStore,
    owner_name: String,
    reviews_enabled: bool,
    max_review_rounds: i32,
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
                    self.store.requeue_inbox(&reset_ids, &error.to_string()).await?;
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
                    self.store.requeue_inbox(&compact_ids, &error.to_string()).await?;
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
                self.store.requeue_inbox(&ids, &error.to_string()).await?;
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
        tracing::info!(agent = %agent.slug, old = %main.id, new = %fresh.id, "Main session reset after saving notes");
        let _ = self.store.announce_context(agent.id).await;
        self.prepare_claimed_turn(runner_id, agent.id).await?;
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
        if main.cli_session_id.is_some() && main.rules_hash.as_deref() != Some(rules_hash.as_str())
        {
            let instructions = [system.as_str(), rules_text.as_str()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
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
            })
            .await?;
        self.runners.set_run_rules_hash(run.id, &rules_hash).await?;
        Ok(run)
    }

    pub async fn build_job(&self, claimed: ClaimedRun, mcp_token: String) -> Result<RunnerJob> {
        let (session, agent, runner, task, parent_cli, skills, images, options, previous_run_id, live_sessions, keys_version) = tokio::try_join!(
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
                    self.run_images(claimed.run_id).await
                } else {
                    Ok(Vec::new())
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
        let (all_environment, omp, provider_keys, publish) = tokio::try_join!(
            self.store.environment_for_agent(agent.id),
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
        let environment = if matches!(claimed.kind.as_str(), "task" | "followup" | "direct" | "quick") {
            self.store.without_mcp_secrets(agent.id, all_environment.clone()).await?
        } else {
            BTreeMap::new()
        };
        let mcp_servers = if claimed.kind == "aside" {
            json!({})
        } else {
            self.mcp_servers(agent.id, claimed.kind == "review", &all_environment)
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
            prompt: claimed.prompt,
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
            provider_keys,
            images,
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
            .ok_or_else(|| anyhow!("session has no workspace"))?;
        if expected != completion.workspace_key {
            bail!("completion workspace does not match the leased session")
        }
        let environment = self.store.secrets_for_agent(agent.id).await?;
        let output = redact(&completion.output, environment.values());
        let error = completion
            .error
            .as_deref()
            .map(|value| redact(value, environment.values()));
        if completion.exit_code != 0 || error.is_some() {
            let message = error
                .unwrap_or_else(|| format!("runner process exited with {}", completion.exit_code));
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
        let accounted = if lease.kind == "publish" {
            Accounted {
                baseline: session.usage_baseline.clone(),
                cost_usd: 0.0,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
            }
        } else {
            self.account(&session, completion)
        };
        let compaction = self.is_compaction(lease.run_id, &lease.kind).await?;
        let reset = self.is_reset(lease.run_id, &lease.kind).await?;
        let context_tokens = match completion.usage.context_tokens {
            _ if lease.kind == "publish" => session.context_tokens,
            _ if compaction => 0,
            measured => loka_shared::context::used(measured, completion.usage.input_tokens, completion.usage.cached_input_tokens),
        };
        let applied = self
            .runners
            .apply_success(
                lease,
                &output,
                completion.session_id.as_deref(),
                &accounted.baseline,
                context_tokens,
                accounted.cost_usd,
                accounted.input_tokens,
                accounted.cached_input_tokens,
                accounted.output_tokens,
            )
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
        self.emit_run_event_once(
            "run.completed",
            lease.run_id,
            &session,
            agent.id,
            json!({ "cost_usd": accounted.cost_usd }),
            false,
        )
        .await?;
        if lease.kind == "turn" {
            self.store
                .set_run_checkpoint(lease.run_id, completion.checkpoint.as_deref())
                .await?;
            if session.rewind.is_some() {
                self.store.clear_session_rewind(session.id).await?;
            }
        }
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
                self.after_followup(&agent, &session, lease.run_id, &output, completion.work_summary.as_deref())
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
        Ok(())
    }

    pub async fn redact_completion(
        &self,
        lease: &LeaseContext,
        completion: &RunnerCompletion,
    ) -> Result<RunnerCompletion> {
        let environment = self.store.secrets_for_agent(lease.agent_id).await?;
        let mut completion = completion.clone();
        completion.output = redact(&completion.output, environment.values());
        completion.error = completion
            .error
            .as_deref()
            .map(|value| redact(value, environment.values()));
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
        let server = crate::mcp::Server::new(self.store.clone(), self.db.clone(), caller).in_quest(quest, session).in_quick(quick);
        let server = if read_only { server.read_only() } else { server };
        let server = if self.store.runner_owner(lease.runner_id).await? == Some(lease.owner_user_id) {
            server
        } else {
            server.on_shared_device()
        };
        Ok(server.jsonrpc(message).await)
    }

    pub async fn redact_events(
        &self,
        lease: &LeaseContext,
        events: &[RunnerEvent],
    ) -> Result<Vec<RunnerEvent>> {
        let environment = self.store.secrets_for_agent(lease.agent_id).await?;
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
                        .map(|tool| redact(tool, environment.values())),
                    text: redact(&event.text, environment.values()),
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
        let thread = self.store.reply_thread(agent, &items).await?;
        let root = loka_shared::replies::reply_root(&items);
        let (text, actions) = prompts::split_actions(output);
        let mut replied = false;
        let mut started = HashSet::new();
        for action in actions {
            match action {
                Action::Reply { text, next } if !text.trim().is_empty() => {
                    self.reply(agent, main, run_id, &text, &next, thread, root).await?;
                    replied = true;
                }
                Action::StartTask { task_id, brief, review, reviewer, time_limit_minutes, milestones, quest } => {
                    let Ok(task_id) = task_id.parse::<Uuid>() else {
                        continue;
                    };
                    let choice = reviewer.as_ref().and_then(ReviewerChoice::from_value);
                    if crate::quests::assigned_elsewhere(&self.store, task_id, agent.id).await {
                        continue;
                    }
                    if started.insert(task_id) {
                        if let Err(error) = crate::quests::apply_choice(&self.store, task_id, &quest).await {
                            self.system_note(agent, &format!("Could not set the quest options on task {task_id}: {error}")).await?;
                            continue;
                        }
                        crate::quests::seed_milestones(&self.store, agent.id, task_id, &milestones).await;
                        if let Err(error) = self
                            .start_task(agent, main, task_id, brief.as_deref(), review.unwrap_or(false) || choice.is_some(), choice.as_ref())
                            .await
                        {
                            self.system_note(
                                agent,
                                &format!("Could not start task {task_id}: {error}"),
                            )
                            .await?;
                        } else if let Some(minutes) = time_limit_minutes {
                            if let Err(error) = self.store.set_task_time_limit(task_id, Some(minutes)).await {
                                self.system_note(agent, &format!("Could not set the time limit on task {task_id}: {error}")).await?;
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
                    self.attention(agent, main, run_id, &text, Vec::new(), Vec::new())
                        .await?;
                    replied = true;
                }
                Action::Ask { text, choices } if !text.trim().is_empty() => {
                    let choices = choices
                        .into_iter()
                        .map(|choice| prompts::plain_dashes(choice.trim()))
                        .filter(|choice| !choice.is_empty())
                        .take(4)
                        .collect();
                    self.attention(agent, main, run_id, &text, choices, Vec::new()).await?;
                    replied = true;
                }
                Action::Drafts { text, items } if !text.trim().is_empty() => {
                    let drafts = prompts::clean_drafts(&items);
                    let body = if drafts.is_empty() { text } else { prompts::drafts_body(&text, &drafts) };
                    self.attention(agent, main, run_id, &body, Vec::new(), drafts).await?;
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
            if started.contains(&task_id)
                || replied
                || self.store.quest_has_parts(task_id).await.unwrap_or(false)
                || crate::quests::assigned_elsewhere(&self.store, task_id, agent.id).await
            {
                continue;
            }
            let brief = (!text.trim().is_empty()).then_some(text.as_str());
            if let Err(error) = self.start_task(agent, main, task_id, brief, false, None).await {
                self.system_note(agent, &format!("Could not start task {task_id}: {error}"))
                    .await?;
            }
        }
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
    ) -> Result<()> {
        let task = self
            .db
            .get_task(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;
        if task.assigned_agent_id != Some(agent.id) {
            bail!("task is not assigned to this agent")
        }
        match self
            .store
            .resume_task_session(agent.id, task.id, &resume_prompt(&agent.name, brief))
            .await?
        {
            TaskResume::Busy(session_id) => bail!(
                "it is already being worked on in session {session_id}; use message_session to steer that session instead of starting it again"
            ),
            TaskResume::Resumed { session_id, run_id } => {
                self.db.update_status(task.id, "in_progress").await?;
                self.store
                    .emit(
                        NewEvent::new("task.started")
                            .agent(agent.id)
                            .session(session_id)
                            .run(run_id)
                            .task(Some(task.id))
                            .payload(json!({ "task_id": task.id, "title": task.title, "repo": task.repo, "resumed": true })),
                        None,
                    )
                    .await?;
                self.store
                    .post_quest_started(agent.id, task.id, session_id, &task.title, brief.filter(|b| !b.trim().is_empty()))
                    .await?;
                return Ok(());
            }
            TaskResume::Fresh => {}
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
                    .payload(crate::quests::started_payload(&task, &options, opened.len())),
                None,
            )
            .await?;
        crate::quests::label_attempts(&self.store, &task, &opened).await?;
        self.store
            .post_quest_started(agent.id, task.id, session.id, &task.title, brief.filter(|b| !b.trim().is_empty()))
            .await?;
        Ok(())
    }

    async fn after_task(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        output: &str,
        work: Option<&str>,
    ) -> Result<()> {
        if session.planning() {
            let run = self.store.get_run(run_id).await?.ok_or_else(|| anyhow!("completed task run disappeared"))?;
            return crate::quests::plan_step(&self.store, agent, session, &run, output).await;
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
        if let Some(error) = incomplete_report(&report) {
            return self
                .finalize_task(agent, &session, None, Some(error), None, None)
                .await;
        }
        if self.reviews_enabled && session.wants_review() {
            self.queue_review(agent, &session, run_id).await
        } else {
            let has_repo = match session.task_id {
                Some(task_id) => self
                    .db
                    .get_task(task_id)
                    .await?
                    .and_then(|task| task.repo)
                    .is_some(),
                None => false,
            };
            if has_repo {
                self.queue_publish(agent, &session, run_id).await
            } else {
                self.finalize_task(agent, &session, None, None, None, None)
                    .await
            }
        }
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
            return Ok(self.store.auto_retry(run_id, &diagnosis).await?.is_some());
        }
        if self.store.nudge_for_report(session.id).await?.is_some() {
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
        let verdict_value = review_value(output, !manual);
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
                .ok_or_else(|| anyhow!("task has no workspace"))?;
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
        if verdict.is_none() && self.store.review_runs_since_work(task_session.id).await? < 2 {
            return self.queue_review(agent, &task_session, run_id).await;
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
            })
            .await?;
        self.store
            .set_session_status(task_session.id, "active", None)
            .await?;
        self.store
            .set_run_activity(&run, "Publishing reviewed work")
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
        if pr.is_none() {
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
                    .map(|output| review_value(&output, true));
                self.finalize_task(agent, session, verdict, None, pr_url, pr_number)
                    .await
            }
        }
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
    ) -> Result<()> {
        if session.role == "task" && session.planning() {
            let run = self.store.get_run(run_id).await?.ok_or_else(|| anyhow!("completed follow-up disappeared"))?;
            return crate::quests::plan_step(&self.store, agent, session, &run, output).await;
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
        if session.role == "task" && matches!(session.status.as_str(), "active" | "completed") {
            let was_completed = session.status == "completed";
            let advance = was_completed || self.store.complete_followup(session.id).await?;
            if !advance {
                return Ok(());
            }
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
            let step = followup_step(
                was_completed,
                &report,
                self.reviews_enabled,
                session.wants_review(),
                nudge_available,
            );
            if session.racing() && !matches!(step, FollowupStep::Nudge | FollowupStep::Leave) {
                return self.attempt_done(agent, &session, run_id, !matches!(step, FollowupStep::Fail(_))).await;
            }
            match step {
                FollowupStep::Nudge | FollowupStep::Leave => {}
                FollowupStep::Fail(error) => {
                    self.finalize_task(agent, &session, None, Some(error), None, None)
                        .await?;
                }
                FollowupStep::Review => {
                    self.queue_review(agent, &session, run_id).await?;
                }
                FollowupStep::Settle => {
                    let has_repo = match session.task_id {
                        Some(task_id) => self
                            .db
                            .get_task(task_id)
                            .await?
                            .and_then(|task| task.repo)
                            .is_some(),
                        None => false,
                    };
                    if has_repo {
                        self.queue_publish(agent, &session, run_id).await?;
                    } else {
                        self.finalize_task(agent, &session, None, None, None, None)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn after_quick(&self, agent: &WorldAgent, session: &AgentSession, run_id: Uuid, output: &str) -> Result<()> {
        let text = prompts::plain_dashes(&prompts::split_actions(output).0);
        let text = if text.trim().is_empty() { "Done.".to_string() } else { text };
        self.store.record_quick_reply(agent, session, run_id, &text, false).await?;
        Ok(())
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
                task_status = if error.is_some() {
                    "failed"
                } else if pr_url.is_some() || review_wants_changes {
                    "review"
                } else {
                    "done"
                };
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
                        return self
                            .finalize_task(
                                agent,
                                &task_session,
                                Some(json!({ "verdict": "review failed", "summary": error })),
                                Some(error.to_string()),
                                None,
                                None,
                            )
                            .await;
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
        choices: Vec<String>,
        drafts: Vec<prompts::Draft>,
    ) -> Result<()> {
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
                    json!({ "kind": "ask", "choices": choices })
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
                        "choices": choices
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

    async fn run_images(&self, run_id: Uuid) -> Result<Vec<RunnerImage>> {
        let payloads = sqlx::query_scalar::<_, Value>(
            "SELECT i.payload FROM agent_inbox i
             JOIN runner_run_inbox x ON x.inbox_id = i.id WHERE x.run_id = $1 ORDER BY i.id",
        )
        .bind(run_id)
        .fetch_all(self.store.pool())
        .await?;
        let ids = payloads
            .iter()
            .filter_map(|payload| payload.get("attachments").and_then(Value::as_array))
            .flatten()
            .filter(|attachment| attachment.get("mime").and_then(Value::as_str).is_none_or(prompts::is_image))
            .filter_map(|attachment| attachment.get("id").and_then(Value::as_str))
            .filter_map(|id| id.parse::<Uuid>().ok())
            .take(8)
            .collect::<Vec<_>>();
        let uploads = futures_util::future::try_join_all(ids.iter().map(|id| self.store.get_upload(*id, None))).await?;
        Ok(ids
            .into_iter()
            .zip(uploads)
            .filter_map(|(id, upload)| {
                let (mime_type, bytes) = upload.filter(|(mime, _)| prompts::is_image(mime))?;
                Some(RunnerImage {
                    id,
                    name: format!("{id}.{}", image_extension(&mime_type)),
                    mime_type,
                    data_base64: STANDARD.encode(bytes),
                })
            })
            .collect())
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
        })
    }

    fn account(&self, session: &AgentSession, completion: &RunnerCompletion) -> Accounted {
        account_usage(session, completion.cost_usd, completion.usage)
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
        let spent = account_usage(&session, 0.0, usage);
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

fn account_usage(session: &AgentSession, reported_cost: f64, reported: RunnerUsage) -> Accounted {
    let previous_f64 = |key: &str| {
        session
            .usage_baseline
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let mut baseline = session.usage_baseline.clone();
    if !baseline.is_object() {
        baseline = json!({});
    }
    let usage = if session.backend == "codex" {
        reported.record(&mut baseline);
        reported.since(&session.usage_baseline)
    } else {
        reported
    };
    let RunnerUsage { input_tokens, cached_input_tokens, output_tokens, .. } = usage;
    let cost_usd = if loka_shared::backends::cost_per_run(&session.backend) && reported_cost > 0.0 {
        reported_cost
    } else if reported_cost > 0.0 {
        let previous = previous_f64("cost_usd");
        if let Some(object) = baseline.as_object_mut() {
            object.insert("cost_usd".into(), json!(reported_cost));
        }
        loka_shared::pricing::session_cost_delta(reported_cost, previous, &session.model, input_tokens, cached_input_tokens, output_tokens)
    } else {
        loka_shared::pricing::cost(&session.model, input_tokens, cached_input_tokens, output_tokens)
    };
    Accounted {
        baseline,
        cost_usd,
        input_tokens: input_tokens as i64,
        cached_input_tokens: cached_input_tokens as i64,
        output_tokens: output_tokens as i64,
    }
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

struct Accounted {
    baseline: Value,
    cost_usd: f64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
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

fn redact<'a>(text: &str, values: impl Iterator<Item = &'a String>) -> String {
    let mut values = values.filter(|value| !value.is_empty()).collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values.into_iter().fold(text.replace('\0', ""), |text, value| {
        text.replace(value, "[REDACTED]")
    })
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

fn incomplete_report(report: &Report) -> Option<String> {
    if report.status.as_deref() == Some("done") {
        return None;
    }
    if report.synthesized {
        return Some(prompts::SYNTHESIZED_ERROR.to_string());
    }
    Some(match report.summary.as_deref() {
        Some(summary) => format!(
            "Task reported {}: {summary}",
            report.status.as_deref().unwrap_or("no status")
        ),
        None => "Task did not provide a valid completion report".to_string(),
    })
}

enum FollowupStep {
    Nudge,
    Fail(String),
    Leave,
    Review,
    Settle,
}

fn followup_step(
    was_completed: bool,
    report: &Report,
    reviews_enabled: bool,
    wants_review: bool,
    nudge_available: bool,
) -> FollowupStep {
    if let Some(error) = incomplete_report(report) {
        if was_completed {
            return FollowupStep::Leave;
        }
        if report.status.is_none() && nudge_available {
            return FollowupStep::Nudge;
        }
        return FollowupStep::Fail(error);
    }
    if reviews_enabled && wants_review {
        FollowupStep::Review
    } else {
        FollowupStep::Settle
    }
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
        "You are {name}, a resident of the Loka. {persona}{addendum}\n\nRight now you are pairing live with {owner} in their own copy of {folder} on their Mac, like a coding assistant in their editor. This is not a quest: there is no worktree, no review and no report block. Answer questions directly and make the edits they ask for in place. Keep replies short and to the point. Do not create branches, commit, push or open pull requests unless they ask you to. When they or their rules allow force-pushing, rebasing or rewriting history, do it (prefer --force-with-lease); that is the approval. The files are theirs and may have uncommitted work, so never discard or reset changes you did not make.{rules}",
        name = agent.name,
        rules = if rules.is_empty() { String::new() } else { format!("\n\n{rules}") }
    )
}

fn tail(text: &str, max: usize) -> String {
    let length = text.chars().count();
    text.chars().skip(length.saturating_sub(max)).collect()
}

fn review_value(output: &str, final_attempt: bool) -> Value {
    match prompts::readable_verdict(output) {
        Some(verdict) => json!({ "verdict": verdict.verdict, "findings": verdict.findings, "summary": tail(output, 800) }),
        None if prompts::parse_verdict(output).is_some_and(|v| v.verdict == "pick") => {
            json!({ "verdict": "pick", "summary": tail(output, 800) })
        }
        None if final_attempt => json!({ "verdict": "unparsed", "summary": tail(output, 800), "note": prompts::UNREADABLE_VERDICT_NOTE }),
        None => json!({ "verdict": "unparsed", "summary": tail(output, 800) }),
    }
}

#[cfg(test)]
mod tests {
    use super::{followup_step, incomplete_report, review_value, FollowupStep};
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
                followup_step(false, &report, true, false, true),
                FollowupStep::Nudge
            ));
            assert!(matches!(
                followup_step(false, &report, true, true, true),
                FollowupStep::Nudge
            ));
            let step = followup_step(false, &report, true, false, false);
            let FollowupStep::Fail(error) = step else {
                panic!("empty report was not failed");
            };
            assert_eq!(error, "Task did not provide a valid completion report");
            assert!(matches!(
                followup_step(true, &report, true, false, true),
                FollowupStep::Leave
            ));
        }
        let done = Report {
            status: Some("done".to_string()),
            ..Report::default()
        };
        assert!(incomplete_report(&done).is_none());
        assert!(matches!(
            followup_step(false, &done, true, false, false),
            FollowupStep::Settle
        ));
        assert!(matches!(
            followup_step(false, &done, true, true, false),
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
        assert!(matches!(followup_step(false, &report, true, false, true), FollowupStep::Settle));
        assert!(matches!(followup_step(true, &report, true, true, false), FollowupStep::Review));
    }

    #[test]
    fn unreadable_review_is_noted_not_failed() {
        let changes = review_value("<!-- verdict {\"verdict\":\"changes\",\"findings\":[{\"issue\":\"x\"}]} verdict -->", true);
        assert_eq!(changes["verdict"], "changes");
        assert_eq!(changes["findings"].as_array().map(Vec::len), Some(1));
        assert!(changes.get("note").is_none());
        let unreadable = review_value("I looked and it seems fine", true);
        assert_eq!(unreadable["verdict"], "unparsed");
        assert_eq!(unreadable["note"], crate::prompts::UNREADABLE_VERDICT_NOTE);
        assert!(review_value("I looked and it seems fine", false).get("note").is_none());
    }
}
