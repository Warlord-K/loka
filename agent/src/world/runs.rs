use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use loka_shared::context_budget::Prepared;
use loka_shared::context::SentContext;
use loka_shared::milestones::MilestoneStore;
use loka_shared::models::AgentTask;
use loka_shared::pr_actions;
use loka_shared::run_causes::Required;
use loka_shared::runners::RunnerUsage;
use loka_shared::world::{
    AgentSession, FollowupAdvance, NewEvent, NewRun, NewSession, ReviewerChoice, RunOutcome, SessionRun, TaskResume, WorldAgent,
};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::quests::{Finish, FollowupStep, Started};
use super::{prompts, Ctx};
use crate::cli::{self, CliOptions, CliResult, StreamEvent, Usage};
use crate::git::{github, workspace};
use crate::quest_storage::{self, Layout};

const TASK_MAX_TURNS: u32 = 200;
const REVIEW_MAX_TURNS: u32 = 60;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
const STATUS_THROTTLE: Duration = Duration::from_secs(5);
const LIVE_USAGE_EVERY: Duration = Duration::from_secs(5);

async fn record_live_usage(store: &loka_shared::world::WorldStore, run_id: Uuid, session: &AgentSession, usage: Usage) {
    let probe = CliResult { output: String::new(), session_id: None, cost_usd: 0.0, exit_code: 0, usage, checkpoint: None };
    let spent = account(session, &probe);
    let n = |v: u64| v.min(i64::MAX as u64) as i64;
    match store
        .record_live_usage(run_id, spent.cost_usd, n(spent.usage.input_tokens), n(spent.usage.cached_input_tokens), n(spent.usage.output_tokens))
        .await
    {
        Ok(stopped) if !stopped.is_empty() => info!(run = %run_id, stopped = stopped.runs.len(), "Budget reached, stopped running work"),
        Ok(_) => {}
        Err(error) => warn!(run = %run_id, %error, "Could not record live usage"),
    }
}

pub struct Accounted {
    pub usage: Usage,
    pub baseline: Value,
    pub cost_usd: f64,
}
pub fn account(session: &AgentSession, result: &CliResult) -> Accounted {
    let u = result.usage;
    let reported = loka_shared::run_recording::Reported {
        cost_usd: result.cost_usd,
        usage: RunnerUsage { input_tokens: u.input_tokens, cached_input_tokens: u.cached_input_tokens, output_tokens: u.output_tokens, context_tokens: 0 },
    };
    let billed = loka_shared::run_recording::account(&session.backend, &session.model, &session.usage_baseline, &reported);
    Accounted {
        usage: Usage {
            input_tokens: billed.input_tokens as u64,
            cached_input_tokens: billed.cached_input_tokens as u64,
            output_tokens: billed.output_tokens as u64,
            context_tokens: u.context_tokens,
        },
        baseline: billed.baseline,
        cost_usd: billed.cost_usd,
    }
}

async fn work_rules(ctx: &Ctx, agent_id: Uuid) -> String {
    let agent = ctx.store.get_agent(agent_id).await.ok().flatten();
    let owner = agent.as_ref().and_then(|a| a.owner_user_id);
    let teams = prompts::teams(&ctx.store.teams_of_agent(agent_id).await.unwrap_or_default(), agent_id, "work");
    match ctx.store.rules_for(owner, agent.and_then(|a| a.org_id)).await {
        Ok(rules) => [prompts::rules(&rules, "work", &ctx.owner), teams].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n"),
        Err(e) => {
            warn!(error = %e, "Could not load rules");
            String::new()
        }
    }
}

fn with_rules(prompt: String, rules: &str) -> String {
    if rules.is_empty() {
        prompt
    } else {
        format!("{prompt}\n\n{rules}")
    }
}

pub fn context_tokens(backend: &str, usage: &Usage) -> i64 {
    loka_shared::context::measured(backend, usage.context_tokens, usage.input_tokens, usage.cached_input_tokens)
}

fn session_id_after_run<'a>(
    backend: &str,
    fork: bool,
    result: Option<&'a str>,
    source: Option<&'a str>,
) -> Option<&'a str> {
    result.or_else(|| (!(backend == "cursor" && fork)).then_some(source).flatten())
}

pub async fn start_task(
    ctx: &Ctx,
    agent: &WorldAgent,
    main: &AgentSession,
    task_id: Uuid,
    start_brief: super::quests::StartBrief<'_>,
    review: bool,
    reviewer: Option<&ReviewerChoice>,
) -> Result<Option<Started>> {
    let resolved_brief = start_brief.text();
    let brief = resolved_brief.as_deref();
    let continuation = start_brief.continuation();
    let public_brief = if continuation.is_some() { None } else { brief };
    let task = ctx
        .db
        .get_task(task_id)
        .await?
        .ok_or_else(|| anyhow!("task {task_id} not found"))?;
    let resume = match continuation {
        Some(item) => ctx
            .store
            .resume_clarification_task_session(agent.id, task.id, &agent.name, brief, item)
            .await?,
        None => ctx.store.resume_task_session(agent.id, task.id, &agent.name, brief).await?,
    };
    if matches!(&resume, TaskResume::Parked) {
        return Ok(None);
    }
    if let Some(problem) = resume.problem() {
        return Err(anyhow!(problem));
    }
    if let TaskResume::Resumed { session_id, run_id } = resume {
        if continuation.is_some() {
        } else {
            ctx.db.update_status(task.id, "in_progress").await?;
        }
        ctx.store.announce_resumed(agent.id, task.id, session_id, run_id, public_brief).await?;
        info!(agent = %agent.slug, task = %task.id, "Task session resumed");
        return Ok(Some(Started { resumed: true, session_id, run_id }));
    }
    let layout = Layout::local(&ctx.config.workspaces_dir);
    if let Err(short) = quest_storage::make_room(&layout, &ctx.config.workspaces_dir).await {
        return Err(anyhow!(loka_shared::run_recording::no_room_to_start_message(short.free, short.freed)));
    }
    let options = ctx.store.quest_options(task.id).await?;
    let mut dirs = Vec::new();
    let opening_attempts = if options.attempts > 1 { 1 } else { options.attempts.max(1) };
    for attempt in 1..=opening_attempts {
        dirs.push(prepare_workspace(ctx, &task, attempt).await?.to_string_lossy().to_string());
    }
    let rules = work_rules(ctx, agent.id).await;
    let opened = super::quests::open_sessions(
        &ctx.store,
        super::quests::QuestStart { agent, main, task: &task, brief, review, reviewer, rules: &rules, options: &options, world_tools: false },
        &dirs,
        continuation,
    )
    .await?;
    let (session, run) = opened.first().ok_or_else(|| anyhow!("no task session opened"))?;
    if continuation.is_some() {
    } else {
        ctx.db.update_status(task.id, "in_progress").await?;
    }
    ctx.store
        .emit(
            NewEvent::new("task.started")
                .agent(agent.id)
                .session(session.id)
                .run(run.id)
                .task(Some(task.id))
                .payload(super::quests::started_payload(&task, &options, options.attempts.max(1) as usize, run.id)),
            None,
        )
        .await?;
    if opened.iter().any(|(session, _)| session.racing()) {
        super::quests::label_attempts(&ctx.store, &task, &opened).await?;
    }
    ctx.store
        .post_quest_started(agent.id, task.id, session.id, &task.title, public_brief.filter(|b| !b.trim().is_empty()))
        .await?;
    info!(agent = %agent.slug, task = %task.id, "Task session queued");
    Ok(Some(Started { resumed: false, session_id: session.id, run_id: run.id }))
}

async fn prepare_workspace(ctx: &Ctx, task: &AgentTask, attempt: i16) -> Result<PathBuf> {
    let root = ctx.config.workspaces_dir.join(task.id.to_string());
    match task.repo.as_deref() {
        Some(repo) => {
            let base = base_branch(ctx, task);
            let repo_path =
                workspace::ensure_cloned_at(&ctx.config.repos_dir, repo, &ctx.config.github_token, Some(&base)).await?;
            let name = repo.rsplit('/').next().unwrap_or(repo);
            let path = root.join(super::quests::attempt_dir(name, attempt));
            tokio::fs::create_dir_all(&root).await?;
            let branch = match workspace::current_branch(&path).await {
                Ok(branch) => branch,
                Err(_) => workspace::unused_branch_name(&repo_path, &task.agent_branch_name()).await,
            };
            workspace::create_worktree(&repo_path, &path, &branch, &base).await?;
            workspace::pin_signing(&path).await;
            Ok(path)
        }
        None => {
            let path = root.join(super::quests::attempt_dir("scratch", attempt));
            tokio::fs::create_dir_all(&path).await?;
            Ok(path)
        }
    }
}

fn base_branch(ctx: &Ctx, task: &AgentTask) -> String {
    if let Some(b) = task.target_branch.as_deref() {
        return b.to_string();
    }
    task.repo
        .as_deref()
        .and_then(|r| ctx.config.repos.iter().find(|c| c.repo == r))
        .map(|c| c.branch.clone())
        .unwrap_or_else(|| "main".to_string())
}

