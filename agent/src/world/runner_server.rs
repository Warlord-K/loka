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
        ApplyCompletion, ClaimedRun, LeaseContext, RunnerCompletion, RunnerEvent, RunnerFile, RunnerImage,
        RunnerJob, RunnerPublish, RunnerSkill, RunnerStore, RunnerUsage, RunnerWorkspace,
    },
    world::{
        clarification_continuation_task, AgentSession, FollowupAdvance, InboxItem, NewEvent, NewMessage, NewRun, NewSession,
        ReviewerChoice, SessionRun, TaskResume, WorldAgent, WorldStore,
    },
};
use serde_json::{json, Map, Value};
use std::str::FromStr;
use uuid::Uuid;

use crate::prompts::{self, Action, Report};
use crate::quests::{finish_error, finish_step, followup_step, incomplete_report, salvages, Finish, FollowupStep, Started};
use loka_shared::context::SentContext;
use loka_shared::pr_actions;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FollowupPrSync<'a> {
    sync: loka_shared::runners::PrSync,
    merged_pr_url: Option<&'a str>,
}

impl<'a> FollowupPrSync<'a> {
    fn observed(sync: loka_shared::runners::PrSync) -> Self {
        Self { sync, merged_pr_url: None }
    }
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
            return self.withdraw_queued_run(run_id, by).await;
        };
        if on_runner {
            let error = format!("Stopped by {by}. Do not restart this work unless they ask.");
            if self.store.stop_runner_run(run_id, &error).await? {
                self.handle_expired(&[run_id]).await?;
            }
        }
        Ok(true)
    }

    pub async fn withdraw_queued_run(&self, run_id: Uuid, by: &str) -> Result<bool> {
        if !self.store.withdraw_queued(run_id, by).await? {
            return Ok(false);
        }
        if let Err(error) = self.carry_on_withdrawn(Some(run_id)).await {
            tracing::warn!(run = %run_id, %error, "Could not carry on the work a withdrawn follow-up was holding back");
        }
        Ok(true)
    }

    pub async fn carry_on_withdrawn(&self, withdrawn: Option<Uuid>) -> Result<()> {
        for withdrawn in self.store.carry_on_candidates(withdrawn, true).await? {
            let Some(carry) = self.store.claim_carry_on(withdrawn, true).await? else {
                continue;
            };
            let prior = carry.prior();
            let Some(run) = self.store.get_run(prior).await? else {
                carry.complete().await?;
                continue;
            };
            let (Some(session), Some(agent)) = (self.store.get_session(run.session_id).await?, self.store.get_agent(run.agent_id).await?) else {
                carry.complete().await?;
                continue;
            };
            let output = run.output.clone().unwrap_or_default();
            match self
                .after_followup(
                    &agent,
                    &session,
                    prior,
                    &output,
                    None,
                    FollowupPrSync::observed(loka_shared::runners::PrSync::Changed),
                )
                .await
            {
                Ok(()) => carry.complete().await?,
                Err(error) => {
                    carry.release().await?;
                    tracing::warn!(run = %prior, %error, "Could not carry on the work a withdrawn follow-up was holding back");
                }
            }
        }
        Ok(())
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
        let mut prompt = prompts::turn(items, &tasks);
        if loka_shared::quests::owner_spoke(items) {
            let note = loka_shared::quests::waiting_plans_note(&self.store.waiting_plans(agent.id).await.unwrap_or_default());
            if !note.is_empty() {
                prompt = format!("{prompt}\n\n{note}");
            }
            if prompts::feature_launch_active(loka_shared::features::LaunchScope::Main) {
                let recent = prompts::recent_feature_exposures(&self.store, agent).await;
                let launch = prompts::main_feature_launch(items, &recent, &self.owner_name);
                if !launch.is_empty() {
                    prompt = format!("{prompt}\n\n{launch}");
                }
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
        Ok(run)
    }

    async fn talk_rules_text(&self, agent: &WorldAgent) -> Result<String> {
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
        Ok([
            prompts::rules(&rules, "talk", &self.owner_name),
            prompts::before_big_work(self.store.agent_before_big_work(agent).await?).to_string(),
            prompts::teams(&teams, agent.id, "talk"),
            skills_index,
        ]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
    }

    async fn main_instructions(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        resumes: bool,
        continues: bool,
    ) -> Result<(SentContext, Option<String>, Option<String>)> {
        let system = prompts::persona(agent, &self.owner_name);
        let rules_text = self.talk_rules_text(agent).await?;
        let mut sent = SentContext::carried(session.context_sent.as_ref(), continues);
        let update = prompts::instructions_update(&mut sent, resumes, &system, &rules_text);
        let fresh = match update {
            Some(_) => None,
            None => prompts::instructions_update(&mut SentContext::default(), false, &system, &rules_text),
        };
        Ok((sent, update, fresh))
    }

    pub async fn build_job(&self, mut claimed: ClaimedRun, mcp_token: String) -> Result<RunnerJob> {
        let prepared = self.store.prepare_request(claimed.run_id).await.unwrap_or_else(|error| {
            tracing::warn!(run = %claimed.run_id, error = %loka_shared::cli_errors::chain(&error), "Could not check the context budget, sending the request as it is");
            loka_shared::context_budget::Prepared::Send
        });
        if let loka_shared::context_budget::Prepared::Checkpoint { prompt } = &prepared {
            claimed.prompt = prompt.clone();
            claimed.cli_session_id = None;
        }
        if matches!(claimed.kind.as_str(), "task" | "followup") {
            crate::quests::append_single_prework_policy(&self.store, claimed.session_id, claimed.run_id, &mut claimed.prompt).await?;
        }
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
                if matches!(claimed.kind.as_str(), "turn" | "task" | "followup") {
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
        let room_answer = loka_shared::world::room_answer_aside(session.asked_by.as_ref());
        if agent.owner_user_id != Some(claimed.owner_user_id) {
            bail!("runner owner no longer owns this agent");
        }
        if room_answer && !isolates_room_answers(&runner.capabilities) {
            self.store.deliver_question_answer(&agent, &session, "", false).await?;
            bail!("this device app is too old to answer safely in a quest room; update Loka on that device, then ask again");
        }
        let skills = if room_answer { Vec::new() } else { skills };
        if runner.owner_user_id != claimed.owner_user_id {
            bail!("runs only go to their owner's own device");
        }
        let pr_action = if claimed.kind == pr_actions::KIND {
            let schedule = self
                .store
                .pr_action_of_run(claimed.run_id)
                .await?
                .ok_or_else(|| anyhow!("the scheduled action was deleted before it ran"))?;
            Some(super::quests::pr_action_job(&self.store, &schedule).await?)
        } else {
            None
        };
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
        let continues = loka_shared::context::resumes_same_session(resume, fork, rewinding);
        let main_turn = claimed.kind == "turn" && claimed.prompt != prompts::COMPACT_PROMPT && claimed.prompt != prompts::RESET_PROMPT;
        let (context_sent, instructions, fresh_instructions) = if main_turn {
            let (sent, update, fresh) = self.main_instructions(&agent, &session, resume, continues).await?;
            if let Some(hash) = sent.hash(loka_shared::context::INSTRUCTIONS_BLOCK) {
                self.runners.set_run_rules_hash(claimed.run_id, hash).await?;
            }
            (sent, update, fresh)
        } else {
            (SentContext::carried(session.context_sent.as_ref(), continues), None, None)
        };
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
                let follows_recorded_pr = claimed.kind == "followup"
                    && task
                        .as_ref()
                        .is_some_and(|task| task.pr_url.is_some());
                if claimed.kind == "publish" || follows_recorded_pr {
                    let task = task.as_ref().ok_or_else(|| anyhow!("publish run has no task"))?;
                    let mut publish = self.publish_payload(task, &session).await?;
                    if claimed.kind == "followup" {
                        let tracked = task
                            .pr_url
                            .as_deref()
                            .and_then(loka_shared::git_names::TrackedPr::parse)
                            .ok_or_else(|| anyhow!("follow-up task has an invalid pull request"))?;
                        publish.repo = tracked.repo().to_string();
                        publish.pr_number = Some(i64::from(tracked.number()));
                    }
                    Ok(Some(publish))
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
        let mcp_servers = if claimed.kind == "aside" || claimed.kind == pr_actions::KIND {
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
            "publish" | "action" => (true, 1),
            _ => (false, 200),
        };
        let fresh_context = matches!(prepared, loka_shared::context_budget::Prepared::Checkpoint { .. });
        let previous_run_id = if rewinding || fresh_context { None } else { previous_run_id };
        let approval = if loka_shared::quests::is_best_of_prework(session.asked_by.as_ref()) {
            "read_only".to_string()
        } else if claimed.kind == "aside" {
            "full".to_string()
        } else {
            agent.approval_mode.clone()
        };
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
            prompt: [instructions, Some(claimed.prompt), skipped_note].into_iter().flatten().collect::<Vec<_>>().join("\n\n"),
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
            room_answer,
            omp_models,
            omp_secrets,
            command_rules,
            approved_actions,
            owner_id: agent.owner_user_id,
            keys_version,
            metered: self.store.metered(agent.id).await?,
            setup_scan,
            context_sent,
            fresh_instructions,
            task_id: claimed.task_id,
            pr_action,
            compact_first: prepared.compact_first(),
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
            _ if lease.kind == "publish" || lease.kind == pr_actions::KIND => None,
            _ if compaction => Some(0),
            measured => Some(loka_shared::context::measured(
                &session.backend,
                measured,
                completion.usage.input_tokens,
                completion.usage.cached_input_tokens,
            )),
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
        if lease.kind != "publish" {
            self.record_sent_context(lease, completion).await?;
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
                    if !self.compact_if_large(&agent, &session, lease.run_id, completion, context_tokens.unwrap_or_default()).await? {
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
                    let task = match session.task_id {
                        Some(task_id) => self.db.get_task(task_id).await?,
                        None => None,
                    };
                    self.after_followup(
                        &agent,
                        &session,
                        lease.run_id,
                        &output,
                        completion.work_summary.as_deref(),
                        verified_followup_sync(
                            completion,
                            task.as_ref().and_then(|task| task.pr_url.as_deref()),
                            task.as_ref().and_then(|task| task.repo.as_deref()),
                        ),
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
                "action" => self.after_pr_action(&agent, lease.run_id, &output).await?,
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
        let server = crate::mcp::Server::new(self.store.clone(), self.db.clone(), caller).in_quest(quest, session).in_quick(quick).in_aside(lease.kind == "aside", Some(lease.session_id));
        let server = if read_only { server.read_only() } else { server };
        let asking = match quest {
            Some(task) => self.store.room_asking(task).await?.is_some_and(|room| room.mode().asks()),
            None => false,
        };
        let server = server.asking(asking);
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

    async fn record_sent_context(&self, lease: &LeaseContext, completion: &RunnerCompletion) -> Result<()> {
        let mut sent = completion.context_sent.clone().unwrap_or_default();
        if let Some(hash) = self.runners.run_rules_hash(lease.run_id).await? {
            sent.set(loka_shared::context::INSTRUCTIONS_BLOCK, &hash);
        }
        self.store.set_sent_context(lease.session_id, &sent).await
    }

    async fn compact_if_large(
        &self,
        agent: &WorldAgent,
        main: &AgentSession,
        run_id: Uuid,
        completion: &RunnerCompletion,
        used: i64,
    ) -> Result<bool> {
        let Some(limit) = prompts::compaction_due(&main.backend, &main.model, used) else { return Ok(false) };
        if completion.session_id.is_none() {
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
        self.store.forget_sent_context(main.id).await?;
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
        let linked_items = self.run_inbox_items(run_id).await?;
        let items: Vec<_> = linked_items.iter().filter(|item| item.status == "processing").cloned().collect();
        let output = if !linked_items.is_empty() && items.is_empty() { "" } else { output };
        let tasks = self.referenced_tasks(&items).await;
        let thread = self.store.reply_thread(agent, &items).await?;
        let root = loka_shared::replies::reply_root(&items);
        let (text, actions) = prompts::split_actions(output);
        let mut problems = prompts::action_problems(output);
        let mut replied = false;
        let mut started = HashSet::new();
        let mut retry_continuations = HashSet::new();
        let mut attempted_continuations = HashSet::new();
        let clarifying = prompts::clarification_targets(&actions, &items, &tasks);
        let single_start = actions.iter().filter(|action| matches!(action, Action::StartTask { .. })).count() == 1;
        for action in actions {
            match action {
                Action::Reply { text, next } if !text.trim().is_empty() => {
                    self.reply(agent, main, run_id, &text, &next, thread, root).await?;
                    replied = true;
                }
                Action::StartTask { task_id, brief, clarification_source, review, reviewer, time_limit_minutes, milestones, quest } => {
                    let task_id = match crate::quests::start_target(&self.store, agent, &task_id).await {
                        Ok(id) => id,
                        Err(problem) => {
                            problems.push(problem);
                            continue;
                        }
                    };
                    let choice = reviewer.as_ref().and_then(ReviewerChoice::from_value);
                    if !clarifying.contains(&task_id) && !started.contains(&task_id) {
                        let continuation = match crate::quests::clarification_for_start(
                            &linked_items,
                            task_id,
                            clarification_source.as_deref(),
                            single_start,
                        ) {
                            Ok(continuation) => continuation,
                            Err(problem) => {
                                problems.push(problem);
                                continue;
                            }
                        };
                        if continuation.is_some_and(|item| item.status != "processing") {
                            continue;
                        }
                        if let Some(item) = continuation.filter(|item| clarification_continuation_task(item).is_some()) {
                            if !self.store.clarification_continuation_open(item).await? {
                                continue;
                            }
                        }
                        if let Some(item) = continuation {
                            attempted_continuations.insert(item.id);
                        }
                        if let Err(error) = crate::quests::apply_choice(&self.store, task_id, &quest).await {
                            if let Some(item) = continuation {
                                retry_continuations.insert(item.id);
                            }
                            problems.push(format!("Could not set the quest options on task {task_id}: {error}"));
                            continue;
                        }
                        crate::quests::seed_milestones(&self.store, agent.id, task_id, &milestones).await;
                        let start_brief = match continuation {
                            Some(item) => crate::quests::StartBrief::Clarification(item),
                            None => crate::quests::StartBrief::Ordinary(brief.as_deref()),
                        };
                        match self
                            .start_task(
                                agent,
                                main,
                                task_id,
                                start_brief,
                                review.unwrap_or(false) || choice.is_some(),
                                choice.as_ref(),
                            )
                            .await
                        {
                            Err(error) => {
                                if let Some(item) = continuation {
                                    retry_continuations.insert(item.id);
                                }
                                problems.push(format!("Could not start task {task_id}: {error}"));
                            }
                            Ok(Some(outcome)) => {
                                tracing::info!(agent = %agent.slug, task = %task_id, resumed = outcome.resumed, session = %outcome.session_id, run = %outcome.run_id, "Start queued a run");
                                started.insert(task_id);
                                if let Some(minutes) = time_limit_minutes {
                                    if let Err(error) = self.store.set_task_time_limit(task_id, Some(minutes)).await {
                                        problems.push(format!("Could not set the time limit on task {task_id}: {error}"));
                                    }
                                }
                            }
                            Ok(None) => {}
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
                        (thread, root),
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
                    let clarify_request = clarify_request
                        .then(|| prompts::clarification_origin(&items, &tasks, clarify_source.as_deref()))
                        .flatten();
                    self.attention(
                        agent,
                        main,
                        run_id,
                        &text,
                        AttentionCard {
                            choices,
                            drafts: Vec::new(),
                            clarify_request,
                        },
                        (thread, root),
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
                        (thread, root),
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
            if attempted_continuations.contains(&item.id) {
                continue;
            }
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
                || clarifying.contains(&task_id)
                || (replied && !resume)
                || self.store.quest_has_parts(task_id).await.unwrap_or(false)
                || crate::quests::assigned_elsewhere(&self.store, task_id, agent.id).await
            {
                continue;
            }
            if let Some(continuation) = clarification_continuation_task(item) {
                if continuation != task_id || !self.store.clarification_continuation_open(item).await? {
                    continue;
                }
            }
            let note = item.payload.get("brief").and_then(Value::as_str).filter(|note| !note.trim().is_empty());
            let continuation = clarification_continuation_task(item).map(|_| item);
            let start_brief = match continuation {
                Some(item) => crate::quests::StartBrief::Clarification(item),
                None => crate::quests::StartBrief::Ordinary(note.or((!text.trim().is_empty()).then_some(text.as_str()))),
            };
            match self.start_task(agent, main, task_id, start_brief, false, None).await {
                Ok(_) => {}
                Err(error) => {
                    if let Some(item) = continuation {
                        retry_continuations.insert(item.id);
                    }
                    problems.push(format!("Could not start task {task_id}: {error}"));
                }
            }
        }
        if !retry_continuations.is_empty() {
            self.store
                .requeue_clarification_starts(&retry_continuations.into_iter().collect::<Vec<_>>())
                .await?;
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
        start_brief: crate::quests::StartBrief<'_>,
        review: bool,
        reviewer: Option<&ReviewerChoice>,
    ) -> Result<Option<Started>> {
        let resolved_brief = start_brief.text();
        let brief = resolved_brief.as_deref();
        let continuation = start_brief.continuation();
        let public_brief = if continuation.is_some() { None } else { brief };
        let task = self
            .db
            .get_task(task_id)
            .await?
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;
        if task.assigned_agent_id != Some(agent.id) {
            bail!("task is not assigned to this agent")
        }
        let resume = match continuation {
            Some(item) => self
                .store
                .resume_clarification_task_session(agent.id, task.id, &agent.name, brief, item)
                .await?,
            None => self.store.resume_task_session(agent.id, task.id, &agent.name, brief).await?,
        };
        if matches!(&resume, TaskResume::Parked) {
            return Ok(None);
        }
        if let Some(problem) = resume.problem() {
            bail!(problem)
        }
        if let TaskResume::Resumed { session_id, run_id } = resume {
            if continuation.is_some() {
            } else {
                self.db.update_status(task.id, "in_progress").await?;
            }
            self.store.announce_resumed(agent.id, task.id, session_id, run_id, public_brief).await?;
            return Ok(Some(Started { resumed: true, session_id, run_id }));
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
        let opening_attempts = if options.attempts > 1 { 1 } else { options.attempts.max(1) };
        let dirs: Vec<String> = (1..=opening_attempts).map(|i| crate::quests::attempt_dir(&workspace, i)).collect();
        let rules = self.work_rules(agent).await?;
        let opened = crate::quests::open_sessions(
            &self.store,
            crate::quests::QuestStart { agent, main, task: &task, brief, review, reviewer, rules: &rules, options: &options, world_tools: true },
            &dirs,
            continuation,
        )
        .await?;
        let (session, run) = opened.first().ok_or_else(|| anyhow!("no task session opened"))?;
        if continuation.is_some() {
        } else {
            self.db.update_status(task.id, "in_progress").await?;
        }
        self.store
            .emit(
                NewEvent::new("task.started")
                    .agent(agent.id)
                    .session(session.id)
                    .run(run.id)
                    .task(Some(task.id))
                    .payload(crate::quests::started_payload(&task, &options, options.attempts.max(1) as usize, run.id)),
                None,
            )
            .await?;
        if opened.iter().any(|(session, _)| session.racing()) {
            crate::quests::label_attempts(&self.store, &task, &opened).await?;
        }
        self.store
            .post_quest_started(agent.id, task.id, session.id, &task.title, public_brief.filter(|b| !b.trim().is_empty()))
            .await?;
        Ok(Some(Started { resumed: false, session_id: session.id, run_id: run.id }))
    }

    async fn start_best_of_attempts(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run: &SessionRun,
        output: &str,
    ) -> Result<()> {
        let Some(current) = self.store.get_session(session.id).await? else {
            return Ok(());
        };
        if current.status != "active" || !loka_shared::quests::is_best_of_prework(current.asked_by.as_ref()) {
            return Ok(());
        }
        let task_id = current.task_id.ok_or_else(|| anyhow!("prework session has no quest"))?;
        let task = self.db.get_task(task_id).await?.ok_or_else(|| anyhow!("task {task_id} not found"))?;
        if task.status != "in_progress" || task.assigned_agent_id != Some(agent.id) {
            return Ok(());
        }
        let current_options = self.store.quest_options(task_id).await?;
        let options = crate::quests::best_of_options(&current, &current_options);
        let main_id = current.parent_session_id.ok_or_else(|| anyhow!("prework session has no main session"))?;
        let main = self.store.get_session(main_id).await?.ok_or_else(|| anyhow!("main session {main_id} not found"))?;
        let workspace = current.working_dir.clone().ok_or_else(|| anyhow!("prework session has no workspace"))?;
        let brief = current.asked_by.as_ref().and_then(|value| value.get("brief")).and_then(Value::as_str);
        let dirs: Vec<String> = (1..=options.attempts.max(1)).map(|attempt| crate::quests::attempt_dir(&workspace, attempt)).collect();
        let rules = self.work_rules(agent).await?;
        let opened = crate::quests::fan_out_sessions(
            &self.store,
            crate::quests::QuestStart {
                agent,
                main: &main,
                task: &task,
                brief,
                review: false,
                reviewer: None,
                rules: &rules,
                options: &options,
                world_tools: true,
            },
            &dirs,
            &current,
            run,
            output,
        )
        .await?;
        crate::quests::label_attempts(&self.store, &task, &opened).await
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
        match crate::quests::prework_step(&self.store, agent, session, &run, output).await? {
            crate::quests::PreworkStep::Continue => {}
            crate::quests::PreworkStep::Handled => return Ok(()),
            crate::quests::PreworkStep::FanOut => return self.start_best_of_attempts(agent, session, &run, output).await,
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
        let (backend, model) = self.store.reviewer_for(agent, task_session).await?;
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
        let task = self.db.get_task(task_id).await?;
        let repo = task
            .as_ref()
            .and_then(|task| task.repo.as_deref())
            .and_then(|repo| loka_shared::git_names::github_repo(repo).ok());
        let tracked = task
            .as_ref()
            .and_then(|task| task.pr_url.as_deref())
            .and_then(loka_shared::git_names::TrackedPr::parse);
        let pr = completion
            .pr_url
            .as_deref()
            .zip(completion.pr_number)
            .and_then(|(url, number)| {
                loka_shared::git_names::TrackedPr::parse(url)
                    .filter(|reported| i64::from(reported.number()) == number)
            });
        if completion.pr_url.is_some() && pr.is_none() {
            bail!("publish completion reported an invalid pull request")
        }
        let checked = completion
            .checked_pr_url
            .as_deref()
            .and_then(loka_shared::git_names::TrackedPr::parse);
        let replacement = completion.publish_pr_state.as_deref() == Some("replaced");
        if replacement {
            let Some((old, reported)) = checked.zip(pr) else {
                bail!("replacement publish completion must include the old and new pull requests")
            };
            if !old.repo().eq_ignore_ascii_case(reported.repo()) || old.same_identity(reported) {
                bail!("replacement publish completion does not replace the recorded pull request")
            }
            if !tracked.is_some_and(|tracked| tracked.same_identity(old) || tracked.same_identity(reported)) {
                return Ok(());
            }
        } else if completion.checked_pr_url.is_some()
            && !checked.zip(pr).is_some_and(|(checked, reported)| checked.same_identity(reported))
        {
            bail!("publish completion checked a different pull request")
        }
        let matches_recorded = tracked.zip(pr).is_some_and(|(tracked, reported)| tracked.same_identity(reported));
        let matches_workspace = pr
            .zip(repo.as_deref())
            .is_some_and(|(reported, repo)| reported.repo().eq_ignore_ascii_case(repo));
        if pr.is_some() && !matches_recorded && !matches_workspace {
            bail!("publish completion reported a pull request outside this quest's repository")
        }
        if !replacement && tracked.is_some() && pr.is_some() && !matches_recorded {
            self.store.set_session_status(session.id, "completed", None).await?;
            return Ok(());
        }
        let terminal = matches!(completion.publish_pr_state.as_deref(), Some("merged" | "closed"));
        if terminal && pr.is_none() {
            bail!("terminal publish completion has no pull request")
        }
        let observed_state = matches!(completion.publish_pr_state.as_deref(), Some("open" | "merged" | "closed"));
        if observed_state
            && matches_recorded
            && !matches_workspace
            && checked.is_none()
        {
            bail!("publish completion did not prove the tracked pull request")
        }
        if completion.publish_pr_checked_at.is_some()
            && (!matches!(completion.publish_pr_state.as_deref(), Some("open" | "closed")) || pr.is_none())
        {
            bail!("pull request check time requires an open or closed pull request")
        }
        if replacement {
            let old = checked.expect("validated replacement");
            let reported = pr.expect("validated replacement");
            let replaced = if tracked.is_some_and(|tracked| tracked.same_identity(reported)) {
                true
            } else {
                self.db
                    .replace_current_pr(task_id, old.url(), old.number(), reported.url(), reported.number())
                    .await?
            };
            if !replaced {
                let current_url = self.db.get_task(task_id).await?.and_then(|task| task.pr_url);
                let current = current_url
                    .as_deref()
                    .and_then(loka_shared::git_names::TrackedPr::parse)
                    .is_some_and(|current| current.same_identity(reported));
                if !current {
                    return Ok(());
                }
            }
        } else if let Some(reported) = pr.filter(|_| tracked.is_none()) {
            self.db.set_pr(task_id, reported.url(), reported.number()).await?;
        }
        let recorded_task = self.db.get_task(task_id).await?;
        let recorded_pr_url = recorded_task.as_ref().and_then(|task| task.pr_url.clone());
        let recorded_pr_number = recorded_pr_url
            .as_deref()
            .and_then(loka_shared::git_names::TrackedPr::parse)
            .map(|recorded| i64::from(recorded.number()));
        if pr.is_some_and(|reported| {
            !recorded_pr_url
                .as_deref()
                .and_then(loka_shared::git_names::TrackedPr::parse)
                .is_some_and(|recorded| recorded.same_identity(reported))
        }) {
            self.store.set_session_status(session.id, "completed", None).await?;
            return Ok(());
        }
        if let Some(reported) = pr {
            if let Some(head) = completion.published_head.as_deref() {
                self.store.record_published_head(task_id, reported.url(), head).await?;
            }
        }
        if completion.publish_pr_state.as_deref() == Some("open") {
            if let (Some(reported), Some(observed_at)) = (pr, completion.publish_pr_checked_at.as_ref()) {
                let milestones = MilestoneStore::new(self.store.pool().clone());
                milestones.rearm_open_pr(task_id, reported.url(), observed_at.to_owned()).await?;
            }
        }
        match completion.publish_pr_state.as_deref() {
            Some("merged") => {
                let Some(reported) = pr else {
                    bail!("merged publish completion has no recorded pull request")
                };
                self.settle_merged(agent, session, task_id, reported.url()).await
            }
            Some("closed") => {
                let Some(reported) = pr else {
                    bail!("closed publish completion has no recorded pull request")
                };
                let settled = match completion.publish_pr_checked_at.as_ref() {
                    Some(observed_at) => self.store.settle_pr_observed(task_id, reported.url(), false, observed_at.to_owned()).await?,
                    None => self.store.settle_pr(task_id, reported.url(), false).await?,
                };
                if !settled {
                    return Ok(());
                }
                self.finalize_task(
                    agent,
                    session,
                    None,
                    Some(loka_shared::world::PR_CLOSED_ERROR.into()),
                    Some(reported.url().to_string()),
                    Some(i64::from(reported.number())),
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
                self.finalize_task(
                    agent,
                    session,
                    verdict,
                    error,
                    recorded_pr_url,
                    recorded_pr_number,
                )
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
            if self.store.quest_merged_pr(task_id).await?.is_some() {
                return self.queue_publish(agent, task_session, run_id).await;
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

    async fn after_pr_action(&self, agent: &WorldAgent, run_id: Uuid, output: &str) -> Result<()> {
        let outcome = pr_actions::Outcome::from_output(output)
            .unwrap_or_else(|| pr_actions::Outcome::failed("the device sent back an answer Loka could not read; update the Loka app on it"));
        let Some(spec) = self.store.settle_pr_action(run_id, &outcome).await? else {
            return Ok(());
        };
        let Some(task_id) = self.store.get_run(run_id).await?.and_then(|run| run.task_id).filter(|_| outcome.status == pr_actions::OutcomeStatus::Ran) else {
            return Ok(());
        };
        let task = self.db.get_task(task_id).await?;
        let merged_url = task
            .as_ref()
            .and_then(|task| pr_actions::merged_pr_url(&spec, &outcome, task.pr_url.as_deref(), task.pr_number));
        match (merged_url, self.store.newest_task_session(task_id).await?) {
            (Some(url), Some(session)) => self.settle_merged(agent, &session, task_id, url).await,
            _ => {
                MilestoneStore::new(self.store.pool().clone()).check_now(task_id).await?;
                Ok(())
            }
        }
    }

    async fn settle_merged(
        &self,
        _agent: &WorldAgent,
        session: &AgentSession,
        task_id: Uuid,
        pr_url: &str,
    ) -> Result<()> {
        debug_assert_eq!(session.task_id, Some(task_id));
        crate::quests::reconcile_quest(&self.store, &self.db, session, crate::quests::QuestEvidence::Merged(pr_url)).await?;
        Ok(())
    }

    async fn after_followup(
        &self,
        agent: &WorldAgent,
        session: &AgentSession,
        run_id: Uuid,
        output: &str,
        work: Option<&str>,
        verified_pr: FollowupPrSync<'_>,
    ) -> Result<()> {
        let FollowupPrSync { sync: observed_sync, merged_pr_url } = verified_pr;
        let run = self.store.get_run(run_id).await?.ok_or_else(|| anyhow!("completed follow-up disappeared"))?;
        let settled_merge = if let ("task", loka_shared::runners::PrSync::Merged, Some(task_id), Some(url)) =
            (session.role.as_str(), observed_sync, session.task_id, merged_pr_url)
        {
            self.store.settle_merged_followup(task_id, session.id, url).await?
        } else {
            false
        };
        let pr_sync = if observed_sync == loka_shared::runners::PrSync::Merged && !settled_merge {
            loka_shared::runners::PrSync::Changed
        } else {
            observed_sync
        };
        if !settled_merge {
            match crate::quests::prework_step(&self.store, agent, session, &run, output).await? {
                crate::quests::PreworkStep::Continue => {}
                crate::quests::PreworkStep::Handled => return Ok(()),
                crate::quests::PreworkStep::FanOut => return self.start_best_of_attempts(agent, session, &run, output).await,
            }
        }
        if let Some(report) = prompts::parse_report(output) {
            self.store
                .set_session_summary(session.id, &serde_json::to_string(&report)?)
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
        if settled_merge {
            return Ok(());
        }
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
            let nudge_available = if !was_completed
                && report.status.is_none()
                && pr_sync != loka_shared::runners::PrSync::Merged
            {
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
        pr_number: Option<i64>,
    ) -> Result<()> {
        self.finalize_task_with(agent, session, verdict, error, pr_url, pr_number).await
    }

    async fn finalize_task_with(
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
        let parsed: Report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
        let report: Value = session
            .summary
            .as_deref()
            .and_then(|summary| serde_json::from_str(summary).ok())
            .unwrap_or_else(|| json!({ "summary": session.summary }));
        let diagnosis = self.store.latest_diagnosis(session.id).await?;
        let error = match (&error, &diagnosis) {
            (Some(_), Some(diagnosis)) if parsed.status.is_none() => Some(diagnosis.reason.clone()),
            _ => error,
        };
        let diagnosis = diagnosis.filter(|_| error.is_some());
        let changes_requested = verdict
            .as_ref()
            .and_then(|value| value.get("verdict"))
            .and_then(Value::as_str)
            == Some("changes");
        let reconciled = crate::quests::reconcile_quest(
            &self.store,
            &self.db,
            session,
            crate::quests::QuestEvidence::Completed {
                report: &report,
                parsed: &parsed,
                error: error.as_deref(),
                pr_url: pr_url.as_deref(),
                changes_requested,
                suppress_announcement: finished,
            },
        )
        .await?
        .completed
        .expect("completed quest evidence returns reconciliation");
        let crate::quests::ReconciledQuest { task: quest, status: task_status, watching, announce } = reconciled;
        if !announce {
            return Ok(());
        }
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
        wrap.add_decisions(&mut payload);
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
        if kind == pr_actions::KIND {
            self.store.settle_pr_action(run_id, &pr_actions::Outcome::failed(loka_shared::cli_errors::describe(error))).await?;
            return Ok(());
        }
        if self.is_compaction(run_id, kind).await? {
            tracing::warn!(agent = %agent.slug, %error, "Main session compaction failed");
            self.store.recover_main_overflow(agent.id, error, true).await?;
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
        let started_over = match diagnosis {
            Some(_) => loka_shared::context::StartOver::Kept,
            None => self.store.start_over(run_id, error).await?,
        };
        match started_over {
            loka_shared::context::StartOver::Retried(_) => return Ok(()),
            loka_shared::context::StartOver::Forgotten => {
                self.stop_typing(agent, run_id).await;
                self.runners.requeue_run_inbox_now(run_id, error).await?;
                self.store.set_agent_status(agent.id, "idle", None).await?;
                return Ok(());
            }
            loka_shared::context::StartOver::Kept => {}
        }
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
        let error = &loka_shared::cli_errors::describe_run(error, kind);
        let payload = || {
            let mut payload = loka_shared::cli_errors::failure_payload(session.task_id, error, session.id, kind);
            if let Some(diagnosis) = &diagnosis {
                diagnosis.merge_into(&mut payload);
            }
            payload
        };
        let task_failed = matches!(kind, "task" | "publish")
            || kind == "followup" && session.role == "task" && session.status == "active";
        if !task_failed || session.racing() {
            self.store
                .fail_queued_followups(session.id, &format!("Parent run failed: {error}"))
                .await?;
        }
        if kind == "turn" {
            self.runners.requeue_run_inbox(run_id, error).await?;
            self.store.recover_main_overflow(agent.id, error, false).await?;
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
                .set_agent_status(agent.id, "error", Some(&loka_shared::cli_errors::status_detail_run(error, kind)))
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
            && prompts::failed_nudge_settles(self.store.run_nudge(run_id).await?, error)
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
        let mut stale_task_failure = false;
        if task_failed {
            if let Some(task_id) = session.task_id {
                let failed = self.store.fail_task_session(task_id, session.id, error).await?;
                if failed {
                } else {
                    stale_task_failure = self.store.task_failure_is_stale(task_id, run_id).await?;
                }
            } else {
                self.store.set_session_status(session.id, "failed", Some(error)).await?;
            }
        }
        let mut failure = payload();
        if let (true, Some(fields)) = (stale_task_failure, failure.as_object_mut()) {
            fields.insert("quiet".into(), Value::Bool(true));
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
            failure,
            !stale_task_failure,
        )
        .await?;
        if stale_task_failure {
            return Ok(());
        }
        self.store
            .set_agent_status(agent.id, "error", Some(&loka_shared::cli_errors::status_detail_run(error, kind)))
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
        reply_scope: (Option<Uuid>, Option<Uuid>),
    ) -> Result<()> {
        let (thread, root) = reply_scope;
        let AttentionCard { choices, drafts, clarify_request } = card;
        let clarification = clarify_request.is_some();
        let text = self.store.expand_message_links(&prompts::plain_dashes(text), agent.id).await;
        let new_message = NewMessage {
            agent_id: agent.id,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent.id),
            body: &text,
            session_id: Some(main.id),
            run_id: Some(run_id),
            meta: if drafts.is_empty() {
                json!({ "kind": "ask", "choices": choices.clone(), "clarify_request": clarify_request })
            } else {
                json!({ "kind": "ask", "choices": [], "drafts": drafts })
            },
        };
        if clarification {
            let outcome = self
                .store
                .post_main_request_clarification(&agent.name, &choices, new_message)
                .await?;
            if outcome == loka_shared::world::MainClarificationPost::Refused {
                self.reply(
                    agent,
                    main,
                    run_id,
                    loka_shared::world::CLARIFICATION_POST_REFUSED,
                    &[],
                    thread,
                    root,
                )
                .await?;
            }
            return Ok(());
        }
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND run_id = $2 AND author_kind = 'agent'
                 AND body = $3 AND meta->>'kind' = 'ask' AND meta->>'clarify_request' IS NULL)",
        )
        .bind(agent.id)
        .bind(run_id)
        .bind(&text)
        .fetch_one(self.store.pool())
        .await?;
        if exists {
            return Ok(());
        }
        let message = self.store.insert_message(new_message).await?;
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
        let payloads = self.store.run_answer_payloads(run_id).await?;
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
        let reported_pr_number = report.reported_pr(&repo).map(|(_, number)| i64::from(number));
        let tracked = task
            .pr_url
            .as_deref()
            .and_then(loka_shared::git_names::TrackedPr::parse);
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
            recovery_branch: task.branch.clone(),
            base_branch,
            commit_message,
            pr_title,
            pr_body,
            pr_url: task.pr_url.clone(),
            pr_number: tracked.map(|tracked| i64::from(tracked.number())),
            reported_pr_number,
            stack_onto_pr: parent_quest_pr(&self.db, task).await,
            other_writer: self.store.other_folder_writer(session).await?,
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
    let room_answer = loka_shared::world::room_answer_aside(session.asked_by.as_ref());
    let key = if room_answer {
        loka_shared::world::room_answer_workspace(session.id)
    } else {
        session.working_dir.clone().ok_or_else(|| anyhow!("session has no workspace key"))?
    };
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

fn isolates_room_answers(capabilities: &Value) -> bool {
    capabilities.get("room_answer_isolation").and_then(Value::as_bool) == Some(true)
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

fn verified_followup_sync<'a>(
    completion: &'a RunnerCompletion,
    recorded_url: Option<&str>,
    workspace_repo: Option<&str>,
) -> FollowupPrSync<'a> {
    let sync = loka_shared::runners::PrSync::from_report(completion.publish_pr_state.as_deref());
    if sync != loka_shared::runners::PrSync::Merged {
        return FollowupPrSync::observed(sync);
    }
    let recorded = recorded_url.and_then(loka_shared::git_names::TrackedPr::parse);
    let observed = completion
        .pr_url
        .as_deref()
        .zip(completion.pr_number)
        .and_then(|(url, number)| {
            loka_shared::git_names::TrackedPr::parse(url)
                .filter(|observed| i64::from(observed.number()) == number)
        });
    let proof = completion
        .checked_pr_url
        .as_deref()
        .and_then(loka_shared::git_names::TrackedPr::parse);
    let matched = recorded
        .zip(observed)
        .is_some_and(|(recorded, observed)| recorded.same_identity(observed));
    let proof_needed = recorded.is_some_and(|recorded| {
        workspace_repo.is_none_or(|repo| !recorded.repo().eq_ignore_ascii_case(repo))
    });
    let proof_matches = proof
        .zip(recorded)
        .is_some_and(|(proof, recorded)| proof.same_identity(recorded));
    let proven = proof_matches || (completion.checked_pr_url.is_none() && !proof_needed);
    if matched && proven {
        FollowupPrSync { sync, merged_pr_url: completion.pr_url.as_deref() }
    } else {
        FollowupPrSync::observed(loka_shared::runners::PrSync::Changed)
    }
}

pub async fn parent_quest_pr(db: &Database, task: &AgentTask) -> Option<i64> {
    let parent = db.get_task(task.parent_task_id?).await.ok()??;
    let tracked = parent
        .pr_url
        .as_deref()
        .and_then(loka_shared::git_names::TrackedPr::parse)?;
    if !task
        .repo
        .as_deref()
        .is_some_and(|repo| tracked.repo().eq_ignore_ascii_case(repo))
    {
        return None;
    }
    Some(i64::from(tracked.number()))
}

#[cfg(test)]
mod tests {
    use super::{
        followup_step, incomplete_report, isolates_room_answers, prompts, review_value,
        verified_followup_sync, FollowupPrSync, FollowupStep,
    };
    use loka_shared::runners::PrSync;

    #[test]
    fn room_answers_require_an_exact_runner_isolation_capability() {
        assert!(isolates_room_answers(&serde_json::json!({ "room_answer_isolation": true })));
        assert!(!isolates_room_answers(&serde_json::json!({ "room_answer_isolation": false })));
        assert!(!isolates_room_answers(&serde_json::json!({})));
    }

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
    async fn a_main_session_omp_never_saved_starts_over_with_notes_and_rules_on_the_runner() -> anyhow::Result<()> {
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
            .bind(format!("neon-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Neonardo', 'omp', 'gemini', $2) RETURNING id",
        )
        .bind(format!("neon-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let lost = "01a21d98-c1f1-7311-ba20-3dde5fef60fa";
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, rules_hash) VALUES ($1, 'main', 'omp', 'gemini', $2, 'old') RETURNING id",
        )
        .bind(agent_id)
        .bind(lost)
        .fetch_one(&pool)
        .await?;
        store
            .save_own_notes(agent_id, Some(owner), None, &format!("neon-{tag}-notes"), "notes", "Neonardo paints in neon.")
            .await?;
        let inbox: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts) VALUES ($1, 'message', $2, 'processing', 3) RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({"body": "Start the bake-off quest."}))
        .fetch_one(&pool)
        .await?;
        let error = format!("Error: Session \"{lost}\" not found.\nRun `omp --resume` without an argument to pick from recent sessions, or `omp` to start a new one.: omp exited before it was ready");
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at)
             VALUES ($1, $2, 'turn', 'running', 'hello', NOW()) RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE session_runs SET status = 'failed', error = $2 WHERE id = $1").bind(run_id).bind(&error).execute(&pool).await?;
        sqlx::query("INSERT INTO runner_run_inbox (run_id, inbox_id) VALUES ($1, $2)").bind(run_id).bind(inbox).execute(&pool).await?;
        let main = store.get_session(main_id).await?.expect("main");
        let agent = store.get_agent(agent_id).await?.expect("agent");

        coordinator.fail_run(run_id, "turn", &main, &agent, &error, false).await?;

        let (status, attempts, due): (String, i32, bool) =
            sqlx::query_as("SELECT status, attempts, retry_at <= NOW() FROM agent_inbox WHERE id = $1")
                .bind(inbox)
                .fetch_one(&pool)
                .await?;
        assert_eq!((status.as_str(), attempts, due), ("pending", 2, true), "the message waits for the fresh turn, its attempt given back");
        let agent = store.get_agent(agent_id).await?.expect("agent");
        assert_eq!(agent.status, "idle", "nobody is told about an error Loka fixed by itself");
        let items = store.take_pending_inbox(agent_id, 20).await?;
        let turn = coordinator.queue_turn(&agent, &items).await?;
        assert!(turn.prompt.contains("[memory handoff]") && turn.prompt.contains("Neonardo paints in neon."), "{}", turn.prompt);
        assert!(turn.prompt.contains("could not be found on this device"), "{}", turn.prompt);
        assert!(turn.prompt.contains("Start the bake-off quest."), "{}", turn.prompt);
        assert!(store.get_session(main_id).await?.expect("main").cli_session_id.is_none());

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(&pool).await?;
        sqlx::query("DELETE FROM world_skills WHERE name = $1").bind(format!("neon-{tag}-notes")).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_merge_waiting_on_finished_fires_when_a_requested_review_approves() -> anyhow::Result<()> {
        use loka_shared::pr_actions::{trigger_problem, ActionKind, ActionSpec, Checks, NewAction, Review};
        use loka_shared::runners::{BeginCompletion, CreateRunner, RunnerStore};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            eprintln!("SKIPPED: set STEER_TEST_DATABASE_URL to a migrated database");
            return Ok(());
        };
        let pool = &harness.pool;
        let tag = Uuid::new_v4().simple().to_string();
        let head = "1111111111111111111111111111111111111111";
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("pr-review-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Ferris', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("ferris-review-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let runners = RunnerStore::new(pool.clone());
        let runner = runners.register(owner, &CreateRunner { name: "Review Mac".into(), platform: "macos".into() }).await?.runner.id;
        runners.heartbeat(runner, &serde_json::json!({ "backends": { "claude": true }, "pr_actions": true })).await?;
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(agent).bind(runner).execute(pool).await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_number, pr_url, branch) VALUES ($1, 'Fix the bell', 'review', 'Acme/Shop', 7, 'https://github.com/Acme/Shop/pull/7', 'quest-7') RETURNING id",
        )
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, working_dir, status) VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, 'completed') RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .bind(format!("tasks/{task}/shop"))
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, timeout_secs, finished_at)
             VALUES ($1, $2, $3, 'task', 'completed', 'p', $4, 60, NOW())",
        )
        .bind(task_session)
        .bind(agent)
        .bind(task)
        .bind("done\n<!-- report {\"status\": \"done\", \"summary\": \"x\", \"checks\": [{\"check\": \"clippy\", \"result\": \"pass\"}]} report -->")
        .execute(pool)
        .await?;
        harness.store.record_published_head(task, "https://github.com/Acme/Shop/pull/7", head).await?;
        let rule = harness.store.create_rule(Some(owner), None, "allow", "Merge reviewed PRs in Acme/Shop").await?;
        harness.store.set_rule_repos(rule.id, owner, None, &["Acme/Shop".to_string()]).await?.expect("allow rule");
        let spec = ActionSpec { kind: ActionKind::Merge, repo: "Acme/Shop".into(), number: 7, sha: head.into(), check: None };
        let action = harness
            .store
            .create_pr_action(NewAction { user_id: owner, agent_id: agent, task_id: task, spec: &spec, run_at: None, on_event: Some("task.finished"), wait_days: None, note: None })
            .await?;
        let schedule = |store: &loka_shared::world::WorldStore| {
            let store = store.clone();
            async move { anyhow::Ok(store.list_agent_schedules(agent).await?.into_iter().find(|s| s.id == action.id).expect("listed")) }
        };

        let session = harness.store.get_session(task_session).await?.expect("task session");
        let review = harness.store.request_review(&session, None, None, None, agent).await?;
        assert!(schedule(&harness.store).await?.fired.is_none(), "asking for a review fires nothing yet");
        let claim = runners
            .claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &["review".to_string()])
            .await?
            .expect("the runner claims the requested review");
        assert_eq!(claim.run_id, review.id);
        let completion: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": "Looks right.\n<!-- verdict {\"verdict\": \"approve\", \"summary\": \"fine\", \"findings\": []} verdict -->",
            "session_id": "review-cli",
            "exit_code": 0,
            "error": null,
            "workspace_key": format!("tasks/{task}/shop"),
        }))?;
        assert_eq!(runners.begin_completion(runner, claim.lease_id, &[3u8; 32], &completion).await?, BeginCompletion::Begun);
        let lease = runners.lease_for_completion(runner, claim.lease_id).await?.expect("lease");
        harness.coordinator.complete(&lease, &completion).await?;
        assert!(runners.finish_completion(runner, claim.lease_id).await?);

        let fired = schedule(&harness.store).await?;
        assert_eq!(fired.fired.as_ref().and_then(|f| f.get("type")).and_then(serde_json::Value::as_str), Some("review.finished"));
        assert_eq!(trigger_problem(fired.on_event.as_deref(), fired.fired.as_ref()), None, "a requested review finishing is the wait it asked for");
        let due = harness.store.claim_due_runner_schedules().await?.into_iter().find(|s| s.id == action.id).expect("due at once");
        assert!(harness.store.fire_schedule(&due).await?);
        let claim = runners.claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &["action".to_string()]).await?.expect("action claimed");
        let job = harness.coordinator.build_job(claim, format!("mcp-{}", Uuid::new_v4())).await?;
        let carried = job.pr_action.expect("the job carries the action");
        assert_eq!(carried.trigger_problem, None);
        assert_eq!((carried.facts.review, carried.facts.checks), (Review::Approved, Checks::Passed));
        assert_eq!(carried.facts.published_head.as_deref(), Some(head));

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_pull_request_action_runs_on_a_capable_runner_and_settles_the_quest() -> anyhow::Result<()> {
        use loka_shared::pr_actions::{ActionKind, ActionSpec, Checks, NewAction, Outcome, Review};
        use loka_shared::runners::{BeginCompletion, CreateRunner, RunnerStore};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            eprintln!("SKIPPED: set STEER_TEST_DATABASE_URL to a migrated database");
            return Ok(());
        };
        let pool = &harness.pool;
        let tag = Uuid::new_v4().simple().to_string();
        let head = "1111111111111111111111111111111111111111";
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
            .bind(format!("pr-action-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Ferris', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("ferris-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let runners = RunnerStore::new(pool.clone());
        let runner = runners.register(owner, &CreateRunner { name: "Old Mac".into(), platform: "macos".into() }).await?.runner.id;
        runners.heartbeat(runner, &serde_json::json!({ "backends": { "claude": true } })).await?;
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(agent).bind(runner).execute(pool).await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_number, pr_url, branch, assigned_agent_id)
             VALUES ($1, 'Fix the bell', 'review', 'Acme/Shop', 7, 'https://github.com/Acme/Shop/pull/7', 'quest-7', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .fetch_one(pool)
        .await?;
        sqlx::query("DELETE FROM agent_inbox WHERE agent_id = $1").bind(agent).execute(pool).await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, working_dir, status) VALUES ($1, 'task', 'claude', 'sonnet', $2, $3, 'completed') RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .bind(format!("tasks/{task}/shop"))
        .fetch_one(pool)
        .await?;
        let reviewer: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, parent_session_id) VALUES ($1, 'review', 'claude', 'sonnet', $2, $3) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .bind(task_session)
        .fetch_one(pool)
        .await?;
        for (session, kind, output) in [
            (task_session, "task", "done\n<!-- report {\"status\": \"done\", \"summary\": \"x\", \"checks\": [{\"check\": \"clippy\", \"result\": \"pass\"}]} report -->"),
            (reviewer, "review", "fine\n<!-- verdict {\"verdict\": \"approve\", \"findings\": []} verdict -->"),
        ] {
            sqlx::query(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, timeout_secs, created_at, finished_at)
                 VALUES ($1, $2, $3, $4, 'completed', 'p', $5, 60,
                   CASE WHEN $4 = 'task' THEN NOW() - INTERVAL '2 minutes' ELSE NOW() - INTERVAL '1 minute' END,
                   NOW())",
            )
            .bind(session)
            .bind(agent)
            .bind(task)
            .bind(kind)
            .bind(output)
            .execute(pool)
            .await?;
        }
        harness.store.record_published_head(task, "https://github.com/Acme/Shop/pull/7", head).await?;
        let rule = harness.store.create_rule(Some(owner), None, "allow", "Merge reviewed PRs in Acme/Shop").await?;
        harness.store.set_rule_repos(rule.id, owner, None, &["Acme/Shop".to_string()]).await?.expect("allow rule");

        let spec = ActionSpec { kind: ActionKind::Merge, repo: "Acme/Shop".into(), number: 7, sha: head.into(), check: None };
        let action = harness
            .store
            .create_pr_action(NewAction { user_id: owner, agent_id: agent, task_id: task, spec: &spec, run_at: None, on_event: None, wait_days: None, note: None })
            .await?;
        assert!(harness.store.claim_due_schedules().await?.iter().all(|s| s.id != action.id), "the local worker never claims a device agent's action");
        let due = harness.store.claim_due_runner_schedules().await?.into_iter().find(|s| s.id == action.id).expect("the runner ticker claims it");
        assert!(harness.store.fire_schedule(&due).await?);
        let kinds = ["action".to_string()];
        assert!(runners.claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &kinds).await?.is_none(), "an app without pull request actions never claims one");
        assert!(!runners.has_work(runner).await?, "and is never woken for one");

        runners.heartbeat(runner, &serde_json::json!({ "backends": { "claude": true }, "pr_actions": true })).await?;
        assert!(runners.has_work(runner).await?);
        let claim = runners.claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &kinds).await?.expect("a capable runner claims it");
        let lease_id = claim.lease_id;
        let job = harness.coordinator.build_job(claim, format!("mcp-{}", Uuid::new_v4())).await?;
        assert_eq!(job.kind, "action");
        assert!(job.mcp_servers.as_object().is_some_and(|servers| servers.is_empty()));
        let carried = job.pr_action.clone().expect("the job carries the action");
        assert_eq!(carried.spec, spec);
        assert_eq!(carried.trigger_problem, None);
        assert!(carried.facts.granted);
        assert_eq!((carried.facts.review.clone(), carried.facts.checks.clone()), (Review::Approved, Checks::Passed));
        assert_eq!(carried.facts.published_head.as_deref(), Some(head));
        assert_eq!(carried.facts.quest_branch.as_deref(), Some("quest-7"));

        sqlx::query("UPDATE agent_tasks SET pr_number = 8, pr_url = 'https://github.com/Acme/Shop/pull/8' WHERE id = $1")
            .bind(task)
            .execute(pool)
            .await?;

        let outcome = Outcome { merge_sha: Some("2222222222222222222222222222222222222222".into()), ..Outcome::ran("") };
        let completion: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": serde_json::to_string(&outcome)?,
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": job.workspace.key,
        }))?;
        assert_eq!(runners.begin_completion(runner, lease_id, &[9u8; 32], &completion).await?, BeginCompletion::Begun);
        let lease = runners.lease_for_completion(runner, lease_id).await?.expect("lease");
        harness.coordinator.complete(&lease, &completion).await?;
        assert!(runners.finish_completion(runner, lease_id).await?);

        let count = |kind: &'static str| sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = $2").bind(task).bind(kind).fetch_one(pool);
        let stale_state: (String, Option<String>, Option<i32>, Option<String>) =
            sqlx::query_as("SELECT status, pr_url, pr_number, merged_pr_url FROM agent_tasks WHERE id = $1")
                .bind(task)
                .fetch_one(pool)
                .await?;
        assert_eq!(stale_state, ("review".into(), Some("https://github.com/Acme/Shop/pull/8".into()), Some(8), None));
        assert_eq!((count("action.ran").await?, count("task.merged").await?), (1, 0), "merging the replaced pull request does not settle the current one");
        let recorded = harness.store.list_agent_schedules(agent).await?.into_iter().find(|s| s.id == action.id).expect("kept after the quest closed");
        assert_eq!(recorded.outcome.as_ref().and_then(|o| o.get("merge_sha")).and_then(serde_json::Value::as_str), Some("2222222222222222222222222222222222222222"));
        let spent: f64 = sqlx::query_scalar("SELECT cost_usd FROM session_runs WHERE id = $1").bind(job.run_id).fetch_one(pool).await?;
        assert_eq!(spent, 0.0);

        let spec_b = ActionSpec { number: 8, ..spec.clone() };
        let action_b = harness
            .store
            .create_pr_action(NewAction { user_id: owner, agent_id: agent, task_id: task, spec: &spec_b, run_at: None, on_event: None, wait_days: None, note: None })
            .await?;
        let due = harness.store.claim_due_runner_schedules().await?.into_iter().find(|s| s.id == action_b.id).expect("replacement action due");
        assert!(harness.store.fire_schedule(&due).await?);
        let claim = runners.claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &kinds).await?.expect("replacement action claimed");
        let lease_id = claim.lease_id;
        let job_b = harness.coordinator.build_job(claim, format!("mcp-{}", Uuid::new_v4())).await?;
        assert_eq!(job_b.pr_action.as_ref().map(|job| &job.spec), Some(&spec_b));
        let outcome_b = Outcome { merge_sha: Some("3333333333333333333333333333333333333333".into()), ..Outcome::ran("") };
        let completion_b: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": serde_json::to_string(&outcome_b)?,
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": job_b.workspace.key,
        }))?;
        assert_eq!(runners.begin_completion(runner, lease_id, &[8u8; 32], &completion_b).await?, BeginCompletion::Begun);
        let lease = runners.lease_for_completion(runner, lease_id).await?.expect("replacement lease");
        harness.coordinator.complete(&lease, &completion_b).await?;
        assert!(runners.finish_completion(runner, lease_id).await?);

        let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(pool).await?;
        assert_eq!(status, "done", "an action for the current pull request settles the quest");
        assert_eq!((count("action.ran").await?, count("task.merged").await?), (2, 1));

        let failing = harness
            .store
            .create_pr_action(NewAction { user_id: owner, agent_id: agent, task_id: task, spec: &ActionSpec { kind: ActionKind::Close, ..spec_b.clone() }, run_at: None, on_event: None, wait_days: None, note: None })
            .await?;
        let due = harness.store.claim_due_runner_schedules().await?.into_iter().find(|s| s.id == failing.id).expect("due");
        assert!(harness.store.fire_schedule(&due).await?);
        let claim = runners.claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &kinds).await?.expect("claimed");
        let run_id = claim.run_id;
        runners.fail_claim(runner, claim.lease_id, "runner crashed").await?;
        harness.coordinator.handle_expired(&[run_id]).await?;
        let failed = harness.store.list_agent_schedules(agent).await?.into_iter().find(|s| s.id == failing.id).expect("listed");
        assert_eq!(failed.outcome.as_ref().and_then(|o| o.get("status")).and_then(serde_json::Value::as_str), Some("failed"));
        assert_eq!(count("action.failed").await?, 1);

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(pool).await?;
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

    #[tokio::test]
    async fn a_runner_refused_clarification_is_explained() -> anyhow::Result<()> {
        use loka_shared::world::NewEvent;
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("runner-refused-{tag}@example.test"))
            .fetch_one(&harness.pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id)
             VALUES ($1, 'Pri', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("runner-refused-{tag}"))
        .bind(owner)
        .fetch_one(&harness.pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 'active') RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("main-{tag}"))
        .fetch_one(&harness.pool)
        .await?;
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id)
             VALUES ($1, 'Closed request', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let seq = harness
            .store
            .emit(
                NewEvent::new("message.created")
                    .agent(agent_id)
                    .actor(Some(owner))
                    .task(Some(task_id))
                    .payload(serde_json::json!({
                        "message_id": Uuid::new_v4(),
                        "body": "Old request",
                        "from_owner": true,
                        "task_id": task_id,
                    })),
                Some((agent_id, "message")),
            )
            .await?;
        let inbox_id: i64 = sqlx::query_scalar(
            "UPDATE agent_inbox SET status = 'processing'
             WHERE agent_id = $1 AND event_seq = $2 RETURNING id",
        )
        .bind(agent_id)
        .bind(seq)
        .fetch_one(&harness.pool)
        .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(task_id)
            .execute(&harness.pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET completed_at = NOW() + INTERVAL '1 second' WHERE id = $1")
        .bind(task_id)
        .execute(&harness.pool)
        .await?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
             VALUES ($1, $2, 'turn', 'completed', 'message', 'Need one detail') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(run_id, &[inbox_id]).await?;
        let agent = harness.store.get_agent(agent_id).await?.expect("agent");
        let main = harness.store.get_session(main_id).await?.expect("main");
        let output = format!(
            "Need one detail.\n<!-- actions\n{}\nactions -->",
            serde_json::json!([{
                "type": "ask",
                "text": "Which format?",
                "choices": ["CSV"],
                "clarify_request": true,
            }])
        );
        harness.coordinator.after_turn(&agent, &main, run_id, &output).await?;
        let replies: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages
             WHERE agent_id = $1 AND run_id = $2 AND body = $3",
        )
        .bind(agent_id)
        .bind(run_id)
        .bind(loka_shared::world::CLARIFICATION_POST_REFUSED)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(replies, 1);
        Ok(())
    }

    #[tokio::test]
    async fn an_unclear_assignment_posts_a_linked_clarification_without_starting_work() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("unclear-assignment-{tag}@example.test"))
            .fetch_one(&harness.pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Pri', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("unclear-assignment-{tag}"))
        .bind(owner)
        .fetch_one(&harness.pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 'active') RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("main-{tag}"))
        .fetch_one(&harness.pool)
        .await?;
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Choose storage', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let second_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Choose cache', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let attachment = harness
            .store
            .insert_upload("application/octet-stream", "requirements.bin", b"storage\0requirements", Some(owner))
            .await?;
        let inbox_id: i64 = sqlx::query_scalar(
            "UPDATE agent_inbox SET status = 'processing', payload = payload || $3
             WHERE id = (
               SELECT id FROM agent_inbox
               WHERE agent_id = $1 AND kind = 'task.assigned' AND payload->>'task_id' = $2
               ORDER BY id DESC LIMIT 1
             ) RETURNING id",
        )
        .bind(agent_id)
        .bind(task_id.to_string())
        .bind(serde_json::json!({
            "attachments": [{
                "id": attachment.id,
                "name": "requirements.bin",
                "mime": "application/octet-stream"
            }]
        }))
        .fetch_one(&harness.pool)
        .await?;
        let second_inbox: i64 = sqlx::query_scalar(
            "UPDATE agent_inbox SET status = 'processing'
             WHERE id = (
               SELECT id FROM agent_inbox
               WHERE agent_id = $1 AND kind = 'task.assigned' AND payload->>'task_id' = $2
               ORDER BY id DESC LIMIT 1
             ) RETURNING id",
        )
        .bind(agent_id)
        .bind(second_task.to_string())
        .fetch_one(&harness.pool)
        .await?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
             VALUES ($1, $2, 'turn', 'completed', 'assignment', 'Need one detail') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(run_id, &[inbox_id, second_inbox]).await?;
        let agent = harness.store.get_agent(agent_id).await?.expect("agent");
        let main = harness.store.get_session(main_id).await?.expect("main");
        let output = format!(
            "Need one detail.\n<!-- actions\n{}\nactions -->",
            serde_json::json!([
                {
                    "type": "ask",
                    "text": "Which database should this use?",
                    "choices": ["Postgres", "SQLite"],
                    "clarify_request": true,
                    "clarify_source": format!("quest:{task_id}"),
                },
                {
                    "type": "ask",
                    "text": "Which database should this use?",
                    "choices": ["Postgres", "SQLite"],
                    "clarify_request": true,
                    "clarify_source": format!("quest:{second_task}"),
                },
            ])
        );

        harness.coordinator.after_turn(&agent, &main, run_id, &output).await?;

        let card = harness
            .store
            .quest_clarification(task_id, agent_id, owner)
            .await?
            .expect("clarification");
        assert_eq!(card.body, "Which database should this use?");
        assert_eq!(card.meta["choices"], serde_json::json!(["Postgres", "SQLite"]));
        assert_eq!(card.meta["clarify_request"]["task_id"], task_id.to_string());
        assert_eq!(card.meta["clarify_request"]["source_inbox_id"], inbox_id);
        let second_card = harness
            .store
            .quest_clarification(second_task, agent_id, owner)
            .await?
            .expect("second clarification");
        assert_eq!(second_card.body, card.body);
        assert_eq!(second_card.meta["clarify_request"]["source_inbox_id"], second_inbox);
        let task_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(task_status, "todo");
        let work: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs
             WHERE task_id = $1 AND kind IN ('task', 'followup')",
        )
        .bind(task_id)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(work, 0);
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE task_id = $1")
            .bind(task_id)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(sessions, 0);
        let linked: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM world_events
               WHERE type = 'message.created' AND task_id = $1 AND payload->>'message_id' = $2
             )",
        )
        .bind(task_id)
        .bind(card.id.to_string())
        .fetch_one(&harness.pool)
        .await?;
        assert!(linked);
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta)
             VALUES ($1, 'user', $2, 'Postgres', $3)",
        )
        .bind(agent_id)
        .bind(owner)
        .bind(serde_json::json!({ "answers": card.id }))
        .execute(&harness.pool)
        .await?;
        harness.coordinator.after_turn(&agent, &main, run_id, &output).await?;
        let replayed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_messages
             WHERE agent_id = $1 AND meta->'clarify_request'->>'source_inbox_id' = $2",
        )
        .bind(agent_id)
        .bind(inbox_id.to_string())
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(replayed, 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_reply_only_turn_starts_a_clarified_quest_once_with_the_saved_answer() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("clarified-{tag}@example.test"))
            .fetch_one(&harness.pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Pri', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("clarified-{tag}"))
        .bind(owner)
        .fetch_one(&harness.pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 'active') RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("main-{tag}"))
        .fetch_one(&harness.pool)
        .await?;
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Choose storage', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let brief = "Original request:\nBuild the store\n\nQuestion:\nWhich database?\n\nTheir answer:\nPostgres";
        let inbox_id: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status) VALUES ($1, 'task.assigned', $2, 'processing') RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({
            "task_id": task_id,
            "title": "Choose storage",
            "brief": brief,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
        }))
        .fetch_one(&harness.pool)
        .await?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
             VALUES ($1, $2, 'turn', 'completed', 'answer', 'On it') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(run_id, &[inbox_id]).await?;
        let agent = harness.store.get_agent(agent_id).await?.expect("agent");
        let main = harness.store.get_session(main_id).await?.expect("main");
        let output = "On it.\n<!-- actions\n[{\"type\":\"reply\",\"text\":\"On it.\"}]\nactions -->";

        harness.coordinator.after_turn(&agent, &main, run_id, output).await?;
        let runs: Vec<String> = sqlx::query_scalar(
            "SELECT prompt FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup') ORDER BY created_at",
        )
        .bind(task_id)
        .fetch_all(&harness.pool)
        .await?;
        assert_eq!(runs.len(), 1);
        assert!(runs[0].contains("Their answer:\nPostgres"));
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE task_id = $1 AND status = 'queued'")
            .bind(task_id)
            .execute(&harness.pool)
            .await?;

        harness.coordinator.after_turn(&agent, &main, run_id, output).await?;
        let run_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup')",
        )
        .bind(task_id)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(run_count, 1);

        let explicit_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Use the upload', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let attachment_bytes = b"id,total\n1,42\0";
        let attachment = harness
            .store
            .insert_upload("application/octet-stream", "rows.bin", attachment_bytes, Some(owner))
            .await?;
        let explicit_brief = "Original request:\nRead the file\n\nQuestion:\nWhich one?\n\nTheir answer:\nThe CSV";
        let explicit_inbox: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status) VALUES ($1, 'task.assigned', $2, 'processing') RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({
            "task_id": explicit_task,
            "title": "Use the upload",
            "brief": explicit_brief,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": "todo",
            "clarification_resume": false,
            "attachments": [{ "id": attachment.id, "name": "rows.bin", "mime": "application/octet-stream" }],
            "files": [{ "id": attachment.id, "name": "rows.bin", "mime": "application/octet-stream", "text": "id,total\n1,42" }],
            "links": [{ "ref": "abcdef12-0000-0000-0000-000000000000", "author": "Priya", "excerpt": "Use Friday's export" }],
            "quote": { "text": "Keep the total column", "label": "Original request", "source": "msg:abcdef12" },
        }))
        .fetch_one(&harness.pool)
        .await?;
        let explicit_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
             VALUES ($1, $2, 'turn', 'completed', 'answer', 'Starting') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(explicit_run, &[explicit_inbox]).await?;
        let explicit_output = format!(
            "Starting.\n<!-- actions\n[{{\"type\":\"reply\",\"text\":\"Starting.\"}},{{\"type\":\"start_task\",\"task_id\":\"{explicit_task}\",\"brief\":\"Use a different file\"}}]\nactions -->"
        );
        harness.coordinator.after_turn(&agent, &main, explicit_run, &explicit_output).await?;
        let (started_run_id, explicit_prompt): (Uuid, String) = sqlx::query_as(
            "SELECT id, prompt FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup')",
        )
        .bind(explicit_task)
        .fetch_one(&harness.pool)
        .await?;
        assert!(explicit_prompt.contains("Their answer:\nThe CSV"));
        assert!(explicit_prompt.contains(&format!("upload {}", attachment.id)));
        assert!(explicit_prompt.contains("id,total\n1,42"));
        assert!(explicit_prompt.contains("Use Friday's export"));
        assert!(explicit_prompt.contains("Keep the total column"));
        assert!(!explicit_prompt.contains("Use a different file"));
        let leaked: bool = sqlx::query_scalar(
            "SELECT EXISTS (
               SELECT 1 FROM agent_messages
               WHERE agent_id = $1 AND author_kind = 'system'
                 AND (body LIKE '%The CSV%' OR body LIKE '%rows.bin%')
             )",
        )
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        assert!(!leaked);
        let (_, files, _) = harness.coordinator.run_uploads(started_run_id).await?;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, attachment.id);
        assert_eq!(files[0].data_base64, "aWQsdG90YWwKMSw0MgA=");
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE task_id = $1 AND status = 'queued'")
            .bind(explicit_task)
            .execute(&harness.pool)
            .await?;

        sqlx::query("UPDATE users SET world_role = 'write' WHERE id = $1")
            .bind(owner)
            .execute(&harness.pool)
            .await?;
        let runner = harness
            .coordinator
            .runners
            .register(
                owner,
                &loka_shared::runners::CreateRunner {
                    name: "Clarification Mac".into(),
                    platform: "macos".into(),
                },
            )
            .await?
            .runner
            .id;
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1")
            .bind(agent_id)
            .bind(runner)
            .execute(&harness.pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET runner_id = $2 WHERE id = $1")
            .bind(main_id)
            .bind(runner)
            .execute(&harness.pool)
            .await?;
        let regular_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Use the chat answer', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let regular_bytes = b"canonical\0answer\xff";
        let regular_upload = harness
            .store
            .insert_upload("application/octet-stream", "answer.bin", regular_bytes, Some(owner))
            .await?;
        let request_tail = "REQUEST_AFTER_5500";
        let answer_tail = "ANSWER_AFTER_5500";
        let request = format!("{}{request_tail}", "request ".repeat(800));
        let answer = format!("{}{answer_tail}", "answer ".repeat(800));
        let seq = harness
            .store
            .emit(
                loka_shared::world::NewEvent::new("message.created")
                    .agent(agent_id)
                    .actor(Some(owner))
                    .payload(serde_json::json!({
                        "author": "Ana",
                        "author_kind": "user",
                        "message_id": Uuid::new_v4(),
                        "body": answer,
                        "clarification_continuation": Uuid::new_v4(),
                        "clarification_handoff": {
                            "request": request,
                            "question": "Which database?",
                            "answer": answer
                        },
                        "attachments": [{
                            "id": regular_upload.id,
                            "name": "answer.bin",
                            "mime": "application/octet-stream"
                        }]
                    })),
                Some((agent_id, "message")),
            )
            .await?;
        let regular_inbox: i64 = sqlx::query_scalar(
            "UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1 AND event_seq = $2 RETURNING id",
        )
        .bind(agent_id)
        .bind(seq)
        .fetch_one(&harness.pool)
        .await?;
        let regular_turn: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt)
             VALUES ($1, $2, 'turn', 'queued', 'answer') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(regular_turn, &[regular_inbox]).await?;
        let turn_claim = harness
            .coordinator
            .runners
            .claim_kinds(runner, &format!("turn-{}", Uuid::new_v4()), &["turn".into()])
            .await?
            .expect("the answer turn is claimed");
        assert_eq!(turn_claim.run_id, regular_turn);
        let actions = serde_json::json!([{
            "type": "start_task",
            "task_id": regular_task,
            "brief": "wrong brief"
        }]);
        let output = format!("Starting.\n<!-- actions\n{actions}\nactions -->");
        harness.coordinator.after_turn(&agent, &main, regular_turn, &output).await?;
        let (regular_run, regular_prompt): (Uuid, String) = sqlx::query_as(
            "SELECT id, prompt FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup')",
        )
        .bind(regular_task)
        .fetch_one(&harness.pool)
        .await?;
        assert!(regular_prompt.contains(request_tail));
        assert!(regular_prompt.contains(answer_tail));
        assert_eq!(regular_prompt.matches(request_tail).count(), 1);
        assert_eq!(regular_prompt.matches(answer_tail).count(), 1);
        assert!(!regular_prompt.contains("wrong brief"));
        let work_claim = harness
            .coordinator
            .runners
            .claim_kinds(
                runner,
                &format!("work-{}", Uuid::new_v4()),
                &loka_shared::runners::WORK_RUN_KINDS.map(String::from),
            )
            .await?
            .expect("the resulting quest is claimed");
        assert_eq!(work_claim.run_id, regular_run);
        let (_, files, _) = harness.coordinator.run_uploads(regular_run).await?;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, regular_upload.id);
        assert_eq!(files[0].data_base64, base64::Engine::encode(&super::STANDARD, regular_bytes));
        Ok(())
    }

    #[tokio::test]
    async fn a_runner_clarification_retries_when_plan_handoff_has_not_started() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("retry-clarification-{tag}@example.test"))
            .fetch_one(&harness.pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Pri', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("retry-clarification-{tag}"))
        .bind(owner)
        .fetch_one(&harness.pool)
        .await?;
        let main_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status)
             VALUES ($1, 'main', 'claude', 'sonnet', $2, 'active') RETURNING id",
        )
        .bind(agent_id)
        .bind(format!("main-{tag}"))
        .fetch_one(&harness.pool)
        .await?;
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, plan_first)
             VALUES ($1, 'Resume after plan', 'in_progress', $2, TRUE) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id, plan_state)
             VALUES ($1, 'task', 'claude', 'sonnet', 'task-thread', 'active', $2, 'awaiting') RETURNING id",
        )
        .bind(agent_id)
        .bind(task_id)
        .fetch_one(&harness.pool)
        .await?;
        let inbox_id: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts)
             VALUES ($1, 'task.assigned', $2, 'processing', 1) RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({
            "task_id": task_id,
            "title": "Resume after plan",
            "brief": "Original request:\nBuild it\n\nQuestion:\nContinue?\n\nTheir answer:\nYes",
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": {
                "request": "Build it",
                "question": "Continue?",
                "answer": "Yes"
            },
            "clarification_status": "in_progress",
            "clarification_resume": false,
            "attachments": [{ "id": Uuid::new_v4(), "name": "choice.txt", "mime": "text/plain" }],
        }))
        .fetch_one(&harness.pool)
        .await?;
        let first_turn: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
             VALUES ($1, $2, 'turn', 'completed', 'answer', 'Starting') RETURNING id",
        )
        .bind(main_id)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        harness.coordinator.runners.mark_run_inbox(first_turn, &[inbox_id]).await?;
        let agent = harness.store.get_agent(agent_id).await?.expect("agent");
        let main = harness.store.get_session(main_id).await?.expect("main");
        let output = format!(
            "Starting.\n<!-- actions\n[{{\"type\":\"start_task\",\"task_id\":\"{task_id}\"}}]\nactions -->"
        );
        harness.coordinator.after_turn(&agent, &main, first_turn, &output).await?;
        harness.store.set_agent_status(agent_id, "thinking", None).await?;
        harness.coordinator.after_turn(&agent, &main, first_turn, &output).await?;
        assert_eq!(harness.store.get_agent(agent_id).await?.expect("agent").status, "idle");
        let waiting: (String, i32, bool) = sqlx::query_as(
            "SELECT status, attempts, retry_at = 'infinity' FROM agent_inbox WHERE id = $1",
        )
            .bind(inbox_id)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(waiting, ("pending".into(), 1, true));
        let problems: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM world_events WHERE run_id = $1 AND type = 'actions.problem'",
        )
        .bind(first_turn)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(problems, 0);

        assert!(!harness.store.take_pending_inbox(agent_id, 20).await?.iter().any(|item| item.id == inbox_id));
        let approved = harness.store.approve_plan(task_id, "owner", "test", "Approve").await?;
        assert!(matches!(approved, loka_shared::quests::PlanApproval::Approved { .. }));
        let failed_inbox: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts)
             VALUES ($1, 'task.assigned', $2, 'processing', 1) RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({
            "task_id": task_id,
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_handoff": { "request": "Build it", "question": "Continue?", "answer": "Yes" },
            "clarification_status": "in_progress",
            "clarification_resume": false,
        }))
        .fetch_one(&harness.pool)
        .await?;
        let invalid_output = format!(
            "Starting.\n<!-- actions\n[{{\"type\":\"start_task\",\"task_id\":\"{task_id}\",\"plan_first\":true,\"attempts\":2}}]\nactions -->"
        );
        for attempt in 1..=3 {
            sqlx::query("UPDATE agent_inbox SET status = 'processing', attempts = $2, retry_at = NULL WHERE id = $1")
                .bind(failed_inbox)
                .bind(attempt)
                .execute(&harness.pool)
                .await?;
            let turn: Uuid = sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, output)
                 VALUES ($1, $2, 'turn', 'completed', 'answer', $3) RETURNING id",
            )
            .bind(main_id)
            .bind(agent_id)
            .bind(&invalid_output)
            .fetch_one(&harness.pool)
            .await?;
            harness.coordinator.runners.mark_run_inbox(turn, &[failed_inbox]).await?;
            harness.coordinator.after_turn(&agent, &main, turn, &invalid_output).await?;
            let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
                .bind(failed_inbox)
                .fetch_one(&harness.pool)
                .await?;
            assert_eq!(status, if attempt == 3 { "failed" } else { "pending" });
        }
        let exhausted: (String, i32) = sqlx::query_as("SELECT status, attempts FROM agent_inbox WHERE id = $1")
            .bind(failed_inbox)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(exhausted, ("failed".into(), 3));
        sqlx::query("UPDATE session_runs SET status = 'completed' WHERE task_id = $1 AND kind = 'followup'")
            .bind(task_id)
            .execute(&harness.pool)
            .await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
            .bind(task_id)
            .execute(&harness.pool)
            .await?;
        let ordinary_inbox: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status)
             VALUES ($1, 'message', $2, 'processing') RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({ "author_kind": "user", "from_owner": true, "body": "Also note this" }))
        .fetch_one(&harness.pool)
        .await?;
        let ordinary_task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id)
             VALUES ($1, 'Handle the ordinary assignment', 'todo', $2) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&harness.pool)
        .await?;
        let ordinary_assignment: i64 = sqlx::query_scalar(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status)
             VALUES ($1, 'task.assigned', $2, 'processing') RETURNING id",
        )
        .bind(agent_id)
        .bind(serde_json::json!({
            "task_id": ordinary_task,
            "title": "Handle the ordinary assignment",
            "brief": "Handle this once",
            "resume": true,
        }))
        .fetch_one(&harness.pool)
        .await?;
        harness
            .coordinator
            .runners
            .mark_run_inbox(first_turn, &[ordinary_inbox, ordinary_assignment])
            .await?;
        harness.coordinator.after_turn(&agent, &main, first_turn, &output).await?;
        let before_replay: (i64, i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT COUNT(*) FROM agent_messages WHERE run_id = $1 AND author_kind = 'agent' AND body = 'Starting.'),
               (SELECT COUNT(*) FROM world_events WHERE run_id = $1 AND type = 'agent.problem'),
               (SELECT COUNT(*) FROM session_runs WHERE task_id = $2 AND kind IN ('task', 'followup'))",
        )
        .bind(first_turn)
        .bind(ordinary_task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(before_replay.0, 1);
        assert_eq!(before_replay.2, 1);
        harness.coordinator.after_turn(&agent, &main, first_turn, &output).await?;
        let after_replay: (i64, i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT COUNT(*) FROM agent_messages WHERE run_id = $1 AND author_kind = 'agent' AND body = 'Starting.'),
               (SELECT COUNT(*) FROM world_events WHERE run_id = $1 AND type = 'agent.problem'),
               (SELECT COUNT(*) FROM session_runs WHERE task_id = $2 AND kind IN ('task', 'followup'))",
        )
        .bind(first_turn)
        .bind(ordinary_task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(after_replay, before_replay);
        let prompts: Vec<String> = sqlx::query_scalar(
            "SELECT prompt FROM session_runs WHERE task_id = $1 AND kind = 'followup' ORDER BY created_at",
        )
            .bind(task_id)
            .fetch_all(&harness.pool)
            .await?;
        let links: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runner_run_inbox x JOIN session_runs r ON r.id = x.run_id
             WHERE r.task_id = $1 AND x.inbox_id = $2",
        )
        .bind(task_id)
        .bind(inbox_id)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Their answer:\nYes"));
        assert_eq!(prompts[0].matches("Their answer:\nYes").count(), 1);
        assert!(prompts[0].contains("choice.txt"));
        assert_eq!(links, 1);
        let settled: (String, i32) = sqlx::query_as("SELECT status, attempts FROM agent_inbox WHERE id = $1")
            .bind(inbox_id)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(settled, ("done".into(), 1));
        let session = harness.store.get_session(task_session).await?.expect("task session");
        assert_eq!(session.plan_state.as_deref(), Some("approved"));
        assert!(!harness.store.take_pending_inbox(agent_id, 20).await?.iter().any(|item| item.id == inbox_id));
        let ordinary_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(ordinary_inbox)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(ordinary_status, "done");
        let assignment_status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(ordinary_assignment)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(assignment_status, "done");
        Ok(())
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
        if merged {
            sqlx::query("UPDATE agent_tasks SET merged_pr_url = pr_url WHERE id = $1")
                .bind(task)
                .execute(pool)
                .await?;
        }
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
        let done_output = "Merged it.\n<!-- report {\"status\": \"done\", \"summary\": \"merged\"} report -->";
        for (sync, reviewed, reviews, status) in [
            (PrSync::Merged, false, 0, "done"),
            (PrSync::Unchanged, true, 1, "review"),
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
            let queued: Option<Uuid> = if sync == PrSync::Merged {
                Some(
                    sqlx::query_scalar(
                        "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
                         VALUES ($1, $2, $3, 'followup', 'queued', 'clean up') RETURNING id",
                    )
                    .bind(quest.task_session)
                    .bind(quest.agent.id)
                    .bind(quest.task)
                    .fetch_one(&harness.pool)
                    .await?,
                )
            } else {
                None
            };
            let session = harness.store.get_session(quest.task_session).await?.expect("task session");
            let verified_pr = if sync == PrSync::Merged {
                FollowupPrSync { sync, merged_pr_url: Some("https://github.com/acme/app/pull/9") }
            } else {
                FollowupPrSync::observed(sync)
            };
            let output = if sync == PrSync::Merged {
                "Merged it.\n<!-- report {\"status\": \"blocked\", \"summary\": \"the final check did not return\"} report -->"
            } else {
                done_output
            };
            harness.coordinator.after_followup(&quest.agent, &session, run, output, None, verified_pr).await?;
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
                let queued_status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1")
                    .bind(queued.expect("queued follow-up"))
                    .fetch_one(&harness.pool)
                    .await?;
                assert_eq!(queued_status, "cancelled", "verified settlement cancels the obsolete queued follow-up");
            }
        }

        let quest = merged_quest(&harness, false).await?;
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'followup', 'completed', 'merge it') RETURNING id",
        )
        .bind(quest.task_session)
        .bind(quest.agent.id)
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        let session = harness.store.get_session(quest.task_session).await?.expect("task session");
        harness
            .coordinator
            .after_followup(
                &quest.agent,
                &session,
                run,
                "Merged it.",
                None,
                FollowupPrSync { sync: PrSync::Merged, merged_pr_url: Some("https://github.com/acme/app/pull/9") },
            )
            .await?;
        assert_eq!(quest_status(&harness, &quest).await?, "done", "a verified merge does not wait for a report");

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
            harness
                .coordinator
                .after_followup(&quest.agent, &session, run, done_output, None, FollowupPrSync::observed(PrSync::Unchanged))
                .await?;
        }
        assert_eq!(review_sessions(&harness, &quest).await?, 1, "follow-ups that change nothing keep the earlier review");
        assert_eq!(
            quest_status(&harness, &quest).await?,
            "review",
            "unchanged follow-ups keep the existing pull request in review"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_verified_followup_merge_does_not_settle_a_replaced_pull_request() -> anyhow::Result<()> {
        use loka_shared::runners::PrSync;
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'followup', 'completed', 'merge it') RETURNING id",
        )
        .bind(quest.task_session)
        .bind(quest.agent.id)
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        sqlx::query("UPDATE agent_tasks SET pr_url = 'https://github.com/acme/app/pull/10', pr_number = 10 WHERE id = $1")
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
        let session = harness.store.get_session(quest.task_session).await?.expect("task session");
        harness
            .coordinator
            .after_followup(
                &quest.agent,
                &session,
                run,
                "Merged it.\n<!-- report {\"status\": \"done\", \"summary\": \"merged\"} report -->",
                None,
                FollowupPrSync { sync: PrSync::Merged, merged_pr_url: Some("https://github.com/acme/app/pull/9") },
            )
            .await?;
        let state: (String, Option<String>, Option<i32>, String) = sqlx::query_as(
            "SELECT t.status, t.pr_url, t.pr_number, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(state, ("in_progress".into(), Some("https://github.com/acme/app/pull/10".into()), Some(10), "active".into()));
        let merged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.merged'")
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(merged, 0);
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
                .after_followup(
                    &quest.agent,
                    &task_session(&harness, &quest).await?,
                    followup.id,
                    done_report("claude"),
                    None,
                    FollowupPrSync::observed(PrSync::Unchanged),
                )
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

    #[tokio::test]
    async fn publish_completion_never_attaches_report_prose_without_verified_identity() -> anyhow::Result<()> {
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = unpublished_quest(&harness, "claude").await?;
        let summary = serde_json::json!({
            "status": "done",
            "summary": "Merged it",
            "pr_url": "https://github.com/acme/ghar-mail/pull/9",
        })
        .to_string();
        sqlx::query("UPDATE agent_sessions SET summary = $2 WHERE id = $1")
            .bind(quest.task_session)
            .bind(&summary)
            .execute(&harness.pool)
            .await?;
        let session = task_session(&harness, &quest).await?;
        assert_eq!(
            prompts::stored_report(session.summary.as_deref())
                .and_then(|report| report.reported_pr("acme/ghar-mail")),
            Some(("https://github.com/acme/ghar-mail/pull/9".into(), 9)),
        );
        let completion = |state: Option<&str>| -> anyhow::Result<loka_shared::runners::RunnerCompletion> {
            Ok(serde_json::from_value(serde_json::json!({
                "output": "Nothing to publish",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": null,
                "pr_number": null,
                "publish_pr_state": state,
            }))?)
        };
        let terminal = completion(Some("merged"))?;
        let error = harness
            .coordinator
            .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &terminal)
            .await
            .expect_err("a terminal result needs a concrete pull request");
        assert!(error.to_string().contains("no pull request"));
        harness
            .coordinator
            .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &completion(None)?)
            .await?;
        let recorded: (Option<String>, Option<i32>) = sqlx::query_as("SELECT pr_url, pr_number FROM agent_tasks WHERE id = $1")
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(recorded, (None, None));
        sqlx::query("UPDATE agent_tasks SET status = 'failed', error_message = 'earlier run failed' WHERE id = $1")
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
        let merged: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": "Found the merged pull request",
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": "tasks/x",
            "pr_url": "https://github.com/acme/ghar-mail/pull/9",
            "pr_number": 9,
            "publish_pr_state": "merged",
        }))?;
        harness.coordinator.after_publish(&quest.agent, &session, &merged).await?;
        let settled: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, pr_url, merged_pr_url, error_message FROM agent_tasks WHERE id = $1",
        )
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(
            settled,
            (
                "done".into(),
                Some("https://github.com/acme/ghar-mail/pull/9".into()),
                Some("https://github.com/acme/ghar-mail/pull/9".into()),
                None,
            ),
        );
        Ok(())
    }

    #[tokio::test]
    async fn edited_repo_publish_results_require_proof_of_the_tracked_pull_request() -> anyhow::Result<()> {
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        sqlx::query(
            "UPDATE agent_tasks SET repo = 'acme/repo-b', status = 'failed', error_message = 'earlier failure' WHERE id = $1",
        )
        .bind(quest.task)
        .execute(&harness.pool)
        .await?;
        let completion = |checked: Option<&str>| -> anyhow::Result<loka_shared::runners::RunnerCompletion> {
            Ok(serde_json::from_value(serde_json::json!({
                "output": "Found the merged pull request",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": "https://github.com/acme/app/pull/9",
                "pr_number": 9,
                "publish_pr_state": "merged",
                "checked_pr_url": checked,
            }))?)
        };
        assert!(
            harness
                .coordinator
                .after_publish(
                    &quest.agent,
                    &task_session(&harness, &quest).await?,
                    &completion(None)?,
                )
                .await
                .is_err()
        );
        assert_eq!(quest_status(&harness, &quest).await?, "failed");
        harness
            .coordinator
            .after_publish(
                &quest.agent,
                &task_session(&harness, &quest).await?,
                &completion(Some("https://github.com/acme/app/pull/9"))?,
            )
            .await?;
        assert_eq!(quest_status(&harness, &quest).await?, "done");

        let stale = merged_quest(&harness, false).await?;
        sqlx::query(
            "UPDATE agent_tasks SET repo = 'acme/repo-b', pr_url = 'https://github.com/acme/repo-b/pull/22', status = 'failed' WHERE id = $1",
        )
        .bind(stale.task)
        .execute(&harness.pool)
        .await?;
        assert!(harness
            .coordinator
            .after_publish(
                &stale.agent,
                &task_session(&harness, &stale).await?,
                &completion(Some("https://github.com/acme/app/pull/9"))?,
            )
            .await
            .is_err());
        assert_eq!(quest_status(&harness, &stale).await?, "failed");
        Ok(())
    }

    #[tokio::test]
    async fn publish_completion_rearms_only_a_newer_verified_open_pull_request() -> anyhow::Result<()> {
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        for newer in [true, false] {
            let quest = merged_quest(&harness, false).await?;
            let observed_at = chrono::Utc::now();
            let closed_at = observed_at.timestamp() as f64 + if newer { -60.0 } else { 60.0 };
            sqlx::query(
                "UPDATE agent_tasks SET status = 'failed', error_message = 'earlier close', repo = 'acme/app', pr_number = 9 WHERE id = $1",
            )
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
            sqlx::query(
                "UPDATE task_milestones SET status = 'failed', detail = 'Closed without merging',
                     data = jsonb_build_object('closed_pr_url', 'https://github.com/acme/app/pull/9', 'closed_at', $2::double precision),
                     done_at = NOW(), next_check_at = NULL
                 WHERE task_id = $1 AND kind = 'merge'",
            )
            .bind(quest.task)
            .bind(closed_at)
            .execute(&harness.pool)
            .await?;
            sqlx::query("UPDATE agent_sessions SET summary = $2 WHERE id = $1")
                .bind(quest.task_session)
                .bind(done_report("claude"))
                .execute(&harness.pool)
                .await?;
            let completion: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
                "output": "Nothing new to publish",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": "https://github.com/acme/app/pull/9",
                "pr_number": 9,
                "publish_pr_state": "open",
                "publish_pr_checked_at": observed_at,
            }))?;
            harness
                .coordinator
                .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &completion)
                .await?;
            let milestone: (String, Option<String>, bool) = sqlx::query_as(
                "SELECT status, detail, next_check_at IS NOT NULL FROM task_milestones WHERE task_id = $1 AND kind = 'merge'",
            )
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
            if newer {
                assert_eq!(milestone, ("active".into(), Some("Waiting for review".into()), true));
                let stale_close: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
                    "output": "Pull request is closed",
                    "session_id": null,
                    "exit_code": 0,
                    "error": null,
                    "workspace_key": "tasks/x",
                    "pr_url": "https://github.com/acme/app/pull/9",
                    "pr_number": 9,
                    "publish_pr_state": "closed",
                    "publish_pr_checked_at": observed_at - chrono::Duration::seconds(120),
                }))?;
                harness
                    .coordinator
                    .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &stale_close)
                    .await?;
                let status: String = sqlx::query_scalar(
                    "SELECT status FROM task_milestones WHERE task_id = $1 AND kind = 'merge'",
                )
                .bind(quest.task)
                .fetch_one(&harness.pool)
                .await?;
                assert_eq!(status, "active", "an older close cannot undo the verified reopen");
            } else {
                assert_eq!(milestone, ("failed".into(), Some("Closed without merging".into()), false));
            }
        }
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
        coordinator
            .after_followup(
                &quest.agent,
                &session(&harness).await?,
                reminders[0].0,
                "Still done.",
                work,
                FollowupPrSync::observed(PrSync::Changed),
            )
            .await?;
        let reminders = quest_runs(&harness, &quest, "followup").await?;
        assert_eq!(reminders.len(), 2, "a reminder that still has no report gets one more");
        assert_eq!(reminders[1].1, loka_shared::world::REPORT_NUDGE_LAST);
        assert!(quest_runs(&harness, &quest, "publish").await?.is_empty(), "nothing is published while a reminder is still pending");

        finish_run(&harness, reminders[1].0).await?;
        coordinator
            .after_followup(
                &quest.agent,
                &session(&harness).await?,
                reminders[1].0,
                "Done, really.",
                work,
                FollowupPrSync::observed(PrSync::Changed),
            )
            .await?;
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
            checked_pr_url: None,
            publish_pr_checked_at: None,
            checkpoint: None,
            steered: Vec::new(),
            retryable: false,
            work_summary: None,
            context_sent: None,
            published_head: None,
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

        coordinator
            .after_followup(
                &quest.agent,
                &session(&harness).await?,
                reminders[1].0,
                "Done, really.",
                work,
                FollowupPrSync::observed(PrSync::Changed),
            )
            .await?;
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
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, nudge)
                 VALUES ($1, $2, $3, $4, 'completed', $5, CASE WHEN $4 = 'followup' THEN 'report' END) RETURNING id",
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
        harness
            .coordinator
            .after_followup(&quest.agent, &session, nudge, "Done.", None, FollowupPrSync::observed(PrSync::Changed))
            .await?;
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
    async fn a_review_stopped_after_the_pr_merged_lets_publication_settle_the_quest() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        for (merged, status) in [(true, "done"), (false, "failed")] {
            let quest = merged_quest(&harness, merged).await?;
            let task_session = harness.store.get_session(quest.task_session).await?.expect("task session");
            harness.coordinator.review_failed(&quest.agent, &task_session, Uuid::new_v4(), "Stopped by Bun", false).await?;
            let publishes = quest_runs(&harness, &quest, "publish").await?;
            assert_eq!(publishes.len(), usize::from(merged), "merged: {merged}, only publication knows whether new work waits");
            if merged {
                assert_eq!(quest_status(&harness, &quest).await?, "in_progress", "nothing settles before publication looked");
                finish_run(&harness, publishes[0].0).await?;
                let nothing_new: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
                    "output": "Pull request #9 is already merged, nothing new to publish",
                    "session_id": null,
                    "exit_code": 0,
                    "error": null,
                    "workspace_key": "tasks/x",
                    "pr_url": "https://github.com/acme/app/pull/9",
                    "pr_number": 9,
                    "publish_pr_state": "merged",
                }))?;
                sqlx::query("UPDATE agent_tasks SET repo = 'acme/app', pr_number = 9 WHERE id = $1").bind(quest.task).execute(&harness.pool).await?;
                harness.coordinator.after_publish(&quest.agent, &task_session, &nothing_new).await?;
            }
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
            (stopped, true, true, "in_progress", 1),
            (crashed, true, true, "in_progress", 1),
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

    #[tokio::test]
    async fn new_work_published_after_the_pr_merged_becomes_the_quests_pr() -> anyhow::Result<()> {
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        let old = "https://github.com/acme/app/pull/9";
        let new = "https://github.com/acme/app/pull/12";
        sqlx::query("UPDATE agent_tasks SET repo = 'acme/app', pr_number = 9 WHERE id = $1").bind(quest.task).execute(&harness.pool).await?;
        sqlx::query("UPDATE agent_sessions SET summary = $2 WHERE id = $1")
            .bind(quest.task_session)
            .bind(r#"{"status":"done","summary":"Renamed the setting"}"#)
            .execute(&harness.pool)
            .await?;
        let task_session = harness.store.get_session(quest.task_session).await?.expect("task session");
        harness.coordinator.settle_merged(&quest.agent, &task_session, quest.task, old).await?;
        let published: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": format!("Published {new}"),
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": "tasks/x",
            "pr_url": new,
            "pr_number": 12,
            "publish_pr_state": "replaced",
            "checked_pr_url": old,
        }))?;
        harness.coordinator.after_publish(&quest.agent, &task_session, &published).await?;
        harness.coordinator.after_publish(&quest.agent, &task_session, &published).await?;
        let (status, pr_url, past): (String, String, serde_json::Value) =
            sqlx::query_as("SELECT status, pr_url, past_prs FROM agent_tasks WHERE id = $1").bind(quest.task).fetch_one(&harness.pool).await?;
        assert_eq!((status.as_str(), pr_url.as_str()), ("review", new), "the new PR is the quest's PR and waits for review");
        assert_eq!(past, serde_json::json!([{ "url": old, "number": 9, "state": "merged", "replaced_at": past[0]["replaced_at"] }]));
        let merge: String = sqlx::query_scalar("SELECT status FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
            .bind(quest.task)
            .fetch_one(&harness.pool)
            .await?;
        assert_eq!(merge, "active", "the merge watch follows the new PR");
        let finished: Option<String> = sqlx::query_scalar(
            "SELECT payload->>'pr_url' FROM world_events WHERE task_id = $1 AND type = 'task.finished' ORDER BY seq DESC LIMIT 1",
        )
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(finished.as_deref(), Some(new));
        harness.coordinator.settle_merged(&quest.agent, &task_session, quest.task, new).await?;
        harness.coordinator.settle_merged(&quest.agent, &task_session, quest.task, new).await?;
        let merged: Vec<String> = sqlx::query_scalar(
            "SELECT payload->>'pr_url' FROM world_events WHERE task_id = $1 AND type = 'task.merged' ORDER BY seq",
        )
        .bind(quest.task)
        .fetch_all(&harness.pool)
        .await?;
        assert_eq!(merged, [old, new], "each PR announces its own merge once");
        assert_eq!(quest_status(&harness, &quest).await?, "done");
        Ok(())
    }

    #[test]
    fn a_followup_only_reports_the_recorded_pull_request_as_merged() {
        let completion = |url: Option<&str>, number: Option<i64>| -> loka_shared::runners::RunnerCompletion {
            serde_json::from_value(serde_json::json!({
                "output": "Merged it",
                "session_id": null,
                "exit_code": 0,
                "error": null,
                "workspace_key": "tasks/x",
                "pr_url": url,
                "pr_number": number,
                "publish_pr_state": "merged",
            }))
            .unwrap()
        };
        let recorded = Some("https://github.com/acme/app/pull/7");
        assert_eq!(
            verified_followup_sync(&completion(None, None), recorded, Some("acme/app")).sync,
            PrSync::Changed
        );
        assert_eq!(
            verified_followup_sync(
                &completion(Some("https://github.com/acme/app/pull/8"), Some(8)),
                recorded,
                Some("acme/app"),
            )
            .sync,
            PrSync::Changed,
        );
        let matching = completion(Some("https://github.com/acme/app/pull/7"), Some(7));
        assert_eq!(
            verified_followup_sync(&matching, recorded, Some("acme/app")),
            FollowupPrSync {
                sync: PrSync::Merged,
                merged_pr_url: Some("https://github.com/acme/app/pull/7"),
            },
        );
        assert_eq!(
            verified_followup_sync(&matching, recorded, Some("acme/edited")).sync,
            PrSync::Changed
        );
        let mut proved = matching;
        proved.checked_pr_url = Some("https://github.com/acme/other/pull/7".into());
        assert_eq!(
            verified_followup_sync(&proved, recorded, Some("acme/app")).sync,
            PrSync::Changed
        );
        proved.checked_pr_url = Some("https://github.com/acme/app/pull/7".into());
        assert_eq!(
            verified_followup_sync(&proved, recorded, Some("acme/edited")).sync,
            PrSync::Merged
        );
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
            .after_followup(
                &quest.agent,
                &task_session(harness, quest).await?,
                run,
                output,
                None,
                FollowupPrSync::observed(loka_shared::runners::PrSync::Changed),
            )
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

            let withdrawn = unpublished_quest(&harness, cli).await?;
            let owner = withdrawn.agent.owner_user_id.expect("owner");
            let runners = loka_shared::runners::RunnerStore::new(harness.pool.clone());
            let device = runners.register(owner, &loka_shared::runners::CreateRunner { name: "Withdraw Mac".into(), platform: "macos".into() }).await?.runner.id;
            sqlx::query("UPDATE agent_sessions SET runner_id = $2, status = 'completed' WHERE id = $1").bind(withdrawn.task_session).bind(device).execute(&harness.pool).await?;
            let answered = harness.store.start_followup(&task_session(&harness, &withdrawn).await?, "One more tweak.", "Abhi", false).await?;
            let stale = harness.store.start_followup(&task_session(&harness, &withdrawn).await?, "And the docs.", "Abhi", false).await?;
            answer_followup(&harness, &withdrawn, answered.id, done_report(cli)).await?;
            assert!(quest_runs(&harness, &withdrawn, "publish").await?.is_empty(), "{cli}: the finished follow-up waits for the queued one");
            assert!(harness.coordinator.stop_run(stale.id, "Abhi").await?, "{cli}: a queued follow-up can be withdrawn");
            assert_eq!(quest_runs(&harness, &withdrawn, "publish").await?.len(), 1, "{cli}: withdrawing it carries the finished work on to publish");
            assert!(!harness.coordinator.stop_run(stale.id, "Abhi").await?, "{cli}: withdrawing twice does nothing");
            assert_eq!(quest_runs(&harness, &withdrawn, "publish").await?.len(), 1, "{cli}: and never publishes twice");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_quest_keeps_its_pull_request_through_followup_publish_and_merge() -> anyhow::Result<()> {
        use loka_shared::runners::PrSync;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let quest = merged_quest(&harness, false).await?;
        let recorded = "https://github.com/acme/app/pull/9";
        sqlx::query("UPDATE agent_tasks SET repo = 'acme/app', pr_number = 9, status = 'failed', error_message = 'earlier run failed' WHERE id = $1")
            .bind(quest.task)
            .execute(&harness.pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET status = 'failed', review = FALSE WHERE id = $1")
            .bind(quest.task_session)
            .execute(&harness.pool)
            .await?;

        let followup = harness
            .store
            .start_followup(&task_session(&harness, &quest).await?, "Finish it.", "Abhi", false)
            .await?;
        finish_run(&harness, followup.id).await?;
        harness
            .coordinator
            .after_followup(
                &quest.agent,
                &task_session(&harness, &quest).await?,
                followup.id,
                done_report("claude"),
                None,
                FollowupPrSync::observed(PrSync::Changed),
            )
            .await?;
        let publish = quest_runs(&harness, &quest, "publish").await?;
        assert_eq!(publish.len(), 1);
        finish_run(&harness, publish[0].0).await?;
        let completion: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": "Nothing new to publish",
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": "tasks/x",
            "pr_url": null,
            "pr_number": null,
            "publish_pr_state": null,
        }))?;
        harness
            .coordinator
            .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &completion)
            .await?;
        let state: (String, Option<String>, Option<i32>, String) = sqlx::query_as(
            "SELECT t.status, t.pr_url, t.pr_number, m.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             WHERE t.id = $1",
        )
        .bind(quest.task)
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(state, ("review".into(), Some(recorded.into()), Some(9), "active".into()));

        assert!(harness.store.settle_pr(quest.task, recorded, true).await?);
        assert_eq!(quest_status(&harness, &quest).await?, "done");
        let cards_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE meta->>'task_id' = $1")
            .bind(quest.task.to_string())
            .fetch_one(&harness.pool)
            .await?;
        let stale: loka_shared::runners::RunnerCompletion = serde_json::from_value(serde_json::json!({
            "output": "Published a different pull request",
            "session_id": null,
            "exit_code": 0,
            "error": null,
            "workspace_key": "tasks/x",
            "pr_url": "https://github.com/acme/app/pull/10",
            "pr_number": 10,
            "publish_pr_state": "merged",
        }))?;
        harness
            .coordinator
            .after_publish(&quest.agent, &task_session(&harness, &quest).await?, &stale)
            .await?;
        let after: (String, Option<String>, Option<i32>, i64) = sqlx::query_as(
            "SELECT status, pr_url, pr_number,
                    (SELECT COUNT(*) FROM agent_messages WHERE meta->>'task_id' = $2)
             FROM agent_tasks WHERE id = $1",
        )
        .bind(quest.task)
        .bind(quest.task.to_string())
        .fetch_one(&harness.pool)
        .await?;
        assert_eq!(after, ("done".into(), Some(recorded.into()), Some(9), cards_before));
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
        finish_run(&harness, followup.id).await?;
        harness
            .coordinator
            .fail_run(
                followup.id,
                "followup",
                &task_session(&harness, &done).await?,
                &done.agent,
                "A late failure",
                false,
            )
            .await?;
        assert_eq!(quest_status(&harness, &done).await?, "done");
        let (quiet, inbox, agent_status): (bool, i64, String) = sqlx::query_as(
            "SELECT COALESCE((e.payload->>'quiet')::boolean, FALSE),
                    (SELECT COUNT(*) FROM agent_inbox i WHERE i.event_seq = e.seq),
                    (SELECT status FROM agent_profiles WHERE id = $2)
             FROM world_events e WHERE e.run_id = $1 AND e.type = 'run.failed'",
        )
        .bind(followup.id)
        .bind(done.agent.id)
        .fetch_one(&harness.pool)
        .await?;
        assert!(!quiet);
        assert_eq!(inbox, 1);
        assert_eq!(agent_status, "error");

        let stale = unpublished_quest(&harness, "claude").await?;
        block_first_run(&harness, &stale).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &stale).await?, "Finish this.", "Abhi", false).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done', error_message = NULL WHERE id = $1")
            .bind(stale.task)
            .execute(&harness.pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.merged', $1, $2, jsonb_build_object('status', 'done'))")
            .bind(stale.agent.id)
            .bind(stale.task)
            .execute(&harness.pool)
            .await?;
        finish_run(&harness, followup.id).await?;
        harness
            .coordinator
            .fail_run(
                followup.id,
                "followup",
                &task_session(&harness, &stale).await?,
                &stale.agent,
                "A stale failure",
                false,
            )
            .await?;
        let (quiet, inbox): (bool, i64) = sqlx::query_as(
            "SELECT COALESCE((e.payload->>'quiet')::boolean, FALSE),
                    (SELECT COUNT(*) FROM agent_inbox i WHERE i.event_seq = e.seq)
             FROM world_events e WHERE e.run_id = $1 AND e.type = 'run.failed'",
        )
        .bind(followup.id)
        .fetch_one(&harness.pool)
        .await?;
        assert!(quiet);
        assert_eq!(inbox, 0);
        assert_eq!(quest_status(&harness, &stale).await?, "done");

        let cancelled = unpublished_quest(&harness, "claude").await?;
        block_first_run(&harness, &cancelled).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'cancelled' WHERE id = $1").bind(cancelled.task).execute(&harness.pool).await?;
        let followup = harness.store.start_followup(&task_session(&harness, &cancelled).await?, "What did you change?", "Abhi", false).await?;
        assert!(followup.prompt.ends_with(FOLLOWUP_ASK));
        assert_eq!(session_status(&harness, &cancelled).await?, "failed", "a cancelled quest is never reopened");
        assert_eq!(quest_status(&harness, &cancelled).await?, "cancelled");
        Ok(())
    }

    #[tokio::test]
    async fn a_device_main_session_carries_its_rules_from_the_first_turn_until_compaction() -> anyhow::Result<()> {
        use loka_shared::context::{SentContext, INSTRUCTIONS_BLOCK, SKILLS_BLOCK};
        use loka_shared::runners::{ClaimedRun, LeaseContext, RunnerCompletion, RunnerUsage};
        use loka_shared::world::{NewRun, NewSession};
        use uuid::Uuid;
        let Some(harness) = harness().await? else {
            return Ok(());
        };
        let pool = &harness.pool;
        let store = &harness.store;
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("rules-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Rue', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("rules-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        let rule = store.create_rule(Some(owner), None, "talk", &format!("Sign every reply {tag}.")).await?;
        let runner: Uuid = sqlx::query_scalar(
            "INSERT INTO world_runners (owner_user_id, name, platform, token_hash) VALUES ($1, $2, 'macos', sha256(convert_to($2, 'UTF8'))) RETURNING id",
        )
        .bind(owner)
        .bind(format!("mac-{tag}"))
        .fetch_one(pool)
        .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let key = format!("agents/{agent_id}");
        let first_main = store
            .create_session(NewSession {
                agent_id,
                role: "main",
                backend: "claude",
                model: "sonnet",
                parent_session_id: None,
                task_id: None,
                working_dir: Some(&key),
                usage_baseline: serde_json::json!({}),
            })
            .await?;

        let turn = |main: Uuid| {
            let harness = &harness;
            let tag = tag.clone();
            async move {
                let session = harness.store.get_session(main).await?.expect("main");
                let run = harness
                    .store
                    .create_run(NewRun {
                        session_id: main,
                        agent_id,
                        task_id: None,
                        kind: "turn",
                        status: "running",
                        fork_from: None,
                        prompt: "hello",
                        timeout_secs: 600,
                        request: None,
                    })
                    .await?;
                let claimed = ClaimedRun {
                    lease_id: Uuid::new_v4(),
                    run_id: run.id,
                    session_id: main,
                    agent_id,
                    runner_id: runner,
                    task_id: None,
                    kind: "turn".into(),
                    prompt: "hello".into(),
                    timeout_secs: 600,
                    backend: "claude".into(),
                    model: "sonnet".into(),
                    cli_session_id: session.cli_session_id.clone(),
                    fork_from: None,
                    role: "main".into(),
                    task_repo: None,
                    task_branch: None,
                    task_base_branch: None,
                    agent_name: "Rue".into(),
                    agent_slug: format!("rules-{tag}"),
                    agent_persona: None,
                    agent_addendum: None,
                    owner_user_id: owner,
                };
                let job = harness.coordinator.build_job(claimed, "token".into()).await?;
                let mut sent = job.context_sent.clone();
                sent.offer(SKILLS_BLOCK, "[skills]\n- notes.md");
                let lease = LeaseContext {
                    lease_id: job.lease_id,
                    run_id: run.id,
                    runner_id: runner,
                    agent_id,
                    session_id: main,
                    owner_user_id: owner,
                    kind: "turn".into(),
                };
                let completion = RunnerCompletion {
                    output: "hi".into(),
                    session_id: Some(format!("cli-{tag}")),
                    cost_usd: 0.0,
                    exit_code: 0,
                    usage: RunnerUsage::default(),
                    error: None,
                    workspace_key: job.workspace.key.clone(),
                    pr_url: None,
                    pr_number: None,
                    publish_pr_state: None,
                    checked_pr_url: None,
                    publish_pr_checked_at: None,
                    checkpoint: None,
                    steered: Vec::new(),
                    retryable: false,
                    work_summary: None,
                    context_sent: Some(sent),
                    published_head: None,
                };
                harness.coordinator.record_sent_context(&lease, &completion).await?;
                harness.store.set_session_cli_id(main, &format!("cli-{tag}")).await?;
                anyhow::Ok(job)
            }
        };
        let rule_text = format!("Sign every reply {tag}.");

        let fresh = turn(first_main.id).await?;
        assert!(fresh.prompt.contains(&rule_text), "a fresh session gets the rules on its first turn: {}", fresh.prompt);
        assert!(fresh.prompt.starts_with("[rules updated]"), "{}", fresh.prompt);
        assert!(!fresh.prompt.contains("[instructions updated]"), "the persona is already the system prompt");
        assert_eq!(fresh.fresh_instructions, None);
        let stored = SentContext::from_value(store.get_session(first_main.id).await?.expect("main").context_sent.as_ref().expect("sent"));
        assert!(stored.hash(INSTRUCTIONS_BLOCK).is_some() && stored.hash(SKILLS_BLOCK).is_some());

        let resumed = turn(first_main.id).await?;
        assert!(!resumed.prompt.contains(&rule_text), "rules already in the session are not sent again");
        assert_eq!(resumed.prompt, "hello");
        assert!(resumed.fresh_instructions.as_deref().is_some_and(|text| text.contains(&rule_text)), "kept for a runner that must start fresh");
        assert_eq!(resumed.context_sent.hash(SKILLS_BLOCK), stored.hash(SKILLS_BLOCK), "the runner learns which catalog it already sent");

        store.update_rule(rule.id, owner, None, None, Some(&format!("Sign every reply {tag} with a wave.")), None).await?;
        let changed = turn(first_main.id).await?;
        assert!(changed.prompt.contains("[instructions updated]") && changed.prompt.contains("with a wave"), "{}", changed.prompt);

        store.forget_sent_context(first_main.id).await?;
        let compacted = turn(first_main.id).await?;
        assert!(compacted.prompt.contains("with a wave"), "a compacted session gets the rules again");
        assert_eq!(compacted.context_sent.hash(SKILLS_BLOCK), None, "and the skill catalog");

        let reset = store.restart_main_session(agent.id, "claude", "sonnet", "").await?;
        let after_reset = turn(reset.id).await?;
        assert!(after_reset.prompt.contains("with a wave") && !after_reset.prompt.contains("[instructions updated]"), "{}", after_reset.prompt);

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent_id).execute(pool).await?;
        sqlx::query("DELETE FROM world_runners WHERE id = $1").bind(runner).execute(pool).await?;
        sqlx::query("DELETE FROM world_rules WHERE user_id = $1").bind(owner).execute(pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(pool).await?;
        Ok(())
    }
}