pub async fn execute(ctx: &Ctx, mut run: SessionRun) -> Result<()> {
    if run.kind == pr_actions::KIND {
        return pr_action(ctx, run).await;
    }
    let mut session = ctx
        .store
        .get_session(run.session_id)
        .await?
        .ok_or_else(|| anyhow!("session {} missing", run.session_id))?;
    let agent = ctx
        .store
        .get_agent(run.agent_id)
        .await?
        .ok_or_else(|| anyhow!("agent {} missing", run.agent_id))?;
    let room_answer = loka_shared::world::room_answer_aside(session.asked_by.as_ref());
    let working_dir = if room_answer {
        crate::cli::room_answer_dir(session.id)?
    } else {
        PathBuf::from(
            session
                .working_dir
                .clone()
                .unwrap_or_else(|| super::agent_home(ctx, &agent.slug).to_string_lossy().to_string()),
        )
    };
    if !tokio::fs::metadata(&working_dir).await.is_ok_and(|metadata| metadata.is_dir()) {
        let error = format!("the saved quest folder is missing: {}", working_dir.display());
        return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
    }
    let layout = Layout::local(&ctx.config.workspaces_dir);
    if loka_shared::runners::WORK_RUN_KINDS.contains(&run.kind.as_str()) {
        if let Err(short) = quest_storage::make_room(&layout, &working_dir).await {
            let error = format!("{}{}", loka_shared::run_recording::no_room_message(short.free), loka_shared::run_recording::cleared_note(short.freed));
            return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
        }
    }
    let _active = quest_storage::activate(&layout, run.id, layout.quest_of(&working_dir));
    if let Err(error) = ctx.store.release_space_wait(&run).await {
        warn!(run = %run.id, %error, "Could not clear the disk wait of a run that is starting");
    }
    let prepared = prepare_request(ctx, &mut run, &mut session).await?;
    if matches!(run.kind.as_str(), "task" | "followup") {
        super::quests::append_single_prework_policy(&ctx.store, session.id, run.id, &mut run.prompt).await?;
    }

    let (cli_sid, resume, fork) = match run.fork_from {
        Some(parent_id) => {
            let parent = ctx
                .store
                .get_session(parent_id)
                .await?
                .ok_or_else(|| anyhow!("parent session {parent_id} missing"))?;
            let sid = parent
                .cli_session_id
                .ok_or_else(|| anyhow!("parent session has no CLI session yet"))?;
            if let Some(baseline) = ctx.store.take_fork_baseline(session.id, parent_id).await? {
                session.usage_baseline = baseline;
            }
            (Some(sid), true, true)
        }
        None => match session.cli_session_id.clone() {
            Some(sid) => (Some(sid), true, false),
            None if run.kind == "quick" && ctx.store.quick_fork_source(&session).await?.is_some() => {
                let (parent_id, sid) = ctx.store.quick_fork_source(&session).await?.ok_or_else(|| anyhow!("main session has no CLI session yet"))?;
                if let Some(baseline) = ctx.store.take_fork_baseline(session.id, parent_id).await? {
                    session.usage_baseline = baseline;
                }
                (Some(sid), true, true)
            }
            None if session.backend == "claude" => (Some(Uuid::new_v4().to_string()), false, false),
            None => (None, false, false),
        },
    };

    let aside = run.kind == "aside";
    let quick = run.kind == "quick";
    let has_environment = session.role == "task" && matches!(run.kind.as_str(), "task" | "followup") || quick;
    let granted_environment = match ctx.store.keys_for_agent(agent.id).await {
        Ok(keys) => match keys.vault_refs.keys().next().filter(|_| has_environment) {
            Some(name) => {
                let error = format!("Vault reference for {name} could not be read: vault references are read only on their owner's own Mac, and this agent runs in the cloud. Move the agent to your Mac or store the value itself.");
                return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
            }
            None => keys.values,
        },
        Err(error) => {
            return fail(ctx, &run, &session, "failed", "run.failed", &loka_shared::cli_errors::chain(&error)).await
        }
    };
    let environment = if has_environment {
        match ctx.store.without_mcp_secrets(agent.id, granted_environment.clone()).await {
            Ok(environment) => environment,
            Err(error) => {
                return fail(ctx, &run, &session, "failed", "run.failed", &loka_shared::cli_errors::chain(&error)).await
            }
        }
    } else {
        Default::default()
    };
    let provider_keys = match ctx.store.provider_keys_for_agent(agent.id).await {
        Ok(keys) => keys,
        Err(error) => {
            return fail(ctx, &run, &session, "failed", "run.failed", &loka_shared::cli_errors::chain(&error)).await
        }
    };
    let redactor = cli::host_redactor().extended(granted_environment.values().chain(provider_keys.values()));
    let kit = match run.kind.as_str() {
        "aside" => Ok(None),
        "review" => super::kit::for_review(ctx, agent.id).await.map(Some),
        _ => super::kit::for_agent(ctx, agent.id).await.map(Some),
    };
    let kit = match kit {
        Ok(kit) => kit,
        Err(error) => return fail_with(ctx, &run, &session, &error, &redactor).await,
    };
    let omp = match ctx.store.resolved_omp_config(agent.id, &session.backend).await {
        Ok(omp) => omp,
        Err(error) => return fail_with(ctx, &run, &session, &error, &redactor).await,
    };
    let mut sent = SentContext::carried(session.context_sent.as_ref(), loka_shared::context::resumes_same_session(resume, fork, false));
    let index = kit.as_ref().map(|k| k.skills_index.as_str()).unwrap_or_default();
    let mut prompt = sent.append(&run.prompt, loka_shared::context::SKILLS_BLOCK, index);
    if !environment.is_empty() {
        prompt.push_str("\n\nEnvironment keys granted to you for this task: ");
        prompt.push_str(&environment.keys().cloned().collect::<Vec<_>>().join(", "));
        prompt.push_str(". Use them through the process environment. Do not print their values or store them in the repository.");
    }
    if !aside {
        let host: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
        if let Some(note) = crate::key_proxy::prompt_note(
            &session.backend,
            provider_keys.keys().chain(environment.keys()).map(String::as_str).chain(host.iter().map(String::as_str)),
        ) {
            prompt.push_str("\n\n");
            prompt.push_str(note);
        }
        if let Some(note) = crate::cmd_hub::prompt_note(&session.backend, &environment) {
            prompt.push_str("\n\n");
            prompt.push_str(note);
        }
    }
    let mut environment = environment;
    if matches!(run.kind.as_str(), "task" | "followup" | "review" | "direct") {
        if let Ok(rules) = ctx.store.rules_for(agent.owner_user_id, agent.org_id).await {
            let approved = loka_shared::approvals::from_rules(&rules);
            if !approved.is_empty() {
                environment.insert(loka_shared::approvals::ENV.to_string(), serde_json::to_string(&approved).unwrap_or_default());
            }
        }
    }
    if matches!(run.kind.as_str(), "task" | "followup" | "review") && loka_shared::command_rules::enforced_for(&session.backend) {
        match ctx.store.command_policy_for_agent(agent.id).await {
            Ok(policy) if !policy.is_empty() => {
                if let Some(note) = crate::cmd_hub::rules_note(&session.backend) {
                    prompt.push_str("\n\n");
                    prompt.push_str(note);
                }
                environment.insert(loka_shared::command_rules::ENV.to_string(), serde_json::to_string(&policy).unwrap_or_default());
            }
            Ok(_) => {}
            Err(error) => return fail(ctx, &run, &session, "failed", "run.failed", &loka_shared::cli_errors::chain(&error)).await,
        }
    }
    if room_answer {
        environment.insert(crate::cli::ROOM_ANSWER_ENV.into(), "1".into());
    }
    let max_turns = match run.kind.as_str() {
        "review" => REVIEW_MAX_TURNS,
        "aside" => 8,
        "quick" => loka_shared::quick::MAX_TURNS,
        _ => TASK_MAX_TURNS,
    };
    let (event_tx, activity) = spawn_activity(ctx.store.clone(), run.clone(), redactor.clone());
    let pids = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let event_tx = record_pids(event_tx, pids.clone());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<String>();
    let stop_watch = spawn_stop_watch(ctx, run.id, stop_tx);
    let mcp_has_secrets = kit.as_ref().is_some_and(|kit| kit.has_secret_refs);
    let owner_aside = aside && !loka_shared::world::toolless_aside(session.asked_by.as_ref());
    let delivery = super::kit::tool_delivery(&session.backend, &session.model, &environment).await;
    let servers = if owner_aside {
        super::kit::world_server_for(agent.id, None, Some(session.id), delivery)
    } else if quick {
        let mut servers = kit.map(|kit| kit.servers).unwrap_or_default();
        servers.extend(super::kit::world_server_for(agent.id, Some(session.id), None, delivery));
        servers
    } else {
        kit.map(|kit| kit.servers).unwrap_or_default()
    };
    let system_prompt = if quick {
        let rules = match ctx.store.rules_for(agent.owner_user_id, agent.org_id).await {
            Ok(rules) => rules.into_iter().filter(|r| r.scope == "talk").collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        let rules = [prompts::rules(&rules, "talk", &ctx.owner), work_rules(ctx, agent.id).await]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(prompts::quick_system(
            &agent,
            &loka_shared::quick::asker_name(&session),
            loka_shared::quick::thread_root(&session).is_some(),
            &rules,
        ))
    } else {
        None
    };
    let wired = match super::kit::wire(servers, agent.owner_user_id, agent.id, &working_dir, delivery).await {
        Ok(wired) => wired,
        Err(error) => return fail_with(ctx, &run, &session, &error, &redactor).await,
    };
    let mcp_config_json = wired.mcp_json.clone();
    let quest = match run.task_id {
        Some(id) if matches!(run.kind.as_str(), "task" | "followup" | "review") => ctx.store.quest_options(id).await.ok(),
        _ => None,
    };
    let effort = quest
        .as_ref()
        .filter(|_| run.kind != "review")
        .and_then(|q| q.effort_for(&session.backend));
    let fast = quest.as_ref().is_some_and(|q| q.fast_for(&session.backend));
    let answer_payloads = ctx.store.run_answer_payloads(run.id).await?;
    let upload_home = super::agent_home(ctx, &agent.slug).join(run.id.to_string());
    let attached = super::prompts::attached_ids(&answer_payloads);
    let (image_paths, files_note) = super::turn::materialize_attached_uploads(ctx, &upload_home, attached).await;
    if let Some(note) = files_note {
        prompt = format!("{prompt}\n\n{note}");
    }
    let mut opts = CliOptions {
        working_dir: &working_dir,
        prompt: &prompt,
        system_prompt: system_prompt.as_deref(),
        allowed_tools: None,
        max_turns,
        model: &session.model,
        mcp_config_json,
        session_id: cli_sid.as_deref(),
        resume,
        event_tx: Some(event_tx),
        image_paths,
        fork,
        lean: aside,
        effort: effort.as_deref(),
        fast,
        environment: environment.clone(),
        mcp_has_secrets,
        sensitive: !granted_environment.is_empty() || !provider_keys.is_empty() || mcp_has_secrets,
        rewind_to: None,
        approval: if loka_shared::quests::is_best_of_prework(session.asked_by.as_ref()) {
            cli::Approval::ReadOnly
        } else if aside {
            cli::Approval::Full
        } else {
            cli::Approval::parse(Some(&agent.approval_mode))
        },
        steer: Default::default(),
        omp_models: omp.map(|omp| omp.models_yml),
        provider_keys: provider_keys.clone(),
        readable_files: Vec::new(),
    };
    let steering = (matches!(run.kind.as_str(), "task" | "followup")
        && session.role == "task"
        && cli::steerable(&session.backend))
    .then(|| {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        opts.steer = cli::SteerSlot::new(receiver);
        spawn_steering(ctx, run.clone(), sender)
    });
    let heartbeat = {
        let store = ctx.store.clone();
        let id = run.id;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT_EVERY).await;
                let _ = store.heartbeat_run(id).await;
            }
        })
    };
    let started = Instant::now();
    let mut stopped_by: Option<String> = None;
    let outcome = tokio::select! {
        outcome = tokio::time::timeout(
            Duration::from_secs(run.timeout_secs.max(60) as u64),
            cli::record_spawns(wired.pids.clone(), cli::run_compacting(&session.backend, opts, prepared.compact_first())),
        ) => outcome,
        Ok(by) = stop_rx => {
            stopped_by = Some(by);
            let roots = std::mem::take(&mut *pids.lock().unwrap());
            cli::kill_process_trees(roots).await;
            Ok(Err(anyhow!("stopped")))
        }
    };
    stop_watch.abort();
    heartbeat.abort();
    if let Some(steering) = steering {
        steering.abort();
    }
    let mut activity = activity;
    if tokio::time::timeout(Duration::from_secs(5), &mut activity)
        .await
        .is_err()
    {
        warn!(run = %run.id, "Activity consumer did not drain");
        activity.abort();
        let _ = activity.await;
    }
    info!(run = %run.id, kind = %run.kind, secs = started.elapsed().as_secs(), "Run finished");

    let hold = hold_for_handoff(ctx, &session, &run).await;
    let ended = match outcome {
        Ok(Ok(result)) => {
            let saved_session_id = session_id_after_run(&session.backend, fork, result.session_id.as_deref(), cli_sid.as_deref()).map(str::to_string);
            let kept = Kept { cli_session_id: saved_session_id, sent: Some(sent) };
            settle_completed(ctx, &agent, session, &run, result, kept, &redactor).await
        }
        Ok(Err(_)) if stopped_by.is_some() => {
            let by = stopped_by.unwrap_or_default();
            let budget = match by == loka_shared::withdraw::STOPPED_BY {
                true => ctx.store.budget_pause(run.agent_id).await.ok().flatten(),
                false => None,
            };
            let error = match budget {
                Some(reason) => loka_shared::withdraw::budget_stop_error(&reason),
                None => format!("Stopped by {by}. Do not restart this work unless they ask."),
            };
            fail(ctx, &run, &session, "failed", "run.failed", &error).await
        }
        Ok(Err(e)) => fail_with(ctx, &run, &session, &e, &redactor).await,
        Err(_) => {
            let msg = format!("timed out after {}s", run.timeout_secs);
            fail(ctx, &run, &session, "timed_out", "run.timed_out", &msg).await
        }
    };
    release_hold(hold, &run).await;
    ended
}

struct Kept {
    cli_session_id: Option<String>,
    sent: Option<SentContext>,
}

async fn settle_completed(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: AgentSession,
    run: &SessionRun,
    result: CliResult,
    kept: Kept,
    redactor: &cli::Redactor,
) -> Result<()> {
    let output = redactor.redact(&result.output);
    let report = prompts::parse_report(&result.output).map(|report| redact_report(redactor, report));
    let reported = loka_shared::run_recording::Reported {
        cost_usd: result.cost_usd,
        usage: RunnerUsage {
            input_tokens: result.usage.input_tokens,
            cached_input_tokens: result.usage.cached_input_tokens,
            output_tokens: result.usage.output_tokens,
            context_tokens: 0,
        },
    };
    let live = ctx.store
        .complete_run(run, &output, &reported, kept.cli_session_id.as_deref(), context_tokens(&session.backend, &result.usage))
        .await?;
    if !live {
        warn!(run = %run.id, "Run finished after it was already closed, keeping the earlier outcome");
        return Ok(());
    }
    if let Some(sent) = kept.sent.as_ref() {
        ctx.store.set_sent_context(session.id, sent).await?;
    }
    if run.kind == "task" {
        let report_value = match report.as_ref() {
            Some(report) => serde_json::to_value(report).unwrap_or_else(|_| json!({ "summary": output.as_str() })),
            None => json!({ "summary": tail(&output, 1500) }),
        };
        ctx.store
            .set_session_status(session.id, "active", Some(&report_value.to_string()))
            .await?;
    }
    let session = ctx.store.get_session(session.id).await?.unwrap_or(session);
    if !prompts::contract_open(&run.kind, &session, &output) {
        ctx.store.announce_completed(run.id).await?;
    }
    let settled = after_completed(ctx, agent, &session, run, report.as_ref(), &output).await;
    let announced = ctx.store.announce_completed(run.id).await;
    settled.and(announced.map(|_| ()))
}

async fn after_completed(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    run: &SessionRun,
    report: Option<&prompts::Report>,
    output: &str,
) -> Result<()> {
    if run.kind == "followup" {
        return after_followup(ctx, agent, session, run, output, report).await;
    }
    match super::quests::prework_step(&ctx.store, agent, session, run, output).await? {
        super::quests::PreworkStep::Continue => {}
        super::quests::PreworkStep::Handled => return Ok(()),
        super::quests::PreworkStep::FanOut => return start_best_of_attempts(ctx, agent, session, run, output).await,
    }
    match run.kind.as_str() {
        "task" => after_task(ctx, agent, session, run, report, output).await,
        "review" => after_review(ctx, agent, session, run, output).await,
        "aside" => after_aside(ctx, agent, session, Ok(output)).await,
        "quick" => {
            let text = prompts::plain_dashes(&prompts::split_actions(output).0);
            let text = if text.trim().is_empty() { "Done.".to_string() } else { text };
            ctx.store.record_quick_reply(agent, session, run.id, &text, false).await.map(|_| ())
        }
        _ => Ok(()),
    }
}

async fn start_best_of_attempts(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    run: &SessionRun,
    output: &str,
) -> Result<()> {
    let Some(current) = ctx.store.get_session(session.id).await? else {
        return Ok(());
    };
    if current.status != "active" || !loka_shared::quests::is_best_of_prework(current.asked_by.as_ref()) {
        return Ok(());
    }
    let task_id = current.task_id.ok_or_else(|| anyhow!("prework session has no quest"))?;
    let task = ctx.db.get_task(task_id).await?.ok_or_else(|| anyhow!("task {task_id} not found"))?;
    if task.status != "in_progress" || task.assigned_agent_id != Some(agent.id) {
        return Ok(());
    }
    let current_options = ctx.store.quest_options(task_id).await?;
    let options = super::quests::best_of_options(&current, &current_options);
    let main_id = current.parent_session_id.ok_or_else(|| anyhow!("prework session has no main session"))?;
    let main = ctx.store.get_session(main_id).await?.ok_or_else(|| anyhow!("main session {main_id} not found"))?;
    let first = current.working_dir.clone().ok_or_else(|| anyhow!("prework session has no workspace"))?;
    let brief = current.asked_by.as_ref().and_then(|value| value.get("brief")).and_then(Value::as_str);
    let mut dirs = vec![first];
    for attempt in 2..=options.attempts.max(1) {
        match prepare_workspace(ctx, &task, attempt).await {
            Ok(dir) => dirs.push(dir.to_string_lossy().to_string()),
            Err(error) => {
                ctx.store
                    .fail_best_of_prework(agent.id, task_id, current.id, run.id, &loka_shared::cli_errors::chain(&error))
                    .await?;
                return Ok(());
            }
        }
    }
    let rules = work_rules(ctx, agent.id).await;
    let opened = super::quests::fan_out_sessions(
        &ctx.store,
        super::quests::QuestStart {
            agent,
            main: &main,
            task: &task,
            brief,
            review: false,
            reviewer: None,
            rules: &rules,
            options: &options,
            world_tools: false,
        },
        &dirs,
        &current,
        run,
        output,
    )
    .await?;
    super::quests::label_attempts(&ctx.store, &task, &opened).await
}

async fn fail_with(ctx: &Ctx, run: &SessionRun, session: &AgentSession, error: &anyhow::Error, redactor: &cli::Redactor) -> Result<()> {
    let text = redactor.redact(&loka_shared::cli_errors::chain(error));
    fail(ctx, run, session, "failed", "run.failed", &text).await
}

async fn fail(
    ctx: &Ctx,
    run: &SessionRun,
    session: &AgentSession,
    status: &str,
    event: &str,
    error: &str,
) -> Result<()> {
    warn!(run = %run.id, status, error, "Run did not complete");
    if status == "failed" && ctx.store.requeue_for_space(run, error).await? {
        info!(run = %run.id, kind = %run.kind, "Run refused for lack of disk space goes back to the queue to wait for room");
        return Ok(());
    }
    let finish = ctx.store
        .finish_run(
            run.id,
            RunOutcome {
                status,
                output: None,
                error: Some(error),
                cost_usd: 0.0,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
            },
        );
    if !finish.await? {
        warn!(run = %run.id, "Run already closed, skipping failure handling");
        return Ok(());
    }
    handle_failure(ctx, run, session, event, error).await
}

async fn pr_action(ctx: &Ctx, run: SessionRun) -> Result<()> {
    pr_action_with(ctx, run, &crate::pr_actions::Gh).await
}

async fn pr_action_with<G: crate::pr_actions::GitHub>(ctx: &Ctx, run: SessionRun, github: &G) -> Result<()> {
    let outcome = match ctx.store.pr_action_of_run(run.id).await? {
        None => pr_actions::Outcome::failed("the scheduled action was deleted before it ran"),
        Some(schedule) => match super::quests::pr_action_job(&ctx.store, &schedule).await {
            Ok(job) => crate::pr_actions::perform(github, &job).await,
            Err(error) => pr_actions::Outcome::failed(format!("could not check the action: {}", loka_shared::cli_errors::chain(&error))),
        },
    };
    let output = serde_json::to_string(&outcome)?;
    let finished = ctx
        .store
        .finish_run(
            run.id,
            RunOutcome { status: "completed", output: Some(&output), error: None, cost_usd: 0.0, input_tokens: 0, cached_input_tokens: 0, output_tokens: 0 },
        )
        .await?;
    if !finished {
        return Ok(());
    }
    ctx.store.announce_completed(run.id).await?;
    let Some(spec) = ctx.store.settle_pr_action(run.id, &outcome).await? else {
        return Ok(());
    };
    info!(run = %run.id, status = ?outcome.status, reason = %outcome.reason, "Pull request action settled");
    let Some(task_id) = run.task_id.filter(|_| outcome.status == pr_actions::OutcomeStatus::Ran) else {
        return Ok(());
    };
    let task = ctx.db.get_task(task_id).await?;
    let merged_url = task
        .as_ref()
        .and_then(|task| pr_actions::merged_pr_url(&spec, &outcome, task.pr_url.as_deref(), task.pr_number));
    match (merged_url, ctx.store.newest_task_session(task_id).await?) {
        (Some(url), Some(session)) => super::watch::settle_merged(&ctx.store, &ctx.db, &session, url).await,
        _ => {
            MilestoneStore::new(ctx.store.pool().clone()).check_now(task_id).await?;
            Ok(())
        }
    }
}

pub async fn fail_stale(ctx: &Ctx, run: &SessionRun, error: &str) -> Result<()> {
    let session = ctx
        .store
        .get_session(run.session_id)
        .await?
        .ok_or_else(|| anyhow!("session {} missing", run.session_id))?;
    let hold = hold_for_handoff(ctx, &session, run).await;
    let ended = handle_failure(ctx, run, &session, "run.failed", error).await;
    release_hold(hold, run).await;
    ended
}

async fn hold_for_handoff(ctx: &Ctx, session: &AgentSession, run: &SessionRun) -> Option<loka_shared::world::FolderHold> {
    if !matches!(run.kind.as_str(), "task" | "followup" | "review") {
        return None;
    }
    match ctx.store.hold_folder(session).await {
        Ok(hold) => hold,
        Err(error) => {
            warn!(run = %run.id, %error, "Could not hold the quest folder while handing the work on");
            None
        }
    }
}

async fn release_hold(hold: Option<loka_shared::world::FolderHold>, run: &SessionRun) {
    if let Some(hold) = hold {
        if let Err(error) = hold.release().await {
            warn!(run = %run.id, %error, "Could not release the quest folder after handing the work on");
        }
    }
}

pub(super) async fn prepare_request(ctx: &Ctx, run: &mut SessionRun, session: &mut AgentSession) -> Result<Prepared> {
    let prepared = ctx.store.prepare_request(run.id).await.unwrap_or_else(|error| {
        warn!(run = %run.id, error = %loka_shared::cli_errors::chain(&error), "Could not check the context budget, sending the request as it is");
        Prepared::Send
    });
    if let Prepared::Checkpoint { prompt } = &prepared {
        run.prompt = prompt.clone();
    }
    if prepared != Prepared::Send {
        if let Some(fresh) = ctx.store.get_session(session.id).await? {
            *session = fresh;
        }
    }
    Ok(prepared)
}

async fn handle_failure(
    ctx: &Ctx,
    run: &SessionRun,
    session: &AgentSession,
    event: &str,
    error: &str,
) -> Result<()> {
    if run.kind == pr_actions::KIND {
        ctx.store.settle_pr_action(run.id, &pr_actions::Outcome::failed(loka_shared::cli_errors::describe(error))).await?;
        return Ok(());
    }
    let diagnosis = if matches!(run.kind.as_str(), "task" | "followup" | "review") {
        let diagnosis = ctx
            .store
            .record_run_end(run.id, &run.kind, Some(error), "", event == "run.timed_out")
            .await?;
        if ctx.store.auto_retry(run.id, &diagnosis).await?.is_some() {
            return Ok(());
        }
        Some(diagnosis)
    } else {
        None
    };
    if diagnosis.is_none() && matches!(ctx.store.start_over(run.id, error).await?, loka_shared::context::StartOver::Retried(_)) {
        return Ok(());
    }
    let error = &loka_shared::cli_errors::describe_run(error, &run.kind);
    let payload = || {
        let mut payload = loka_shared::cli_errors::failure_payload(run.task_id, error, session.id, &run.kind);
        if let Some(diagnosis) = &diagnosis {
            diagnosis.merge_into(&mut payload);
        }
        payload
    };
    let task_failed = matches!(run.kind.as_str(), "task" | "publish")
        || run.kind == "followup" && session.role == "task" && session.status == "active";
    if !task_failed || session.racing() {
        ctx.store
            .fail_queued_followups(session.id, &format!("Parent run failed: {error}"))
            .await?;
    }
    if run.kind == "aside" {
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        return after_aside(ctx, &agent, session, Err(error)).await;
    }
    if run.kind == "quick" {
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        ctx.store.record_quick_reply(&agent, session, run.id, &format!("I could not answer that: {error}"), true).await?;
        return Ok(());
    }
    if run.kind == "review"
        && loka_shared::quests::compare_candidates(session).is_none()
        && prompts::failed_nudge_settles(ctx.store.run_nudge(run.id).await?, error)
    {
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        let manual = session
            .asked_by
            .as_ref()
            .and_then(|asked_by| asked_by.get("manual"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output = prompts::failed_nudge_output(error);
        return settle_review(ctx, &agent, session, run, &output, manual).await;
    }
    if run.kind == "review" {
        ctx.store.set_session_status(session.id, "failed", Some(error)).await?;
        if let Some(ids) = loka_shared::quests::compare_candidates(session) {
            let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
            return after_compare(ctx, &agent, session, &ids, None).await;
        }
        let manual = session
            .asked_by
            .as_ref()
            .and_then(|asked_by| asked_by.get("manual"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !manual {
            if let Some(parent) = session.parent_session_id {
                if let Some(task_session) = ctx.store.get_session(parent).await? {
                    let agent = ctx
                        .store
                        .get_agent(run.agent_id)
                        .await?
                        .ok_or_else(|| anyhow!("agent missing"))?;
                    match merged_pr(ctx, &task_session).await? {
                        Some((url, false)) => return super::watch::settle_merged(&ctx.store, &ctx.db, &task_session, &url).await,
                        Some((_, true)) => {
                            let verdict = json!({ "verdict": prompts::NO_VERDICT, "summary": error });
                            return finalize(ctx, &agent, &task_session, Some(verdict)).await;
                        }
                        None => {}
                    }
                    let stopped = diagnosis.as_ref().is_some_and(|d| d.cause == loka_shared::run_causes::Cause::Stopped);
                    let published = match task_session.task_id {
                        Some(task_id) => ctx.db.get_task(task_id).await?.is_some_and(|task| task.repo.is_some() && task.pr_url.is_some()),
                        None => false,
                    };
                    if stopped && published {
                        let verdict = json!({ "verdict": prompts::NO_VERDICT, "summary": error });
                        return finalize(ctx, &agent, &task_session, Some(verdict)).await;
                    }
                    let verdict = json!({ "verdict": "review failed", "summary": error });
                    return finalize(ctx, &agent, &task_session, Some(verdict)).await;
                }
            }
        }
    }
    if task_failed && session.racing() {
        ctx.store.set_session_status(session.id, "failed", Some(error)).await?;
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        let outcome = ctx.store.finish_attempt(session.id, false).await?;
        let last = outcome == loka_shared::quests::AttemptsOutcome::NoneFinished;
        let ev = NewEvent::new(event).agent(run.agent_id).session(session.id).run(run.id).task(run.task_id).payload(payload());
        ctx.store.emit(ev, last.then_some((run.agent_id, event))).await?;
        return match outcome {
            loka_shared::quests::AttemptsOutcome::NoneFinished => {
                let task_id = run.task_id.ok_or_else(|| anyhow!("attempt without a quest"))?;
                super::quests::settle(&ctx.store, &agent, task_id, session.id, "No attempt finished").await?;
                ctx.db.set_error(task_id, &format!("Every attempt failed. Last error: {error}")).await?;
                Ok(())
            }
            loka_shared::quests::AttemptsOutcome::Winner(id) => {
                let task_id = run.task_id.ok_or_else(|| anyhow!("attempt without a quest"))?;
                let winner = super::quests::settle(&ctx.store, &agent, task_id, id, "The only attempt that finished").await?;
                finalize(ctx, &agent, &winner, None).await
            }
            loka_shared::quests::AttemptsOutcome::Compare(ids) => {
                let task = ctx.db.get_task(run.task_id.unwrap_or_default()).await?.ok_or_else(|| anyhow!("task missing"))?;
                let candidates = super::quests::candidates(&ctx.store, &ids).await?;
                let rules = work_rules(ctx, agent.id).await;
                super::quests::queue_compare(&ctx.store, &agent, &task, &base_branch(ctx, &task), &rules, &candidates).await?;
                Ok(())
            }
            _ => Ok(()),
        };
    }
    let mut stale_task_failure = false;
    if let (Some(task_id), true) = (run.task_id, task_failed) {
        let failed = ctx.store.fail_task_session(task_id, session.id, error).await?;
        if failed {
        } else {
            stale_task_failure = ctx.store.task_failure_is_stale(task_id, run.id).await?;
        }
    } else if task_failed {
        ctx.store.set_session_status(session.id, "failed", Some(error)).await?;
    }
    let mut failure = payload();
    if let (true, Some(fields)) = (stale_task_failure, failure.as_object_mut()) {
        fields.insert("quiet".into(), Value::Bool(true));
    }
    let ev = NewEvent::new(event)
        .agent(run.agent_id)
        .session(session.id)
        .run(run.id)
        .task(run.task_id)
        .payload(failure);
    ctx.store
        .emit(ev, (!stale_task_failure).then_some((run.agent_id, event)))
        .await?;
    Ok(())
}

async fn after_task(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    run: &SessionRun,
    report: Option<&prompts::Report>,
    output: &str,
) -> Result<()> {
    let missing = report.and_then(|r| r.status.as_deref()).is_none();
    if missing && missing_report(ctx, session, run, output).await? {
        return Ok(());
    }
    if !ctx.store.complete_task_run(session.id, run.created_at).await? {
        return Ok(());
    }
    advance_task(ctx, agent, session, run.task_id, run.id).await
}

async fn missing_report(ctx: &Ctx, session: &AgentSession, run: &SessionRun, output: &str) -> Result<bool> {
    let diagnosis = ctx.store.record_run_end(run.id, &run.kind, None, output, false).await?;
    if diagnosis.cause == loka_shared::run_causes::Cause::NeverStarted {
        let retried = ctx.store.auto_retry(run.id, &diagnosis).await?.is_some();
        ctx.store.announce_completed(run.id).await?;
        return Ok(retried);
    }
    if ctx.store.settle_missing(run.id, session.id, Required::Report, output).await?.handled() {
        return Ok(true);
    }
    let task = match session.task_id {
        Some(id) => ctx.db.get_task(id).await?,
        None => None,
    };
    let work = match (task.as_ref().filter(|t| t.repo.is_some()), session.working_dir.as_deref()) {
        (Some(task), Some(wd)) => workspace::work_summary(Path::new(wd), &base_branch(ctx, task)).await,
        _ => None,
    };
    let report = prompts::Report::synthesized(output, work.as_deref());
    ctx.store
        .set_session_status(session.id, "active", Some(&serde_json::to_string(&report)?))
        .await?;
    Ok(false)
}

async fn advance_task(ctx: &Ctx, agent: &WorldAgent, session: &AgentSession, task_id: Option<Uuid>, source_run_id: Uuid) -> Result<()> {
    let session = &ctx.store.get_session(session.id).await?.unwrap_or_else(|| session.clone());
    let report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
    let task = match task_id {
        Some(id) => ctx.db.get_task(id).await?,
        None => None,
    };
    let step = super::quests::finish_step(&report, ctx.reviews_enabled && session.wants_review(), None);
    if session.racing() {
        return attempt_done(ctx, agent, session, !matches!(step, Finish::Fail(_))).await;
    }
    match (step, task) {
        (Finish::Review, Some(task)) => queue_review(ctx, agent, session, &task, source_run_id).await,
        _ => finalize(ctx, agent, session, None).await,
    }
}

async fn attempt_done(ctx: &Ctx, agent: &WorldAgent, session: &AgentSession, ok: bool) -> Result<()> {
    use loka_shared::quests::AttemptsOutcome;
    let task_id = session.task_id.ok_or_else(|| anyhow!("attempt without a quest"))?;
    match ctx.store.finish_attempt(session.id, ok).await? {
        AttemptsOutcome::Settled | AttemptsOutcome::Waiting => Ok(()),
        AttemptsOutcome::Winner(id) => {
            let winner = super::quests::settle(&ctx.store, agent, task_id, id, "The only attempt that finished").await?;
            finalize(ctx, agent, &winner, None).await
        }
        AttemptsOutcome::Compare(ids) => {
            let task = ctx.db.get_task(task_id).await?.ok_or_else(|| anyhow!("task missing"))?;
            let candidates = super::quests::candidates(&ctx.store, &ids).await?;
            let rules = work_rules(ctx, agent.id).await;
            super::quests::queue_compare(&ctx.store, agent, &task, &base_branch(ctx, &task), &rules, &candidates).await?;
            Ok(())
        }
        AttemptsOutcome::NoneFinished => {
            let last = super::quests::settle(&ctx.store, agent, task_id, session.id, "No attempt finished").await?;
            if ok {
                return Ok(());
            }
            finalize(ctx, agent, &last, None).await
        }
    }
}

async fn after_compare(ctx: &Ctx, agent: &WorldAgent, review: &AgentSession, ids: &[Uuid], output: Option<&str>) -> Result<()> {
    let task_id = review.task_id.ok_or_else(|| anyhow!("compare without a quest"))?;
    let candidates = super::quests::candidates(&ctx.store, ids).await?;
    let (winner, verdict) = super::quests::pick(output, &candidates);
    let why = verdict.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
    let winner = super::quests::settle(&ctx.store, agent, task_id, winner, &why).await?;
    finalize(ctx, agent, &winner, Some(verdict)).await
}

async fn queue_review(ctx: &Ctx, agent: &WorldAgent, session: &AgentSession, task: &AgentTask, source_run_id: Uuid) -> Result<()> {
    let pending = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
           SELECT 1 FROM agent_sessions s
           JOIN session_runs r ON r.session_id = s.id
           JOIN session_runs source ON source.id = $2
           WHERE s.parent_session_id = $1 AND s.role = 'review'
             AND r.created_at > source.created_at)",
    )
    .bind(session.id)
    .bind(source_run_id)
    .fetch_one(ctx.store.pool())
    .await?;
    if pending {
        return Ok(());
    }
    let (backend, model) = ctx.store.reviewer_for(agent, session).await?;
    let review = ctx
        .store
        .create_session(NewSession {
            agent_id: agent.id,
            role: "review",
            backend: &backend,
            model: &model,
            parent_session_id: Some(session.id),
            task_id: Some(task.id),
            working_dir: session.working_dir.as_deref(),
            usage_baseline: json!({}),
        })
        .await?;
    let rules = work_rules(ctx, agent.id).await;
    let mut prompt = prompts::review(task, &base_branch(ctx, task));
    let author_output = match ctx.store.last_run_id(session.id).await? {
        Some(id) => ctx.store.get_run(id).await?.and_then(|r| r.output),
        None => None,
    };
    prompt.push_str(&prompts::executor_checks(author_output.as_deref()));
    if !rules.is_empty() {
        prompt.push_str(&format!("\n\nThe author was bound by these rules. Any violation is a finding:\n{rules}"));
    }
    let run = ctx.store
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
    ctx.store
        .set_run_activity(&run, &format!("Getting \"{}\" reviewed", task.title))
        .await?;
    Ok(())
}

async fn after_aside(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    answer: std::result::Result<&str, &str>,
) -> Result<()> {
    let session = ctx.store.get_session(session.id).await?.unwrap_or_else(|| session.clone());
    let asked_by = session.asked_by.clone().unwrap_or(Value::Null);
    let thread = loka_shared::guests::asked_by_user(&asked_by);
    let question = asked_by.get("question").and_then(Value::as_str).unwrap_or("").to_string();
    let (text, status) = match answer {
        Ok(a) => (prompts::plain_dashes(&prompts::split_actions(a).0), "completed"),
        Err(e) => (format!("(could not answer: {e})"), "failed"),
    };
    ctx.store.set_session_status(session.id, status, Some(&tail(&text, 2000))).await?;
    if ctx.store.deliver_loka_answer(agent, &session, &text, status == "completed").await? {
        return Ok(());
    }
    let msg = ctx
        .store
        .insert_message(loka_shared::world::NewMessage {
            agent_id: agent.id,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent.id),
            body: &text,
            session_id: Some(session.id),
            run_id: None,
            meta: loka_shared::guests::in_thread(Value::Null, thread),
        })
        .await?;
    let asker_agent = asked_by
        .get("agent_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    let payload = loka_shared::guests::in_thread(
        json!({
            "aside_session_id": session.id,
            "agent": agent.name,
            "agent_slug": agent.slug,
            "question": question,
            "answer": text,
            "asked_by": asked_by,
            "message_id": msg.id,
        }),
        thread,
    );
    ctx.store
        .emit(
            NewEvent::new("aside.answered").agent(agent.id).session(session.id).payload(payload),
            asker_agent.map(|a| (a, "aside.answered")),
        )
        .await?;
    ctx.store.deliver_guest_answer(agent, &session, &text, status == "completed").await?;
    Ok(())
}

async fn after_followup(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    run: &SessionRun,
    output: &str,
    report: Option<&prompts::Report>,
) -> Result<()> {
    let task = match run.task_id {
        Some(id) => ctx.db.get_task(id).await?,
        None => None,
    };
    let merged_pr_url = match verified_followup_merge(session, run, task.as_ref()).await? {
        Some(url) if ctx.store.settle_merged_followup(task.as_ref().expect("verified quest").id, session.id, &url).await? => {
            Some(url)
        }
        _ => None,
    };
    if merged_pr_url.is_none() {
        match super::quests::prework_step(&ctx.store, agent, session, run, output).await? {
            super::quests::PreworkStep::Continue => {}
            super::quests::PreworkStep::Handled => return Ok(()),
            super::quests::PreworkStep::FanOut => return start_best_of_attempts(ctx, agent, session, run, output).await,
        }
    }
    if let Some(report) = report {
        ctx.store
            .set_session_summary(session.id, &serde_json::to_string(report)?)
            .await?;
    }
    let unreported = report.and_then(|r| r.status.as_deref()).is_none()
        && prompts::stored_report(session.summary.as_deref()).and_then(|r| r.status).is_none();
    let reply = prompts::plain_dashes(&prompts::clip_reply(output));
    ctx.store
        .emit(
            NewEvent::new("session.replied")
                .agent(agent.id)
                .session(session.id)
                .run(run.id)
                .task(session.task_id)
                .payload(json!({ "session_id": session.id, "task_id": session.task_id, "reply": reply })),
            Some((agent.id, "session.replied")),
        )
        .await?;
    if merged_pr_url.is_some() {
        return Ok(());
    }
    if session.role != "task" {
        return Ok(());
    }
    advance_followup(ctx, agent, session, run, output, unreported).await
}

pub async fn carry_on_withdrawn(ctx: &Ctx) -> Result<()> {
    for withdrawn in ctx.store.carry_on_candidates(None, false).await? {
        let Some(carry) = ctx.store.claim_carry_on(withdrawn, false).await? else {
            continue;
        };
        let prior = carry.prior();
        let Some(run) = ctx.store.get_run(prior).await? else {
            carry.complete().await?;
            continue;
        };
        let (Some(session), Some(agent)) = (ctx.store.get_session(run.session_id).await?, ctx.store.get_agent(run.agent_id).await?) else {
            carry.complete().await?;
            continue;
        };
        let output = run.output.clone().unwrap_or_default();
        let hold = hold_for_handoff(ctx, &session, &run).await;
        let carried = match ctx.store.get_session(run.session_id).await {
            Ok(Some(session)) => {
                let unreported = prompts::stored_report(session.summary.as_deref()).and_then(|r| r.status).is_none();
                match super::quests::prework_step(&ctx.store, &agent, &session, &run, &output).await {
                    Ok(super::quests::PreworkStep::Continue) => {
                        advance_followup(ctx, &agent, &session, &run, &output, unreported).await
                    }
                    Ok(super::quests::PreworkStep::Handled) => Ok(()),
                    Ok(super::quests::PreworkStep::FanOut) => {
                        start_best_of_attempts(ctx, &agent, &session, &run, &output).await
                    }
                    Err(error) => Err(error),
                }
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        release_hold(hold, &run).await;
        match carried {
            Ok(()) => carry.complete().await?,
            Err(error) => {
                carry.release().await?;
                warn!(run = %run.id, %error, "Could not carry on the work a withdrawn follow-up was holding back");
            }
        }
    }
    Ok(())
}

async fn advance_followup(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    run: &SessionRun,
    output: &str,
    unreported: bool,
) -> Result<()> {
    let FollowupAdvance::Advance { was_completed } = ctx.store.complete_followup(session.id).await? else {
        return Ok(());
    };
    let mut session = ctx.store.get_session(session.id).await?.unwrap_or_else(|| session.clone());
    let task = match run.task_id {
        Some(id) => ctx.db.get_task(id).await?,
        None => None,
    };
    let sync = followup_work_sync(&session, run, task.as_ref()).await;
    let nudge_available = if !was_completed && unreported {
        ctx.store.set_session_status(session.id, "active", None).await?;
        let handled = missing_report(ctx, &session, run, output).await?;
        session = ctx.store.get_session(session.id).await?.unwrap_or(session);
        handled
    } else {
        false
    };
    let report = prompts::stored_report(session.summary.as_deref()).unwrap_or_default();
    let has_repo = task.as_ref().is_some_and(|task| task.repo.is_some());
    let verdict = super::quests::review_on_record(&ctx.store, session.id, run.id, sync)
        .await?
        .map(|output| review_verdict_value(&output));
    let step = super::quests::followup_step(was_completed, &report, ctx.reviews_enabled, session.wants_review(), nudge_available, has_repo, sync, verdict.as_ref());
    if session.racing() && !matches!(step, FollowupStep::Nudge | FollowupStep::Leave) {
        return attempt_done(ctx, agent, &session, !matches!(step, FollowupStep::Fail(_) | FollowupStep::Salvage)).await;
    }
    match (step, task) {
        (FollowupStep::Nudge | FollowupStep::Leave, _) => Ok(()),
        (FollowupStep::Review, Some(task)) => queue_review(ctx, agent, &session, &task, run.id).await,
        (FollowupStep::Settle, _) => finalize(ctx, agent, &session, verdict).await,
        (FollowupStep::Review | FollowupStep::Salvage | FollowupStep::Fail(_), _) => finalize(ctx, agent, &session, None).await,
    }
}

async fn verified_followup_merge(
    session: &AgentSession,
    run: &SessionRun,
    task: Option<&AgentTask>,
) -> Result<Option<String>> {
    if session.role != "task" {
        return Ok(None);
    }
    let Some(wd) = session.working_dir.as_deref() else {
        return Ok(None);
    };
    let Some(task) = task.filter(|task| run.task_id == Some(task.id)) else {
        return Ok(None);
    };
    let Some(tracked) = task
        .pr_url
        .as_deref()
        .and_then(loka_shared::git_names::TrackedPr::parse)
    else {
        return Ok(None);
    };
    if github::tracked_pr_sync(Path::new(wd), std::time::SystemTime::from(run.created_at), tracked).await
        != loka_shared::runners::PrSync::Merged
    {
        return Ok(None);
    }
    Ok(Some(tracked.url().to_string()))
}

async fn followup_work_sync(session: &AgentSession, run: &SessionRun, task: Option<&AgentTask>) -> loka_shared::runners::PrSync {
    let (Some(wd), Some(_)) = (session.working_dir.as_deref(), task.filter(|task| run.task_id == Some(task.id))) else {
        return loka_shared::runners::PrSync::Changed;
    };
    match github::pr_sync(Path::new(wd), std::time::SystemTime::from(run.created_at)).await {
        loka_shared::runners::PrSync::Merged => loka_shared::runners::PrSync::Changed,
        sync => sync,
    }
}

async fn merged_pr(ctx: &Ctx, task_session: &AgentSession) -> Result<Option<(String, bool)>> {
    let Some(task_id) = task_session.task_id else {
        return Ok(None);
    };
    let Some(task) = ctx.db.get_task(task_id).await? else {
        return Ok(None);
    };
    let Some(url) = task.pr_url.clone() else {
        return Ok(None);
    };
    let sync = match (
        task_session.working_dir.as_deref(),
        loka_shared::git_names::TrackedPr::parse(&url),
    ) {
        (Some(wd), Some(tracked)) => Some(github::tracked_pr_sync(Path::new(wd), std::time::SystemTime::now(), tracked).await),
        _ => None,
    };
    let merged = sync == Some(loka_shared::runners::PrSync::Merged) || ctx.store.quest_merged_pr(task_id).await?.is_some();
    Ok(merged.then_some((url, sync == Some(loka_shared::runners::PrSync::Changed))))
}

async fn after_review(
    ctx: &Ctx,
    agent: &WorldAgent,
    review: &AgentSession,
    run: &SessionRun,
    output: &str,
) -> Result<()> {
    let manual = review
        .asked_by
        .as_ref()
        .and_then(|a| a.get("manual"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if prompts::contract_open("review", review, output)
        && ctx.store.settle_missing(run.id, review.id, Required::Verdict, output).await?.handled()
    {
        return Ok(());
    }
    settle_review(ctx, agent, review, run, output, manual).await
}

async fn settle_review(
    ctx: &Ctx,
    agent: &WorldAgent,
    review: &AgentSession,
    run: &SessionRun,
    output: &str,
    manual: bool,
) -> Result<()> {
    if manual {
        ctx.store.set_session_status(review.id, "completed", Some(&tail(output, 2000))).await?;
        let verdict = review_verdict_value(output);
        let requester = review
            .asked_by
            .as_ref()
            .and_then(|a| a.get("agent_id"))
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Uuid>().ok())
            .unwrap_or(agent.id);
        ctx.store
            .emit(
                NewEvent::new("review.finished")
                    .agent(agent.id)
                    .session(review.id)
                    .run(run.id)
                    .task(review.task_id)
                    .payload(json!({
                        "task_session_id": review.parent_session_id,
                        "task_id": review.task_id,
                        "review": verdict,
                        "reviewer": review.asked_by.as_ref().and_then(|a| a.get("reviewer")).cloned(),
                    })),
                Some((requester, "review.finished")),
            )
            .await?;
        return Ok(());
    }
    if let Some(ids) = loka_shared::quests::compare_candidates(review) {
        ctx.store.set_session_status(review.id, "completed", Some(&tail(output, 2000))).await?;
        return after_compare(ctx, agent, review, &ids, Some(output)).await;
    }
    let task_session_id = review
        .parent_session_id
        .ok_or_else(|| anyhow!("review session has no task session"))?;
    let task_session = ctx
        .store
        .get_session(task_session_id)
        .await?
        .ok_or_else(|| anyhow!("task session missing"))?;
    ctx.store.set_session_status(review.id, "completed", Some(&tail(output, 2000))).await?;
    let verdict = prompts::readable_verdict(output);
    let verdict_value = review_verdict_value(output);
    let wants_changes = verdict.as_ref().is_some_and(|v| v.verdict == "changes" && !v.findings.is_empty());
    if wants_changes && task_session.review_rounds < ctx.max_review_rounds {
        let round = ctx.store.bump_review_round(task_session.id).await?;
        let task = ctx
            .db
            .get_task(run.task_id.ok_or_else(|| anyhow!("review without task"))?)
            .await?
            .ok_or_else(|| anyhow!("task missing"))?;
        let findings = serde_json::to_string_pretty(&verdict_value).unwrap_or_default();
        let wd = task_session.working_dir.clone().unwrap_or_default();
        let prompt = with_rules(prompts::task(&task, &wd, Some(&findings)), &work_rules(ctx, agent.id).await);
        let run = ctx.store
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
        ctx.store
            .set_run_activity(&run, &format!("Fixing review notes on \"{}\" (round {round})", task.title))
            .await?;
        return Ok(());
    }
    finalize(ctx, agent, &task_session, Some(verdict_value)).await
}

async fn finalize(
    ctx: &Ctx,
    agent: &WorldAgent,
    task_session: &AgentSession,
    verdict: Option<Value>,
) -> Result<()> {
    let task_session = &ctx
        .store
        .get_session(task_session.id)
        .await?
        .unwrap_or_else(|| task_session.clone());
    let report: Value = task_session
        .summary
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({ "summary": task_session.summary }));
    let parsed: prompts::Report = prompts::stored_report(task_session.summary.as_deref()).unwrap_or_default();
    let diagnosis = ctx.store.latest_diagnosis(task_session.id).await?;
    let mut error = match (super::quests::finish_error(&parsed, verdict.as_ref()), &diagnosis) {
        (Some(_), Some(diagnosis)) if parsed.status.is_none() => Some(diagnosis.reason.clone()),
        (error, _) => error,
    };
    let task = match task_session.task_id {
        Some(task_id) => ctx.db.get_task(task_id).await?,
        None => None,
    };
    let mut pr_url = task.as_ref().and_then(|task| task.pr_url.clone());
    let duplicate_report = ctx
        .store
        .finished_report_unchanged(task_session.id, &report, pr_url.as_deref(), error.as_deref())
        .await?;
    let mut publish_secs = None;
    let mut open_pr_observation = None;
    if let Some(task) = task.as_ref().filter(|_| !duplicate_report) {
        if error.is_none() || parsed.synthesized {
            if let (Some(repo), Some(wd)) = (task.repo.as_deref(), task_session.working_dir.as_deref()) {
                let publishing = Instant::now();
                match open_pr(ctx, task, repo, task_session, Path::new(wd), &parsed).await {
                    Ok(PublishOutcome::Open(Some(url), _, checked_at)) => {
                        let recorded = ctx.db.get_task(task.id).await?.and_then(|task| task.pr_url);
                        if !recorded.as_deref().is_some_and(|recorded| recorded.eq_ignore_ascii_case(&url)) {
                            ctx.store.set_session_status(task_session.id, "completed", None).await?;
                            return Ok(());
                        }
                        if let Some(checked_at) = checked_at {
                            open_pr_observation = Some((url.clone(), checked_at));
                        }
                        pr_url = Some(url);
                    }
                    Ok(PublishOutcome::Open(None, _, _)) => {}
                    Ok(PublishOutcome::Merged(url, observed_at)) => {
                        if !ctx.store.settle_pr_observed(task.id, &url, true, observed_at).await? {
                            ctx.store.set_session_status(task_session.id, "completed", None).await?;
                            return Ok(());
                        }
                        pr_url = Some(url);
                    }
                    Ok(PublishOutcome::Closed(url, observed_at)) => {
                        if !ctx.store.settle_pr_observed(task.id, &url, false, observed_at).await? {
                            ctx.store.set_session_status(task_session.id, "completed", None).await?;
                            return Ok(());
                        }
                        pr_url = Some(url);
                        error = Some(loka_shared::world::PR_CLOSED_ERROR.to_string());
                    }
                    Ok(PublishOutcome::Stale) => return Ok(()),
                    Err(e) if error.is_none() => error = Some(loka_shared::cli_errors::chain(&e)),
                    Err(e) => warn!(task = %task.id, error = %e, "Could not publish the work of a run that ended without its report"),
                }
                publish_secs = Some(publishing.elapsed().as_secs());
            }
        }
        if let Some((url, observed_at)) = open_pr_observation.as_ref() {
            MilestoneStore::new(ctx.store.pool().clone())
                .rearm_open_pr(task.id, url, observed_at.to_owned())
                .await?;
        }
    }
    let changes_requested = verdict.as_ref().and_then(|v| v.get("verdict")).and_then(Value::as_str) == Some("changes");
    let reconciled = super::quests::reconcile_quest(
        &ctx.store,
        &ctx.db,
        task_session,
        super::quests::QuestEvidence::Completed {
            report: &report,
            parsed: &parsed,
            error: error.as_deref(),
            pr_url: pr_url.as_deref(),
            changes_requested,
                suppress_announcement: duplicate_report,
        },
    )
    .await?
    .completed
    .expect("completed quest evidence returns reconciliation");
    let super::quests::ReconciledQuest { task, status: task_status, watching, announce } = reconciled;
    if !announce {
        return Ok(());
    }
    let verdict_label = verdict
        .as_ref()
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str)
        .map(String::from);
    let wrap = super::quests::wrapup(&ctx.store, agent, task_session, task.as_ref(), &parsed, error.as_deref(), changes_requested, publish_secs).await;
    let diagnosis = diagnosis.filter(|_| error.is_some());
    let mut payload = json!({
        "task_id": task_session.task_id,
        "session_id": task_session.id,
        "report": report,
        "verdict": verdict_label,
        "review": verdict,
        "pr_url": pr_url,
        "status": task_status,
        "error": error,
        "pace": wrap.pace,
        "timing": wrap.timing,
    });
    wrap.add_decisions(&mut payload);
    if let Some(diagnosis) = &diagnosis {
        diagnosis.merge_into(&mut payload);
    }
    ctx.store
        .emit(
            NewEvent::new("task.finished")
                .agent(agent.id)
                .session(task_session.id)
                .task(task_session.task_id)
                .payload(payload),
            wrap.wake.then_some((agent.id, "task.finished")),
        )
        .await?;
    if let Some(task_id) = task_session.task_id {
        let title = ctx.db.get_task(task_id).await.ok().flatten().map(|t| t.title).unwrap_or_else(|| "A quest".into());
        let summary = report.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let main = ctx.store.main_session(agent.id).await?;
        ctx.store
            .insert_message(loka_shared::world::NewMessage {
                agent_id: agent.id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body: &prompts::plain_dashes(&summary),
                session_id: main.map(|m| m.id),
                run_id: None,
                meta: json!({
                    "kind": "quest",
                    "title": title,
                    "status": task_status,
                    "pr_url": pr_url,
                    "verdict": verdict_label,
                    "error": error,
                    "task_id": task_id,
                    "session_id": task_session.id,
                    "watching": watching,
                    "pace": wrap.pace,
                    "timing": wrap.timing,
                    "cause": diagnosis.as_ref().map(|d| d.cause.as_str()),
                    "reason": diagnosis.as_ref().map(|d| d.reason.as_str()),
                    "synthesized": parsed.synthesized,
                }),
            })
            .await?;
    }
    Ok(())
}

enum PublishOutcome {
    Open(Option<String>, bool, Option<chrono::DateTime<chrono::Utc>>),
    Merged(String, chrono::DateTime<chrono::Utc>),
    Closed(String, chrono::DateTime<chrono::Utc>),
    Stale,
}

async fn open_pr(
    ctx: &Ctx,
    task: &AgentTask,
    repo: &str,
    task_session: &AgentSession,
    wd: &Path,
    report: &prompts::Report,
) -> Result<PublishOutcome> {
    let other = ctx.store.other_folder_writer(task_session).await?;
    let outcome = publish_pr(ctx, task, repo, wd, report, other.as_deref()).await?;
    if let PublishOutcome::Open(Some(url), true, _) = &outcome {
        if let Some(head) = github::head_commit(wd).await {
            ctx.store.record_published_head(task.id, url, &head).await?;
        }
    }
    Ok(outcome)
}

async fn publish_pr(
    ctx: &Ctx,
    task: &AgentTask,
    repo: &str,
    wd: &Path,
    report: &prompts::Report,
    other_writer: Option<&str>,
) -> Result<PublishOutcome> {
    let base = base_branch(ctx, task);
    let task_branch = task.branch.clone().unwrap_or_else(|| task.agent_branch_name());
    let target = workspace::PublishTarget { repo, base: &base, onto: &base, quest_branch: &task_branch };
    let tracked_pr = task
        .pr_url
        .as_deref()
        .and_then(loka_shared::git_names::TrackedPr::parse);
    if task.pr_url.is_some() && tracked_pr.is_none() {
        anyhow::bail!("recorded pull request is invalid")
    }
    if let Some(tracked) = tracked_pr {
        if !tracked.repo().eq_ignore_ascii_case(repo) {
            anyhow::bail!("recorded pull request is outside this quest's repository")
        }
        let url = tracked.url().to_string();
        let checked_at = chrono::Utc::now();
        let state = github::pr_state(wd, tracked).await?;
        let recorded_pr_open_at = (state == "OPEN").then_some(checked_at);
        let message = report.commit_message.clone().unwrap_or_else(|| format!("fix: address review for {}", task.title));
        let committed = workspace::commit_for_publish(wd, repo, &message, other_writer).await?;
        match github::carry_past_recorded_pr(wd, &base, &task_branch, i64::from(tracked.number()), Some(&state), None).await? {
            github::RecordedPr::Open => {}
            github::RecordedPr::Final { carried: None, .. } if state == "MERGED" => {
                return Ok(PublishOutcome::Merged(url, checked_at));
            }
            github::RecordedPr::Final { carried: None, .. } => {
                return Ok(PublishOutcome::Closed(url, checked_at));
            }
            github::RecordedPr::Final { state, carried: Some(branch) } => {
                workspace::publish_commits(wd, &committed.carried(), &target, &branch).await.context("push failed")?;
                let title = report.pr_title.clone().unwrap_or_else(|| task.title.clone());
                let body = report.pr_body.clone().or_else(|| report.summary.clone()).unwrap_or_default();
                let body = github::follow_up_body(&body, i64::from(tracked.number()), &state);
                let Some(pr) = github::create_pr(wd, repo, &branch, &base, &title, &body).await? else {
                    return Ok(PublishOutcome::Open(Some(url), false, None));
                };
                let replaced = ctx
                    .db
                    .replace_current_pr(task.id, tracked.url(), tracked.number(), &pr.url, pr.number)
                    .await?;
                if replaced {
                    return Ok(PublishOutcome::Open(Some(pr.url), true, None));
                }
                let current = ctx.db.get_task(task.id).await?.and_then(|task| task.pr_url);
                let current_is_new = current
                    .as_deref()
                    .and_then(loka_shared::git_names::TrackedPr::parse)
                    .zip(loka_shared::git_names::TrackedPr::parse(&pr.url))
                    .is_some_and(|(current, created)| current.same_identity(created));
                return Ok(if current_is_new {
                    PublishOutcome::Open(Some(pr.url), false, None)
                } else {
                    PublishOutcome::Stale
                });
            }
        }
        if !workspace::ahead_of_fresh_base(wd, &base).await? {
            return Ok(PublishOutcome::Open(Some(url), false, recorded_pr_open_at));
        }
        let parent_pr = loka_agent::runner_server::parent_quest_pr(&ctx.db, task).await;
        let head = match github::open_pr_containing_head(wd, repo, &base, &task_branch, parent_pr).await? {
            github::HeadPr::Own(_, head) => head,
            github::HeadPr::Elsewhere(_) => return Ok(PublishOutcome::Open(Some(url), false, recorded_pr_open_at)),
            github::HeadPr::Extends(pr, head_ref)
                if github::extends_recorded_pr(Some(i64::from(tracked.number())), pr.number) =>
            {
                let extending = workspace::PublishTarget { quest_branch: &head_ref, ..target };
                workspace::publish_commits(wd, &committed, &extending, &head_ref).await.context("push failed")?;
                return Ok(PublishOutcome::Open(Some(url), true, recorded_pr_open_at));
            }
            github::HeadPr::Extends(..) | github::HeadPr::Missing => workspace::publish_branch(wd, Some(&task_branch)).await?,
        };
        workspace::publish_commits(wd, &committed, &target, &head).await?;
        return Ok(PublishOutcome::Open(Some(url), true, recorded_pr_open_at));
    }
    let message = report
        .commit_message
        .clone()
        .unwrap_or_else(|| format!("feat: {}", task.title));
    let committed = workspace::commit_for_publish(wd, repo, &message, other_writer).await?;
    let parent_pr = loka_agent::runner_server::parent_quest_pr(&ctx.db, task).await;
    let ahead = workspace::ahead_of_fresh_base(wd, &base).await?;
    if !ahead {
        let reported = report.reported_pr(repo).map(|(_, number)| i64::from(number));
        let checked_at = chrono::Utc::now();
        let recovered = match task.branch.as_deref() {
            Some(branch) => github::recover_pr(wd, repo, branch, reported).await?,
            None => None,
        };
        let Some(recovered) = recovered else {
            return Ok(PublishOutcome::Open(None, false, None));
        };
        ctx.db.set_pr(task.id, &recovered.info.url, recovered.info.number).await?;
        return match recovered.state.as_str() {
            "MERGED" => Ok(PublishOutcome::Merged(recovered.info.url, checked_at)),
            "CLOSED" => Ok(PublishOutcome::Closed(recovered.info.url, checked_at)),
            _ => Ok(PublishOutcome::Open(Some(recovered.info.url), false, Some(checked_at))),
        };
    }
    let found = github::open_pr_containing_head(wd, repo, &base, &task_branch, parent_pr).await?;
    match found {
        github::HeadPr::Elsewhere(_) => return Ok(PublishOutcome::Open(None, false, None)),
        github::HeadPr::Own(pr, head) => {
            workspace::publish_commits(wd, &committed, &target, &head).await.context("push failed")?;
            ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
            return Ok(PublishOutcome::Open(Some(pr.url), true, None));
        }
        github::HeadPr::Extends(pr, head_ref) => {
            let extending = workspace::PublishTarget { quest_branch: &head_ref, ..target };
            workspace::publish_commits(wd, &committed, &extending, &head_ref).await.context("push failed")?;
            ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
            return Ok(PublishOutcome::Open(Some(pr.url), true, None));
        }
        github::HeadPr::Missing => {}
    }
    if !workspace::ahead_of_fresh_base(wd, &base).await? {
        return Ok(PublishOutcome::Open(None, false, None));
    }
    let branch = workspace::publish_branch(wd, Some(&task_branch)).await?;
    if branch == base {
        return Err(anyhow!("workspace is on the base branch {base}"));
    }
    let into = github::stack_base(wd, repo, &base, &task_branch, parent_pr).await;
    let target = workspace::PublishTarget { onto: &into, ..target };
    workspace::publish_commits(wd, &committed, &target, &branch).await.context("push failed")?;
    let title = report.pr_title.clone().unwrap_or_else(|| task.title.clone());
    let body = report.pr_body.clone().or_else(|| report.summary.clone()).unwrap_or_default();
    let Some(pr) = github::create_pr(wd, repo, &branch, &into, &title, &body).await? else {
        return Ok(PublishOutcome::Open(None, false, None));
    };
    ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
    Ok(PublishOutcome::Open(Some(pr.url), true, None))
}

pub(super) fn record_pids(
    forward: cli::EventSender,
    pids: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
) -> cli::EventSender {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if let StreamEvent::Spawned { pid } = &event {
                pids.lock().unwrap().push(*pid);
            }
            let _ = forward.send(event);
        }
    });
    sender
}

pub(super) fn spawn_stop_watch(ctx: &Ctx, run_id: Uuid, stop: tokio::sync::oneshot::Sender<String>) -> tokio::task::JoinHandle<()> {
    let store = ctx.store.clone();
    let bus = ctx.bus.clone();
    tokio::spawn(async move {
        loop {
            let notified = bus.runs.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match store.stop_requested(run_id).await {
                Ok(Some(by)) => {
                    info!(run = %run_id, by = %by, "Stopping run on request");
                    let _ = stop.send(by);
                    return;
                }
                Ok(None) => {}
                Err(error) => warn!(run = %run_id, %error, "Could not check for a stop request"),
            }
            let _ = tokio::time::timeout(Duration::from_secs(20), notified).await;
        }
    })
}

fn spawn_steering(ctx: &Ctx, run: SessionRun, sender: cli::SteerSender) -> tokio::task::JoinHandle<()> {
    let store = ctx.store.clone();
    let bus = ctx.bus.clone();
    tokio::spawn(async move {
        loop {
            let notified = bus.runs.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match store.pending_steers(run.session_id).await {
                Ok(steers) => {
                    for steer in steers {
                        if !cli::deliver(&sender, steer.text, steer.interrupt).await {
                            return;
                        }
                        match store.mark_steered(steer.id, run.id).await {
                            Ok(true) => {
                                info!(run = %run.id, followup = %steer.id, interrupt = steer.interrupt, "Steered follow-up into running work");
                                let event = NewEvent::new("run.steered")
                                    .agent(run.agent_id)
                                    .task(run.task_id)
                                    .payload(json!({ "run_id": run.id, "followup_id": steer.id, "session_id": run.session_id }));
                                let _ = store.emit(event, None).await;
                            }
                            Ok(false) => {}
                            Err(error) => warn!(run = %run.id, %error, "Could not mark follow-up as steered"),
                        }
                    }
                }
                Err(error) => warn!(run = %run.id, %error, "Could not read follow-ups to steer"),
            }
            if sender.is_closed() {
                return;
            }
            let _ = tokio::time::timeout(Duration::from_secs(20), notified).await;
        }
    })
}

fn spawn_activity(
    store: loka_shared::world::WorldStore,
    run: SessionRun,
    redactor: cli::Redactor,
) -> (cli::EventSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    let activity = tokio::spawn(async move {
        let (session_id, run_id) = (run.session_id, run.id);
        let mut last = Instant::now() - STATUS_THROTTLE;
        let mut last_usage = Instant::now() - LIVE_USAGE_EVERY;
        let mut session: Option<AgentSession> = None;
        let shows_activity = loka_shared::run_recording::shows_activity(&run.kind);
        while let Some(event) = rx.recv().await {
            let event = event.redacted_clip(&redactor, loka_shared::run_recording::EVENT_MAX_CHARS);
            if let StreamEvent::Usage(usage) = &event {
                if last_usage.elapsed() >= LIVE_USAGE_EVERY {
                    last_usage = Instant::now();
                    if session.is_none() {
                        session = store.get_session(session_id).await.ok().flatten();
                    }
                    if let Some(session) = &session {
                        record_live_usage(&store, run_id, session, *usage).await;
                    }
                }
                continue;
            }
            let _ = match &event {
                StreamEvent::SessionStarted {
                    session_id: cli_session_id,
                } => {
                    if let Err(error) = store.set_session_cli_id(session_id, cli_session_id).await {
                        warn!(%error, session = %session_id, "Could not save CLI session checkpoint");
                    }
                    continue;
                }
                StreamEvent::Spawned { .. } | StreamEvent::Usage(_) => continue,
                StreamEvent::ToolUse { tool, input_summary } => store.insert_run_event(run_id, "tool", Some(tool), input_summary).await,
                StreamEvent::ToolResult { tool, output } => store.insert_run_event(run_id, "result", Some(tool), output).await,
                StreamEvent::AssistantText(text) => store.insert_run_event(run_id, "text", None, text).await,
                StreamEvent::Error(err) => store.insert_run_event(run_id, "error", None, err).await,
            };
            if let StreamEvent::ToolUse { tool, input_summary } = event {
                if !shows_activity || last.elapsed() < STATUS_THROTTLE {
                    continue;
                }
                last = Instant::now();
                let detail: String = loka_shared::progress::phrase(&tool, &input_summary)
                    .chars()
                    .take(120)
                    .collect();
                let _ = store.set_run_activity(&run, &detail).await;
            }
        }
    });
    (tx, activity)
}

fn redact_report(redactor: &cli::Redactor, mut report: prompts::Report) -> prompts::Report {
    report.summary = report.summary.map(|value| redactor.redact(&value));
    report.decisions = report.decisions.into_iter().map(|value| redactor.redact(&value)).collect();
    report.followups = report.followups.into_iter().map(|value| redactor.redact(&value)).collect();
    report.pr_title = report.pr_title.map(|value| redactor.redact(&value));
    report.pr_body = report.pr_body.map(|value| redactor.redact(&value));
    report.pr_url = report.pr_url.map(|value| redactor.redact(&value));
    report.commit_message = report.commit_message.map(|value| redactor.redact(&value));
    report
}

fn review_verdict_value(output: &str) -> Value {
    match prompts::readable_verdict(output) {
        Some(_) => extract_raw(output, "verdict").unwrap_or_default(),
        None => prompts::no_verdict(tail(output, 800)),
    }
}

fn extract_raw(output: &str, name: &str) -> Option<Value> {
    serde_json::from_str(&prompts::block_body(output, name)?.1).ok()
}

fn tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        text.to_string()
    } else {
        chars[chars.len() - max..].iter().collect()
    }
}

fn task_timeout() -> i32 {
    std::env::var("WORLD_TASK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
}

#[cfg(test)]
#[path = "contract.rs"]
mod contract;

#[cfg(test)]
mod tests {
    use super::{redact_report, review_verdict_value, session_id_after_run};

    #[test]
    fn a_review_without_a_readable_verdict_settles_as_no_verdict() {
        for output in [
            "The tests are still compiling, I'll wait.",
            "<!-- verdict {\"verdict\":\"maybe\"} verdict -->",
            super::prompts::failed_nudge_output("timed out").as_str(),
        ] {
            let value = review_verdict_value(output);
            assert_eq!(value["verdict"], super::prompts::NO_VERDICT);
            assert_eq!(value["note"], super::prompts::UNREADABLE_VERDICT_NOTE);
            let done = super::prompts::Report { status: Some("done".into()), ..Default::default() };
            assert_eq!(super::super::quests::finish_step(&done, true, Some(&value)), super::Finish::Publish);
        }
        let readable = review_verdict_value("<!-- verdict {\"verdict\":\"approve\",\"summary\":\"ok\"} verdict -->");
        assert_eq!(readable["verdict"], "approve");
        assert!(readable.get("note").is_none());
    }

    use crate::world::prompts::Report;
    use loka_shared::progress::phrase as activity_phrase;

    pub(super) async fn test_ctx() -> anyhow::Result<Option<super::Ctx>> {
        use crate::config::Config;
        use loka_shared::{db::Database, world::WorldStore};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        let db = Database::connect(&url).await?;
        let store = WorldStore::new(db.pool().clone());
        let config = Config {
            database_url: url,
            github_token: String::new(),
            repos_dir: std::env::temp_dir(),
            workspaces_dir: std::env::temp_dir(),
            backends: Default::default(),
            repos: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            signing: None,
        };
        Ok(Some(super::Ctx {
            config,
            db,
            store,
            worker: "test".into(),
            owner: "the team".into(),
            max_review_rounds: 1,
            reviews_enabled: true,
            warm: crate::world::warm::Pool::new(1),
            bus: Default::default(),
        }))
    }

    #[tokio::test]
    async fn the_local_worker_runs_a_pull_request_action_once_and_skips_a_moved_head() -> anyhow::Result<()> {
        use crate::pr_actions::tests::FakeGitHub;
        use loka_shared::pr_actions::{ActionKind, ActionSpec, NewAction};
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let head = "1111111111111111111111111111111111111111";
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("local-action-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("local-action-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_number, pr_url, branch) VALUES ($1, 'ship it', 'review', 'Acme/Shop', 7, 'https://github.com/Acme/Shop/pull/7', 'quest-branch') RETURNING id",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let rule = ctx.store.create_rule(Some(owner), None, "allow", "Close and merge reviewed PRs in Acme/Shop").await?;
        ctx.store.set_rule_repos(rule.id, owner, None, &["Acme/Shop".to_string()]).await?.expect("allow rule");
        let spec = ActionSpec { kind: ActionKind::Close, repo: "Acme/Shop".into(), number: 7, sha: head.into(), check: None };
        let queue = |spec: ActionSpec| {
            let ctx = &ctx;
            async move {
                let action = ctx
                    .store
                    .create_pr_action(NewAction { user_id: owner, agent_id: agent, task_id: task, spec: &spec, run_at: None, on_event: None, wait_days: None, note: None })
                    .await?;
                let run_id = ctx.store.queue_pr_action(&action).await?.expect("queued");
                sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1").bind(run_id).execute(ctx.db.pool()).await?;
                anyhow::Ok((action, ctx.store.get_run(run_id).await?.expect("run")))
            }
        };
        let status = |id: Uuid| {
            let ctx = &ctx;
            async move {
                let row = ctx.store.list_agent_schedules(agent).await?.into_iter().find(|s| s.id == id).expect("listed");
                anyhow::Ok(row.outcome.and_then(|o| o.get("status").and_then(serde_json::Value::as_str).map(String::from)).unwrap_or_default())
            }
        };

        let github = FakeGitHub::new("OPEN");
        let (closing, run) = queue(spec.clone()).await?;
        super::pr_action_with(&ctx, run.clone(), &github).await?;
        assert_eq!(github.acted(), vec!["close Acme/Shop#7".to_string()]);
        assert_eq!(status(closing.id).await?, "ran");
        super::pr_action_with(&ctx, run, &github).await?;
        assert_eq!(github.acted().len(), 1, "a run that already finished never acts twice");

        let moved = FakeGitHub::new("OPEN");
        moved.live.lock().unwrap().head_sha = "3333333333333333333333333333333333333333".into();
        let (merging, run) = queue(ActionSpec { kind: ActionKind::Merge, ..spec }).await?;
        super::pr_action_with(&ctx, run.clone(), &moved).await?;
        assert!(moved.acted().is_empty());
        assert_eq!(status(merging.id).await?, "skipped");
        let (line, wakes): (String, i64) = sqlx::query_as(
            "SELECT MAX(payload->>'line'), COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'action.skipped'",
        )
        .bind(agent)
        .fetch_one(&pool)
        .await?;
        assert_eq!(wakes, 1);
        assert!(line.contains("head moved from 11111111 to 33333333"), "{line}");
        let run_status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1").bind(run.id).fetch_one(&pool).await?;
        assert_eq!(run_status, "completed");

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_report_that_never_came_still_finalizes_the_quest_as_failed_with_its_reason() -> anyhow::Result<()> {
        use loka_shared::world::WorldStore;
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let store: WorldStore = ctx.store.clone();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("silent-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'codex', 'gpt', $2) RETURNING id",
        )
        .bind(format!("silent-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, repo) VALUES ($1, 'ship it', 'in_progress', $2, 'acme/app') RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&pool)
        .await?;
        let report = Report::synthesized("No commit or push performed.", Some("Commits ahead of main:\nabc fix"));
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, working_dir, summary)
             VALUES ($1, 'task', 'codex', 'gpt', 'thread-1', $2, 'active', $3, $4) RETURNING id",
        )
        .bind(agent_id)
        .bind(task)
        .bind(format!("{}/tasks/{task}/app", std::env::temp_dir().display()))
        .bind(serde_json::to_string(&report)?)
        .fetch_one(&pool)
        .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let session = store.get_session(session).await?.expect("session");

        super::finalize(&ctx, &agent, &session, None).await?;

        let (status, error, pr): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, error_message, pr_url FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
        assert_eq!(status, "failed", "a quest without its report never counts as done");
        assert_eq!(error.as_deref(), Some(crate::world::prompts::SYNTHESIZED_ERROR), "a publish that could not run never replaces the reason");
        assert_eq!(pr, None);
        let finished: Vec<bool> = sqlx::query_scalar(
            "SELECT COALESCE((payload#>>'{report,synthesized}')::boolean, false) FROM world_events WHERE task_id = $1 AND type = 'task.finished'",
        )
        .bind(task)
        .fetch_all(&pool)
        .await?;
        assert_eq!(finished, [true]);

        super::finalize(&ctx, &agent, &store.get_session(session.id).await?.expect("session"), None).await?;
        let announced: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.finished'")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(announced, 1, "a stale re-finish never announces the quest twice");
        Ok(())
    }

    #[tokio::test]
    async fn a_done_follow_up_finishes_the_quest_from_every_starting_state() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let earlier = |status: &str, summary: &str| serde_json::to_string(&Report { status: Some(status.into()), summary: Some(summary.into()), ..Default::default() });
        let starts = [
            ("review", "completed", earlier("done", "First version")?, None),
            ("done", "completed", earlier("done", "First version")?, None),
            ("failed", "failed", earlier("blocked", "Waiting for Loka to publish")?, Some("Task reported blocked: Waiting for Loka to publish")),
        ];
        for (quest_status, session_status, summary, error) in starts {
            let tag = Uuid::new_v4().simple().to_string();
            let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
                .bind(format!("follow-{tag}@example.test"))
                .fetch_one(&pool)
                .await?;
            let agent_id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'haiku', $2) RETURNING id",
            )
            .bind(format!("follow-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, error_message, assigned_agent_id) VALUES ($1, 'ship it', $2, $3, $4) RETURNING id",
            )
            .bind(owner)
            .bind(quest_status)
            .bind(error)
            .bind(agent_id)
            .fetch_one(&pool)
            .await?;
            let session: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, summary, review)
                 VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', $2, $3, $4, FALSE) RETURNING id",
            )
            .bind(agent_id)
            .bind(task)
            .bind(session_status)
            .bind(&summary)
            .fetch_one(&pool)
            .await?;
            let agent = ctx.store.get_agent(agent_id).await?.expect("agent");
            let first = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "One more tweak.", "Abhi", false).await?;
            let second = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "And the docs.", "Abhi", false).await?;
            let output = "Done.\n<!-- report {\"status\": \"done\", \"summary\": \"Second version\"} report -->";
            let report = super::prompts::parse_report(output);
            let finished = || async {
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT payload->>'status', payload#>>'{report,summary}' FROM world_events WHERE task_id = $1 AND type = 'task.finished' ORDER BY seq",
                )
                .bind(task)
                .fetch_all(&pool)
                .await
            };
            for (run, last) in [(&first, false), (&second, true)] {
                sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
                    .bind(run.id)
                    .execute(&pool)
                    .await?;
                let current = ctx.store.get_session(session).await?.expect("session");
                super::after_completed(&ctx, &agent, &current, run, report.as_ref(), output).await?;
                let announced = finished().await?;
                match last {
                    false => assert!(announced.is_empty(), "{quest_status}: a follow-up still waiting holds the publish"),
                    true => assert_eq!(
                        announced,
                        [("done".to_string(), Some("Second version".to_string()))],
                        "{quest_status}: a done follow-up publishes and announces the quest"
                    ),
                }
            }
            let (status, error): (String, Option<String>) =
                sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_eq!((status.as_str(), error), ("done", None), "{quest_status}: the quest finishes done");
            assert_eq!(ctx.store.get_session(session).await?.expect("session").status, "completed", "{quest_status}");
            let retry = ctx
                .store
                .start_followup(&ctx.store.get_session(session).await?.expect("session"), "Retry the same report.", "Abhi", false)
                .await?;
            sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
                .bind(retry.id)
                .execute(&pool)
                .await?;
            let current = ctx.store.get_session(session).await?.expect("session");
            super::after_completed(&ctx, &agent, &current, &retry, report.as_ref(), output).await?;
            let retried: (String, i64) = sqlx::query_as(
                "SELECT t.status, COUNT(e.seq) FROM agent_tasks t
                 LEFT JOIN world_events e ON e.task_id = t.id AND e.type = 'task.finished'
                 WHERE t.id = $1 GROUP BY t.status",
            )
            .bind(task)
            .fetch_one(&pool)
            .await?;
            assert_eq!(retried, ("done".into(), 1), "{quest_status}: retry reconciles without another announcement");
        }
        Ok(())
    }

    #[tokio::test]
    async fn local_best_of_prework_prepares_extra_workspaces_only_after_release() -> anyhow::Result<()> {
        use loka_shared::world::NewSession;
        use serde_json::json;
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("best-of-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, quest_context) VALUES ($1, 'Bun', 'codex', 'gpt', $2, 'lean') RETURNING id",
        )
        .bind(format!("best-of-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let main = ctx
            .store
            .create_session(NewSession {
                agent_id,
                role: "main",
                backend: "codex",
                model: "gpt",
                parent_session_id: None,
                task_id: None,
                working_dir: None,
                usage_baseline: json!({}),
            })
            .await?;
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, attempts, context) VALUES ($1, 'Compare it', 'todo', $2, 2, 'lean') RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&pool)
        .await?;
        let agent = ctx.store.get_agent(agent_id).await?.expect("agent");

        let started = super::start_task(
            &ctx,
            &agent,
            &main,
            task_id,
            super::super::quests::StartBrief::Ordinary(None),
            false,
            None,
        )
        .await?
        .expect("task started");
        let first = std::env::temp_dir().join(task_id.to_string()).join("scratch");
        let second = std::env::temp_dir().join(task_id.to_string()).join("scratch-2");
        assert!(first.is_dir());
        assert!(!second.exists());
        let session = ctx.store.get_session(started.session_id).await?.expect("session");
        assert!(loka_shared::quests::is_best_of_prework(session.asked_by.as_ref()));
        assert_eq!(session.attempt, None);
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(started.run_id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1")
            .bind(session.id)
            .bind(format!("best-of-{tag}"))
            .execute(&pool)
            .await?;
        let upload = Uuid::new_v4();
        let accepted_status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await?;
        let continuation = sqlx::query_as::<_, loka_shared::world::InboxItem>(
            "INSERT INTO agent_inbox (agent_id, kind, payload, status)
             VALUES ($1, 'task.assigned', $2, 'processing')
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(agent_id)
        .bind(json!({
            "task_id": task_id,
            "title": "Compare it",
            "brief": "Original request: compare both options. Their answer: use the team account.",
            "resume": true,
            "clarification_continuation": Uuid::new_v4(),
            "clarification_status": accepted_status,
            "clarification_resume": false,
            "attachments": [{ "id": upload, "name": "requirements.txt", "mime": "text/plain" }],
            "files": [{ "name": "notes.txt", "mime": "text/plain", "text": "Keep both checks." }],
        }))
        .fetch_one(&pool)
        .await?;
        let continued = super::start_task(
            &ctx,
            &agent,
            &main,
            task_id,
            super::super::quests::StartBrief::Clarification(&continuation),
            false,
            None,
        )
        .await?
        .expect("clarification continued");
        assert!(continued.resumed);
        assert_eq!(continued.session_id, session.id);
        let continued_run = ctx.store.get_run(continued.run_id).await?.expect("continued run");
        assert!(continued_run.prompt.contains("Their answer: use the team account."));
        assert!(continued_run.prompt.contains(&format!("upload {upload}")));
        assert!(continued_run.prompt.contains("Keep both checks."));
        let replay = super::start_task(
            &ctx,
            &agent,
            &main,
            task_id,
            super::super::quests::StartBrief::Clarification(&continuation),
            false,
            None,
        )
        .await;
        assert!(replay.is_err());
        let continuations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND kind = 'followup'",
        )
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(continuations, 1);
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(continued.run_id)
            .execute(&pool)
            .await?;
        let run = ctx.store.get_run(continued.run_id).await?.expect("run");
        let output = "<!-- report {\"status\":\"done\",\"summary\":\"Ready\",\"proceed_without_plan\":true} report -->";
        let report = super::prompts::parse_report(output);

        super::after_completed(&ctx, &agent, &session, &run, report.as_ref(), output).await?;
        assert!(second.is_dir());
        let active: Vec<(i16, String)> = sqlx::query_as(
            "SELECT attempt, working_dir FROM agent_sessions WHERE task_id = $1 AND status = 'active' AND attempt IS NOT NULL ORDER BY attempt",
        )
        .bind(task_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(active, vec![(1, first.display().to_string()), (2, second.display().to_string())]);
        let queued: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND status = 'queued' AND kind = 'task'",
        )
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(queued, 2);
        let released = ctx.store.get_session(session.id).await?.expect("released session");
        assert_eq!(released.status, "archived");
        assert!(!loka_shared::quests::is_best_of_prework(released.asked_by.as_ref()));

        super::after_completed(&ctx, &agent, &session, &run, report.as_ref(), output).await?;
        let queued_after_retry: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND status = 'queued' AND kind = 'task'",
        )
        .bind(task_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(queued_after_retry, 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_verified_merge_settles_before_a_queued_local_followup() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("merge-queue-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id)
             VALUES ($1, 'Bun', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("merge-queue-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let pr_url = "https://github.com/acme/app/pull/9";
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, error_message, assigned_agent_id, repo, pr_url, pr_number, merged_pr_url)
             VALUES ($1, 'ship it', 'failed', 'earlier failure', $2, 'acme/app', $3, 9, $3) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .bind(pr_url)
        .fetch_one(&pool)
        .await?;
        sqlx::query("DELETE FROM task_milestones WHERE task_id = $1 AND kind = 'merge'")
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO task_milestones (task_id, title, kind, status) VALUES ($1, 'merge', 'merge', 'active')")
            .bind(task)
            .execute(&pool)
            .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, working_dir, review)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', $2, 'failed', $3, FALSE) RETURNING id",
        )
        .bind(agent_id)
        .bind(task)
        .bind(format!("tasks/{task}/app"))
        .fetch_one(&pool)
        .await?;
        let first = ctx
            .store
            .start_followup(&ctx.store.get_session(session).await?.expect("session"), "Merge it.", "Abhi", false)
            .await?;
        let second = ctx
            .store
            .start_followup(&ctx.store.get_session(session).await?.expect("session"), "Clean up.", "Abhi", false)
            .await?;
        sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(first.id)
            .execute(&pool)
            .await?;
        assert!(ctx.store.settle_merged_followup(task, session, pr_url).await?);
        let state: (String, Option<String>, String, String, String) = sqlx::query_as(
            "SELECT t.status, t.error_message, m.status, s.status, r.status
             FROM agent_tasks t JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge'
             JOIN agent_sessions s ON s.id = $2 JOIN session_runs r ON r.id = $3 WHERE t.id = $1",
        )
        .bind(task)
        .bind(session)
        .bind(second.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, ("done".into(), None, "done".into(), "completed".into(), "cancelled".into()));
        Ok(())
    }

    #[tokio::test]
    async fn withdrawing_the_follow_up_a_finished_one_waited_on_finishes_the_quest_on_the_local_worker() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("withdraw-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("withdraw-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'ship it', 'done', $2) RETURNING id")
            .bind(owner)
            .bind(agent_id)
            .fetch_one(&pool)
            .await?;
        let summary = serde_json::to_string(&Report { status: Some("done".into()), summary: Some("First version".into()), ..Default::default() })?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, summary, review)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', $2, 'completed', $3, FALSE) RETURNING id",
        )
        .bind(agent_id)
        .bind(task)
        .bind(&summary)
        .fetch_one(&pool)
        .await?;
        let agent = ctx.store.get_agent(agent_id).await?.expect("agent");
        let answered = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "One more tweak.", "Abhi", false).await?;
        let stale = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "And the docs.", "Abhi", false).await?;
        let output = "Done.\n<!-- report {\"status\": \"done\", \"summary\": \"Second version\"} report -->";
        sqlx::query("UPDATE session_runs SET status = 'completed', output = $2, started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(answered.id)
            .bind(output)
            .execute(&pool)
            .await?;
        let current = ctx.store.get_session(session).await?.expect("session");
        super::after_completed(&ctx, &agent, &current, &answered, super::prompts::parse_report(output).as_ref(), output).await?;
        let finished = || async {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT payload->>'status', payload#>>'{report,summary}' FROM world_events WHERE task_id = $1 AND type = 'task.finished' ORDER BY seq",
            )
            .bind(task)
            .fetch_all(&pool)
            .await
        };
        assert!(finished().await?.is_empty(), "the finished follow-up waits for the queued one");
        let replies = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'session.replied'").bind(task).fetch_one(&pool).await
        };
        let replied = replies().await?;

        assert!(ctx.store.withdraw_queued(stale.id, "Abhi").await?);
        super::carry_on_withdrawn(&ctx).await?;
        assert_eq!(finished().await?, [("done".to_string(), Some("Second version".to_string()))], "withdrawing it finishes the quest with the finished work");
        assert_eq!(replies().await?, replied, "carrying the work on does not post the reply again");
        super::carry_on_withdrawn(&ctx).await?;
        assert_eq!(finished().await?.len(), 1, "the work is carried on once");
        let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
        assert_eq!(status, "done");
        assert!(ctx.store.settle_cancelled_work().await?.iter().all(|id| *id != task), "the withdrawn run never fails the quest");

        sqlx::query("UPDATE agent_sessions SET review = TRUE WHERE id = $1").bind(session).execute(&pool).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'in_progress' WHERE id = $1").bind(task).execute(&pool).await?;
        let reviewed = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "Review this too.", "Abhi", false).await?;
        let withdrawn = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "This one is stale.", "Abhi", false).await?;
        sqlx::query("UPDATE session_runs SET status = 'completed', output = $2, started_at = NOW(), finished_at = NOW() WHERE id = $1")
            .bind(reviewed.id)
            .bind(output)
            .execute(&pool)
            .await?;
        assert!(ctx.store.withdraw_queued(withdrawn.id, "Abhi").await?);
        let current = ctx.store.get_session(session).await?.expect("session");
        super::after_completed(&ctx, &agent, &current, &reviewed, super::prompts::parse_report(output).as_ref(), output).await?;
        super::carry_on_withdrawn(&ctx).await?;
        let reviews: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
             WHERE s.parent_session_id = $1 AND s.role = 'review' AND r.kind = 'review' AND r.created_at > $2",
        )
        .bind(session)
        .bind(reviewed.created_at)
        .fetch_one(&pool)
        .await?;
        assert_eq!(reviews, 1, "carrying the same completed follow-up on does not queue another review");
        Ok(())
    }

    #[tokio::test]
    async fn withdrawing_the_follow_up_after_a_plan_posts_the_finished_plan() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("withdraw-plan-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(format!("withdraw-plan-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, plan_first) VALUES ($1, 'plan it', 'in_progress', $2, TRUE) RETURNING id",
        )
        .bind(owner)
        .bind(agent_id)
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, plan_state)
             VALUES ($1, 'task', 'claude', 'haiku', 'cli-plan', $2, 'active', 'planning') RETURNING id",
        )
        .bind(agent_id)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let output = "1. Change the worker\n2. Run its focused tests";
        let planned: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output)
             VALUES ($1, $2, $3, 'followup', 'completed', 'Plan the work', $4) RETURNING id",
        )
        .bind(session)
        .bind(agent_id)
        .bind(task)
        .bind(output)
        .fetch_one(&pool)
        .await?;
        let stale: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt)
             VALUES ($1, $2, $3, 'followup', 'queued', 'Replace the plan') RETURNING id",
        )
        .bind(session)
        .bind(agent_id)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE session_runs SET started_at = NOW(), finished_at = clock_timestamp() WHERE id = $1")
            .bind(planned)
            .execute(&pool)
            .await?;
        assert!(ctx.store.withdraw_queued(stale, "Abhi").await?);
        super::carry_on_withdrawn(&ctx).await?;
        let state: Option<String> = sqlx::query_scalar("SELECT plan_state FROM agent_sessions WHERE id = $1").bind(session).fetch_one(&pool).await?;
        assert_eq!(state.as_deref(), Some("awaiting"));
        let ready: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.plan_ready'")
            .bind(task)
            .fetch_one(&pool)
            .await?;
        assert_eq!(ready, 1, "the completed planning run posts its plan once");
        Ok(())
    }

    fn quiet_worktree(dir: &std::path::Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let git = |args: &[&str]| -> anyhow::Result<()> {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=Loka", "-c", "user.email=loka@example.test", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
                .stdout(std::process::Stdio::null())
                .status()?;
            anyhow::ensure!(status.success(), "git {args:?} failed");
            Ok(())
        };
        git(&["init", "-q", "-b", "main"])?;
        git(&["commit", "-q", "--allow-empty", "-m", "first"])
    }

    #[tokio::test]
    async fn a_done_follow_up_that_changed_nothing_gets_its_review_instead_of_failing() -> anyhow::Result<()> {
        use uuid::Uuid;
        let Some(ctx) = test_ctx().await? else {
            return Ok(());
        };
        let pool = ctx.db.pool().clone();
        let done = serde_json::to_string(&Report { status: Some("done".into()), summary: Some("First version".into()), ..Default::default() })?;
        let starts = [
            ("failed", "failed", Some("Independent review failed: timed out")),
            ("review", "completed", None),
            ("in_progress", "active", None),
        ];
        for (quest_status, session_status, error) in starts {
            let tag = Uuid::new_v4().simple().to_string();
            let dir = std::env::temp_dir().join(format!("loka-unchanged-{tag}"));
            quiet_worktree(&dir)?;
            let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
                .bind(format!("unchanged-{tag}@example.test"))
                .fetch_one(&pool)
                .await?;
            let agent_id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bun', 'claude', 'haiku', $2) RETURNING id",
            )
            .bind(format!("unchanged-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, error_message, assigned_agent_id, repo) VALUES ($1, 'ship it', $2, $3, $4, 'acme/app') RETURNING id",
            )
            .bind(owner)
            .bind(quest_status)
            .bind(error)
            .bind(agent_id)
            .fetch_one(&pool)
            .await?;
            let session: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, task_id, status, summary, working_dir)
                 VALUES ($1, 'task', 'claude', 'haiku', 'cli-1', $2, $3, $4, $5) RETURNING id",
            )
            .bind(agent_id)
            .bind(task)
            .bind(session_status)
            .bind(&done)
            .bind(dir.display().to_string())
            .fetch_one(&pool)
            .await?;
            sqlx::query(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, created_at)
                 VALUES ($1, $2, $3, 'task', 'completed', 'work', NOW() - INTERVAL '3 minutes')",
            )
            .bind(session)
            .bind(agent_id)
            .bind(task)
            .execute(&pool)
            .await?;
            let agent = ctx.store.get_agent(agent_id).await?.expect("agent");
            let run = ctx.store.start_followup(&ctx.store.get_session(session).await?.expect("session"), "Anything left?", "Abhi", false).await?;
            sqlx::query("UPDATE session_runs SET status = 'completed', started_at = NOW(), finished_at = NOW() WHERE id = $1")
                .bind(run.id)
                .execute(&pool)
                .await?;
            let output = "Nothing left.\n<!-- report {\"status\": \"done\", \"summary\": \"Nothing left\"} report -->";
            let current = ctx.store.get_session(session).await?.expect("session");
            super::after_completed(&ctx, &agent, &current, &run, super::prompts::parse_report(output).as_ref(), output).await?;

            let reviews: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id
                 WHERE s.parent_session_id = $1 AND s.role = 'review' AND r.kind = 'review' AND r.status = 'queued'",
            )
            .bind(session)
            .fetch_one(&pool)
            .await?;
            assert_eq!(reviews, 1, "{quest_status}: work that was never reviewed gets its review");
            let finished: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.finished'")
                .bind(task)
                .fetch_one(&pool)
                .await?;
            assert_eq!(finished, 0, "{quest_status}: nothing finishes or publishes before the review");
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(task).fetch_one(&pool).await?;
            assert_ne!(status, "failed", "{quest_status}: a missing review never fails the quest");
            let unchanged: bool = sqlx::query_scalar("SELECT unchanged_work FROM session_runs WHERE id = $1").bind(run.id).fetch_one(&pool).await?;
            assert!(unchanged, "{quest_status}: the follow-up is recorded as one that changed nothing");
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    #[test]
    fn turns_tool_calls_into_plain_phrases() {
        assert_eq!(activity_phrase("Read", "…/src/world/runs.rs"), "Reading runs.rs");
        assert_eq!(activity_phrase("Bash", "cargo test -p x"), "Running cargo");
        assert_eq!(activity_phrase("Bash", "/bin/zsh -lc 'ls'"), "Running a command");
        assert_eq!(activity_phrase("Grep", "\"foo\" in src"), "Searching the code");
        assert_eq!(activity_phrase("linear/list_issues", ""), "Using list_issues");
    }

    #[test]
    fn cursor_fork_never_falls_back_to_main_session() {
        assert_eq!(session_id_after_run("cursor", true, None, Some("main")), None);
        assert_eq!(
            session_id_after_run("cursor", true, Some("task"), Some("main")),
            Some("task")
        );
        assert_eq!(session_id_after_run("codex", true, None, Some("main")), Some("main"));
    }

    #[test]
    fn preserves_report_status_when_secret_matches_control_value() {
        let redactor = crate::cli::Redactor::new(["done"]);
        let report = redact_report(&redactor, crate::world::prompts::parse_report(
            r#"<!-- report {"status":"done","summary":"done"} report -->"#,
        ).unwrap());
        assert_eq!(report.status.as_deref(), Some("done"));
        assert_eq!(report.summary.as_deref(), Some("[REDACTED]"));
    }
}
