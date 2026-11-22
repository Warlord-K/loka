use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use loka_shared::milestones::{finished_status, MilestoneStore};
use loka_shared::models::AgentTask;
use loka_shared::runners::RunnerUsage;
use loka_shared::world::{
    resume_prompt, AgentSession, NewEvent, NewRun, NewSession, ReviewerChoice, RunOutcome, SessionRun, TaskResume, WorldAgent,
};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::{prompts, Ctx};
use crate::cli::{self, CliOptions, CliResult, StreamEvent, Usage};
use crate::git::{github, workspace};

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
    let prev_f = |k: &str| session.usage_baseline.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let mut baseline = session.usage_baseline.clone();
    if !baseline.is_object() {
        baseline = json!({});
    }
    let usage = if session.backend == "codex" {
        let total = RunnerUsage {
            input_tokens: u.input_tokens,
            cached_input_tokens: u.cached_input_tokens,
            output_tokens: u.output_tokens,
            context_tokens: 0,
        };
        let delta = total.since(&session.usage_baseline);
        total.record(&mut baseline);
        Usage {
            input_tokens: delta.input_tokens,
            cached_input_tokens: delta.cached_input_tokens,
            output_tokens: delta.output_tokens,
            context_tokens: u.context_tokens,
        }
    } else {
        u
    };
    let cost_usd = if loka_shared::backends::cost_per_run(&session.backend) && result.cost_usd > 0.0 {
        result.cost_usd
    } else if result.cost_usd > 0.0 {
        let before = prev_f("cost_usd");
        if let Some(obj) = baseline.as_object_mut() {
            obj.insert("cost_usd".into(), json!(result.cost_usd));
        }
        loka_shared::pricing::session_cost_delta(result.cost_usd, before, &session.model, usage.input_tokens, usage.cached_input_tokens, usage.output_tokens)
    } else {
        loka_shared::pricing::cost(&session.model, usage.input_tokens, usage.cached_input_tokens, usage.output_tokens)
    };
    Accounted { usage, baseline, cost_usd }
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

pub fn context_tokens(usage: &Usage) -> i64 {
    loka_shared::context::used(usage.context_tokens, usage.input_tokens, usage.cached_input_tokens)
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
    brief: Option<&str>,
    review: bool,
    reviewer: Option<&ReviewerChoice>,
) -> Result<()> {
    let task = ctx
        .db
        .get_task(task_id)
        .await?
        .ok_or_else(|| anyhow!("task {task_id} not found"))?;
    match ctx
        .store
        .resume_task_session(agent.id, task.id, &resume_prompt(&agent.name, brief))
        .await?
    {
        TaskResume::Busy(session_id) => {
            return Err(anyhow!(
                "it is already being worked on in session {session_id}; use message_session to steer that session instead of starting it again"
            ))
        }
        TaskResume::AwaitingPlan(session_id) => {
            return Err(anyhow!("session {session_id} has a plan that {}", loka_shared::quests::PLAN_WAITING))
        }
        TaskResume::Resumed { session_id, run_id } => {
            ctx.db.update_status(task.id, "in_progress").await?;
            ctx.store
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
            ctx.store
                .post_quest_started(agent.id, task.id, session_id, &task.title, brief.filter(|b| !b.trim().is_empty()))
                .await?;
            info!(agent = %agent.slug, task = %task.id, "Task session resumed");
            return Ok(());
        }
        TaskResume::Fresh => {}
    }
    let options = ctx.store.quest_options(task.id).await?;
    let mut dirs = Vec::new();
    for attempt in 1..=options.attempts.max(1) {
        dirs.push(prepare_workspace(ctx, &task, attempt).await?.to_string_lossy().to_string());
    }
    let rules = work_rules(ctx, agent.id).await;
    let opened = super::quests::open_sessions(
        &ctx.store,
        super::quests::QuestStart { agent, main, task: &task, brief, review, reviewer, rules: &rules, options: &options },
        &dirs,
    )
    .await?;
    let (session, run) = opened.first().ok_or_else(|| anyhow!("no task session opened"))?;
    ctx.db.update_status(task.id, "in_progress").await?;
    ctx.store
        .emit(
            NewEvent::new("task.started")
                .agent(agent.id)
                .session(session.id)
                .run(run.id)
                .task(Some(task.id))
                .payload(super::quests::started_payload(&task, &options, opened.len())),
            None,
        )
        .await?;
    if opened.len() > 1 {
        super::quests::label_attempts(&ctx.store, &task, &opened).await?;
    }
    ctx.store
        .post_quest_started(agent.id, task.id, session.id, &task.title, brief.filter(|b| !b.trim().is_empty()))
        .await?;
    info!(agent = %agent.slug, task = %task.id, "Task session queued");
    Ok(())
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

pub async fn execute(ctx: &Ctx, run: SessionRun) -> Result<()> {
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
    let working_dir = PathBuf::from(
        session
            .working_dir
            .clone()
            .unwrap_or_else(|| super::agent_home(ctx, &agent.slug).to_string_lossy().to_string()),
    );
    if !tokio::fs::metadata(&working_dir).await.is_ok_and(|metadata| metadata.is_dir()) {
        let error = format!("the saved workspace is missing: {}", working_dir.display());
        return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
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
            return fail(ctx, &run, &session, "failed", "run.failed", &error.to_string()).await
        }
    };
    let environment = if has_environment {
        match ctx.store.without_mcp_secrets(agent.id, granted_environment.clone()).await {
            Ok(environment) => environment,
            Err(error) => {
                return fail(ctx, &run, &session, "failed", "run.failed", &error.to_string()).await
            }
        }
    } else {
        Default::default()
    };
    let provider_keys = match ctx.store.provider_keys_for_agent(agent.id).await {
        Ok(keys) => keys,
        Err(error) => {
            return fail(ctx, &run, &session, "failed", "run.failed", &error.to_string()).await
        }
    };
    let redactor = SecretRedactor::new(granted_environment.values().chain(provider_keys.values()));
    let kit = match run.kind.as_str() {
        "aside" => Ok(None),
        "review" => super::kit::for_review(ctx, agent.id).await.map(Some),
        _ => super::kit::for_agent(ctx, agent.id, false).await.map(Some),
    };
    let kit = match kit {
        Ok(kit) => kit,
        Err(error) => {
            let error = redactor.redact(&error.to_string());
            return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
        }
    };
    let omp = match ctx.store.resolved_omp_config(agent.id, &session.backend).await {
        Ok(omp) => omp,
        Err(error) => {
            let error = redactor.redact(&error.to_string());
            return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
        }
    };
    let mut prompt = match kit.as_ref().map(|k| k.skills_index.as_str()).filter(|i| !i.is_empty()) {
        Some(index) => format!("{}\n\n{index}", run.prompt),
        None => run.prompt.clone(),
    };
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
            Err(error) => return fail(ctx, &run, &session, "failed", "run.failed", &error.to_string()).await,
        }
    }
    let max_turns = match run.kind.as_str() {
        "review" => REVIEW_MAX_TURNS,
        "aside" => 8,
        "quick" => loka_shared::quick::MAX_TURNS,
        _ => TASK_MAX_TURNS,
    };
    let (event_tx, activity) = spawn_activity(
        ctx.store.clone(),
        run.clone(),
        !aside,
        redactor.clone(),
    );
    let pids = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let event_tx = record_pids(event_tx, pids.clone());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<String>();
    let stop_watch = spawn_stop_watch(ctx, run.id, stop_tx);
    let mcp_has_secrets = kit.as_ref().is_some_and(|kit| kit.has_secret_refs);
    let owner_aside = aside && !loka_shared::world::toolless_aside(session.asked_by.as_ref());
    let servers = if owner_aside {
        super::kit::world_server_for(agent.id, None, true)
    } else if quick {
        let mut servers = kit.map(|kit| kit.servers).unwrap_or_default();
        servers.extend(super::kit::world_server_for(agent.id, Some(session.id), false));
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
    let wired = match super::kit::wire(servers, agent.owner_user_id, agent.id, &working_dir).await {
        Ok(wired) => wired,
        Err(error) => {
            let error = redactor.redact(&error.to_string());
            return fail(ctx, &run, &session, "failed", "run.failed", &error).await;
        }
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
        image_paths: Vec::new(),
        fork,
        lean: aside,
        effort: effort.as_deref(),
        fast,
        environment: environment.clone(),
        mcp_has_secrets,
        sensitive: !granted_environment.is_empty() || !provider_keys.is_empty() || mcp_has_secrets,
        rewind_to: None,
        approval: if aside { cli::Approval::Full } else { cli::Approval::parse(Some(&agent.approval_mode)) },
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
            cli::record_spawns(wired.pids.clone(), cli::run(&session.backend, opts)),
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

    match outcome {
        Ok(Ok(result)) => {
            let acc = account(&session, &result);
            let output = redactor.redact(&result.output);
            let report = prompts::parse_report(&result.output).map(|report| redactor.redact_report(report));
            let live = ctx.store
                .finish_run(
                    run.id,
                    RunOutcome {
                        status: "completed",
                        output: Some(&output),
                        error: None,
                        cost_usd: acc.cost_usd,
                        input_tokens: acc.usage.input_tokens as i64,
                        cached_input_tokens: acc.usage.cached_input_tokens as i64,
                        output_tokens: acc.usage.output_tokens as i64,
                    },
                )
                .await?;
            ctx.store
                .update_session_after_run(
                    session.id,
                    session_id_after_run(
                        &session.backend,
                        fork,
                        result.session_id.as_deref(),
                        cli_sid.as_deref(),
                    ),
                    acc.baseline,
                    context_tokens(&result.usage),
                )
                .await?;
            if !live {
                warn!(run = %run.id, "Run finished after it was already closed, keeping the earlier outcome");
                return Ok(());
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
            emit_run(ctx, &run, "run.completed", json!({ "cost_usd": acc.cost_usd })).await?;
            let session = ctx.store.get_session(session.id).await?.unwrap_or(session);
            if session.role == "task" && session.planning() && matches!(run.kind.as_str(), "task" | "followup") {
                return super::quests::plan_step(&ctx.store, &agent, &session, &run, &output).await;
            }
            match run.kind.as_str() {
                "task" => after_task(ctx, &agent, &session, &run, report.as_ref(), &output).await,
                "review" => after_review(ctx, &agent, &session, &run, &output).await,
                "aside" => after_aside(ctx, &agent, &session, Ok(&output)).await,
                "quick" => {
                    let text = prompts::plain_dashes(&prompts::split_actions(&output).0);
                    let text = if text.trim().is_empty() { "Done.".to_string() } else { text };
                    ctx.store.record_quick_reply(&agent, &session, run.id, &text, false).await.map(|_| ())
                }
                "followup" => after_followup(ctx, &agent, &session, &run, &output, report.as_ref()).await,
                _ => Ok(()),
            }
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
        Ok(Err(e)) => {
            let error = redactor.redact(&e.to_string());
            fail(ctx, &run, &session, "failed", "run.failed", &error).await
        }
        Err(_) => {
            let msg = format!("timed out after {}s", run.timeout_secs);
            fail(ctx, &run, &session, "timed_out", "run.timed_out", &msg).await
        }
    }
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

pub async fn fail_stale(ctx: &Ctx, run: &SessionRun, error: &str) -> Result<()> {
    let session = ctx
        .store
        .get_session(run.session_id)
        .await?
        .ok_or_else(|| anyhow!("session {} missing", run.session_id))?;
    handle_failure(ctx, run, &session, "run.failed", error).await
}

async fn handle_failure(
    ctx: &Ctx,
    run: &SessionRun,
    session: &AgentSession,
    event: &str,
    error: &str,
) -> Result<()> {
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
    let error = &loka_shared::cli_errors::describe(error);
    let payload = || {
        let mut payload = loka_shared::cli_errors::failure_payload(run.task_id, error, session.id, &run.kind);
        if let Some(diagnosis) = &diagnosis {
            diagnosis.merge_into(&mut payload);
        }
        payload
    };
    ctx.store
        .fail_queued_followups(session.id, &format!("Parent run failed: {error}"))
        .await?;
    if run.kind == "aside" {
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        return after_aside(ctx, &agent, session, Err(error)).await;
    }
    if run.kind == "quick" {
        let agent = ctx.store.get_agent(run.agent_id).await?.ok_or_else(|| anyhow!("agent missing"))?;
        ctx.store.record_quick_reply(&agent, session, run.id, &format!("I could not answer that: {error}"), true).await?;
        return Ok(());
    }
    if run.kind == "review" && prompts::failed_nudge_settles(&run.prompt, error) && loka_shared::quests::compare_candidates(session).is_none() {
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
                    if let Some(url) = merged_pr_url(ctx, &task_session).await? {
                        return super::watch::settle_merged(&ctx.store, &ctx.db, &task_session, &url).await;
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
    let task_failed = run.kind == "task"
        || run.kind == "followup" && session.role == "task" && session.status == "active";
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
    if let (Some(task_id), true) = (run.task_id, task_failed) {
        ctx.store.set_session_status(session.id, "failed", Some(error)).await?;
        ctx.db.set_error(task_id, error).await?;
    }
    let ev = NewEvent::new(event)
        .agent(run.agent_id)
        .session(session.id)
        .run(run.id)
        .task(run.task_id)
        .payload(payload());
    ctx.store.emit(ev, Some((run.agent_id, event))).await?;
    Ok(())
}

async fn emit_run(ctx: &Ctx, run: &SessionRun, event: &str, payload: Value) -> Result<()> {
    ctx.store
        .emit(
            NewEvent::new(event)
                .agent(run.agent_id)
                .session(run.session_id)
                .run(run.id)
                .task(run.task_id)
                .payload(payload),
            None,
        )
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
    let done = report.and_then(|r| r.status.as_deref()) == Some("done");
    advance_task(ctx, agent, session, run.task_id, done, true).await
}

async fn missing_report(ctx: &Ctx, session: &AgentSession, run: &SessionRun, output: &str) -> Result<bool> {
    let diagnosis = ctx.store.record_run_end(run.id, &run.kind, None, output, false).await?;
    if diagnosis.cause == loka_shared::run_causes::Cause::NeverStarted {
        return Ok(ctx.store.auto_retry(run.id, &diagnosis).await?.is_some());
    }
    if ctx.store.nudge_for_report(session.id).await?.is_some() {
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

async fn advance_task(
    ctx: &Ctx,
    agent: &WorldAgent,
    session: &AgentSession,
    task_id: Option<Uuid>,
    done: bool,
    review: bool,
) -> Result<()> {
    let task = match task_id {
        Some(id) => ctx.db.get_task(id).await?,
        None => None,
    };
    if session.racing() {
        return attempt_done(ctx, agent, session, done).await;
    }
    if let (true, true, Some(task)) = (review && ctx.reviews_enabled && session.wants_review(), done, task) {
        return queue_review(ctx, agent, session, &task).await;
    }
    finalize(ctx, agent, session, None).await
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

async fn queue_review(ctx: &Ctx, agent: &WorldAgent, session: &AgentSession, task: &AgentTask) -> Result<()> {
    if let Some(url) = ctx.store.quest_merged_pr(task.id).await? {
        return super::watch::settle_merged(&ctx.store, &ctx.db, session, &url).await;
    }
    let (backend, model) = session.review_target(agent.reviewer.as_ref());
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
    if let Some(report) = report {
        ctx.store
            .set_session_status(session.id, &session.status, Some(&serde_json::to_string(report)?))
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
    if session.role == "task" && session.status == "active" {
        if unreported && missing_report(ctx, session, run, output).await? {
            return Ok(());
        }
        if !ctx.store.complete_followup(session.id).await? {
            return Ok(());
        }
        let session = ctx.store.get_session(session.id).await?.unwrap_or_else(|| session.clone());
        let done = prompts::stored_report(session.summary.as_deref())
            .and_then(|report| report.status)
            .as_deref()
            == Some("done");
        let sync = followup_pr_sync(ctx, &session, run, done).await;
        if sync == loka_shared::runners::PrSync::Merged && !session.racing() {
            if let Some(url) = quest_pr_url(ctx, run.task_id).await? {
                return super::watch::settle_merged(&ctx.store, &ctx.db, &session, &url).await;
            }
        }
        advance_task(ctx, agent, &session, run.task_id, done, sync.needs_review()).await?;
    }
    Ok(())
}

async fn followup_pr_sync(ctx: &Ctx, session: &AgentSession, run: &SessionRun, done: bool) -> loka_shared::runners::PrSync {
    let Some(wd) = session.working_dir.as_deref().filter(|_| done) else {
        return loka_shared::runners::PrSync::Changed;
    };
    let has_repo = match run.task_id {
        Some(id) => ctx.db.get_task(id).await.ok().flatten().is_some_and(|task| task.repo.is_some()),
        None => false,
    };
    if !has_repo {
        return loka_shared::runners::PrSync::Changed;
    }
    github::pr_sync(Path::new(wd), std::time::SystemTime::from(run.created_at)).await
}

async fn merged_pr_url(ctx: &Ctx, task_session: &AgentSession) -> Result<Option<String>> {
    let Some(task_id) = task_session.task_id else {
        return Ok(None);
    };
    if let Some(url) = ctx.store.quest_merged_pr(task_id).await? {
        return Ok(Some(url));
    }
    let (Some(wd), Some(url)) = (task_session.working_dir.as_deref(), quest_pr_url(ctx, Some(task_id)).await?) else {
        return Ok(None);
    };
    let merged = github::pr_sync(Path::new(wd), std::time::SystemTime::now()).await == loka_shared::runners::PrSync::Merged;
    Ok(merged.then_some(url))
}

async fn quest_pr_url(ctx: &Ctx, task_id: Option<Uuid>) -> Result<Option<String>> {
    match task_id {
        Some(id) => Ok(ctx.db.get_task(id).await?.and_then(|task| task.pr_url)),
        None => Ok(None),
    }
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
    if prompts::needs_verdict(output)
        && loka_shared::quests::compare_candidates(review).is_none()
        && ctx.store.nudge_for_verdict(review.id).await?.is_some()
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
            })
            .await?;
        ctx.store
            .set_run_activity(&run, &format!("Fixing review notes on \"{}\" (round {round})", task.title))
            .await?;
        return Ok(());
    }
    if verdict.is_none() && ctx.store.review_runs_since_work(task_session.id).await? < 2 {
        if let Some(task) = match task_session.task_id {
            Some(task_id) => ctx.db.get_task(task_id).await?,
            None => None,
        } {
            return queue_review(ctx, agent, &task_session, &task).await;
        }
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
    let mut pr_url = None;
    let diagnosis = ctx.store.latest_diagnosis(task_session.id).await?;
    let mut error = match (completion_error(&parsed, verdict.as_ref(), ctx.reviews_enabled && task_session.wants_review()), &diagnosis) {
        (Some(_), Some(diagnosis)) if parsed.status.is_none() => Some(diagnosis.reason.clone()),
        (error, _) => error,
    };
    let mut task_status = "failed";
    let mut watching = false;
    let task = match task_session.task_id {
        Some(task_id) => ctx.db.get_task(task_id).await?,
        None => None,
    };
    if ctx
        .store
        .finished_report_unchanged(task_session.id, &report, task.as_ref().and_then(|task| task.pr_url.as_deref()), error.as_deref())
        .await?
    {
        ctx.store
            .set_session_status(task_session.id, if error.is_some() { "failed" } else { "completed" }, None)
            .await?;
        return Ok(());
    }
    let mut publish_secs = None;
    let mut changes_requested = false;
    if let Some(task) = task.as_ref() {
        if error.is_none() {
            if let (Some(repo), Some(wd)) = (task.repo.as_deref(), task_session.working_dir.as_deref()) {
                let publishing = Instant::now();
                match open_pr(ctx, task, repo, Path::new(wd), &parsed).await {
                    Ok(url) => pr_url = url,
                    Err(e) => error = Some(e.to_string()),
                }
                publish_secs = Some(publishing.elapsed().as_secs());
            }
        }
        if let Some(summary) = parsed.summary.as_deref() {
            let _ = ctx.db.set_result_content(task.id, summary).await;
        }
        let milestones = MilestoneStore::new(ctx.store.pool().clone());
        milestones.complete_work(task.id, &parsed.milestone_positions(), &parsed.milestone_titles(), error.is_none()).await?;
        if error.is_none() {
            watching = milestones.arm_merge(task.id, pr_url.is_some()).await?;
        }
        let review_wants_changes = verdict.as_ref().and_then(|v| v.get("verdict")).and_then(Value::as_str) == Some("changes");
        changes_requested = review_wants_changes;
        let already_merged = error.is_none() && milestones.merge_done(task.id).await?;
        task_status = finished_status(error.is_some(), already_merged, pr_url.is_some(), review_wants_changes);
        if let Some(err) = &error {
            ctx.db.set_error(task.id, err).await?;
        } else {
            ctx.db.update_status(task.id, task_status).await?;
        }
    }
    ctx.store
        .set_session_status(task_session.id, if error.is_some() { "failed" } else { "completed" }, None)
        .await?;
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

fn completion_error(
    report: &prompts::Report,
    verdict: Option<&Value>,
    reviews_enabled: bool,
) -> Option<String> {
    if report.synthesized {
        return Some(prompts::SYNTHESIZED_ERROR.to_string());
    }
    match report.status.as_deref() {
        Some("done") => {}
        Some("blocked") => {
            return Some(match report.summary.as_deref().filter(|summary| !summary.trim().is_empty()) {
                Some(summary) => format!("Task reported blocked: {summary}"),
                None => "Task reported blocked".to_string(),
            });
        }
        Some(status) => return Some(format!("Task reported unsupported status: {status}")),
        None => return Some("Task did not provide a valid completion report".to_string()),
    }
    if !reviews_enabled {
        return None;
    }
    let summary = verdict
        .and_then(|value| value.get("summary"))
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty());
    match verdict.and_then(|value| value.get("verdict")).and_then(Value::as_str) {
        Some("review failed") => Some(match summary {
            Some(summary) => format!("Independent review failed: {summary}"),
            None => "Independent review failed".to_string(),
        }),
        Some(_) => None,
        None => Some("Independent review did not provide a valid verdict".to_string()),
    }
}

fn reported_pr_to_keep(found: Option<&github::HeadPr>, report: &prompts::Report, repo: &str) -> Option<(String, i32)> {
    match found {
        None => report.reported_pr(repo),
        Some(_) => None,
    }
}

async fn open_pr(
    ctx: &Ctx,
    task: &AgentTask,
    repo: &str,
    wd: &Path,
    report: &prompts::Report,
) -> Result<Option<String>> {
    let base = base_branch(ctx, task);
    let task_branch = task.branch.clone().unwrap_or_else(|| task.agent_branch_name());
    let target = workspace::PublishTarget { repo, base: &base, onto: &base, quest_branch: &task_branch };
    if let Some(url) = task.pr_url.clone() {
        if let Some(number) = task.pr_number {
            if let Some(state) = github::pr_state(wd, repo, i64::from(number)).await? {
                if github::is_final_pr_state(&state) {
                    return Ok(Some(url));
                }
            }
        }
        let message = report.commit_message.clone().unwrap_or_else(|| format!("fix: address review for {}", task.title));
        let committed = workspace::commit_for_publish(wd, repo, &message).await?;
        if !workspace::ahead_of_fresh_base(wd, &base).await? {
            return Ok(Some(url));
        }
        let parent_pr = loka_agent::runner_server::parent_quest_pr(&ctx.db, task).await;
        let head = match github::open_pr_containing_head(wd, repo, &base, &task_branch, parent_pr).await? {
            github::HeadPr::Own(_, head) => head,
            github::HeadPr::Elsewhere(_) => return Ok(Some(url)),
            github::HeadPr::Extends(pr, head_ref) if github::extends_recorded_pr(task.pr_number.map(i64::from), pr.number) => {
                let extending = workspace::PublishTarget { quest_branch: &head_ref, ..target };
                workspace::publish_commits(wd, &committed, &extending, &head_ref).await.context("push failed")?;
                return Ok(Some(url));
            }
            github::HeadPr::Extends(..) | github::HeadPr::Missing => workspace::publish_branch(wd, Some(&task_branch)).await?,
        };
        workspace::publish_commits(wd, &committed, &target, &head).await?;
        return Ok(Some(url));
    }
    let message = report
        .commit_message
        .clone()
        .unwrap_or_else(|| format!("feat: {}", task.title));
    let committed = workspace::commit_for_publish(wd, repo, &message).await?;
    let parent_pr = loka_agent::runner_server::parent_quest_pr(&ctx.db, task).await;
    let found = if workspace::ahead_of_fresh_base(wd, &base).await? {
        Some(github::open_pr_containing_head(wd, repo, &base, &task_branch, parent_pr).await?)
    } else {
        None
    };
    match found {
        None | Some(github::HeadPr::Elsewhere(_)) => {
            let Some((url, number)) = reported_pr_to_keep(found.as_ref(), report, repo) else {
                return Ok(None);
            };
            ctx.db.set_pr(task.id, &url, number).await?;
            return Ok(Some(url));
        }
        Some(github::HeadPr::Own(pr, head)) => {
            workspace::publish_commits(wd, &committed, &target, &head).await.context("push failed")?;
            ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
            return Ok(Some(pr.url));
        }
        Some(github::HeadPr::Extends(pr, head_ref)) => {
            let extending = workspace::PublishTarget { quest_branch: &head_ref, ..target };
            workspace::publish_commits(wd, &committed, &extending, &head_ref).await.context("push failed")?;
            ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
            return Ok(Some(pr.url));
        }
        Some(github::HeadPr::Missing) => {}
    }
    if !workspace::ahead_of_fresh_base(wd, &base).await? {
        return Ok(None);
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
        return Ok(None);
    };
    ctx.db.set_pr(task.id, &pr.url, pr.number).await?;
    Ok(Some(pr.url))
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
    record_activity: bool,
    redactor: SecretRedactor,
) -> (cli::EventSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    let activity = tokio::spawn(async move {
        let (session_id, run_id) = (run.session_id, run.id);
        let mut last = Instant::now() - STATUS_THROTTLE;
        let mut last_usage = Instant::now() - LIVE_USAGE_EVERY;
        let mut session: Option<AgentSession> = None;
        while let Some(event) = rx.recv().await {
            let clip = |t: &str| redactor.redact(t).chars().take(2000).collect::<String>();
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
                _ if !record_activity => continue,
                StreamEvent::ToolUse { tool, input_summary } => store.insert_run_event(run_id, "tool", Some(tool), &clip(input_summary)).await,
                StreamEvent::ToolResult { tool, output } => store.insert_run_event(run_id, "result", Some(tool), &clip(output)).await,
                StreamEvent::AssistantText(text) => store.insert_run_event(run_id, "text", None, &clip(text)).await,
                StreamEvent::Error(err) => store.insert_run_event(run_id, "error", None, &clip(err)).await,
            };
            if let StreamEvent::ToolUse { tool, input_summary } = event {
                if last.elapsed() < STATUS_THROTTLE {
                    continue;
                }
                last = Instant::now();
                let detail: String = loka_shared::progress::phrase(&tool, &redactor.redact(&input_summary))
                    .chars()
                    .take(120)
                    .collect();
                let _ = store.set_run_activity(&run, &detail).await;
            }
        }
    });
    (tx, activity)
}

#[derive(Clone)]
struct SecretRedactor {
    values: std::sync::Arc<Vec<String>>,
}

impl SecretRedactor {
    fn new<'a>(values: impl Iterator<Item = &'a String>) -> Self {
        let mut values: Vec<String> = values.chain(crate::cli::host_secrets()).filter(|value| !value.is_empty()).cloned().collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self { values: std::sync::Arc::new(values) }
    }

    fn redact(&self, text: &str) -> String {
        self.values
            .iter()
            .fold(text.to_string(), |text, value| text.replace(value, "[REDACTED]"))
    }

    fn redact_report(&self, mut report: prompts::Report) -> prompts::Report {
        report.summary = report.summary.map(|value| self.redact(&value));
        report.decisions = report.decisions.into_iter().map(|value| self.redact(&value)).collect();
        report.followups = report.followups.into_iter().map(|value| self.redact(&value)).collect();
        report.pr_title = report.pr_title.map(|value| self.redact(&value));
        report.pr_body = report.pr_body.map(|value| self.redact(&value));
        report.pr_url = report.pr_url.map(|value| self.redact(&value));
        report.commit_message = report.commit_message.map(|value| self.redact(&value));
        report
    }
}

fn review_verdict_value(output: &str) -> Value {
    match prompts::readable_verdict(output) {
        Some(_) => extract_raw(output, "verdict").unwrap_or_default(),
        None => json!({ "verdict": prompts::NO_VERDICT, "summary": tail(output, 800), "note": prompts::UNREADABLE_VERDICT_NOTE }),
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
mod tests {
    use super::{completion_error, reported_pr_to_keep, review_verdict_value, session_id_after_run, SecretRedactor};
    use crate::git::github::{HeadPr, PrInfo};

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
            assert!(completion_error(&super::prompts::Report { status: Some("done".into()), ..Default::default() }, Some(&value), true).is_none());
        }
        let readable = review_verdict_value("<!-- verdict {\"verdict\":\"approve\",\"summary\":\"ok\"} verdict -->");
        assert_eq!(readable["verdict"], "approve");
        assert!(readable.get("note").is_none());
    }

    #[test]
    fn another_branchs_pr_is_never_attached_from_the_report() {
        let report = Report { pr_url: Some("https://github.com/o/r/pull/86".into()), ..Report::default() };
        let elsewhere = HeadPr::Elsewhere(PrInfo { url: "https://github.com/o/r/pull/86".into(), number: 86 });
        assert_eq!(reported_pr_to_keep(Some(&elsewhere), &report, "o/r"), None);
        assert_eq!(reported_pr_to_keep(Some(&HeadPr::Missing), &report, "o/r"), None);
        assert_eq!(reported_pr_to_keep(None, &report, "o/r"), Some(("https://github.com/o/r/pull/86".to_string(), 86)));
        assert_eq!(reported_pr_to_keep(None, &Report::default(), "o/r"), None);
    }
    use crate::world::prompts::Report;
    use loka_shared::progress::phrase as activity_phrase;
    use serde_json::json;

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
    fn completion_requires_done_report() {
        let missing = Report::default();
        assert!(completion_error(&missing, None, false).is_some());

        let blocked = Report {
            status: Some("blocked".to_string()),
            summary: Some("needs credentials".to_string()),
            ..Default::default()
        };
        assert_eq!(
            completion_error(&blocked, None, false).as_deref(),
            Some("Task reported blocked: needs credentials")
        );
    }

    #[test]
    fn review_that_wants_changes_or_is_unreadable_does_not_fail_the_work() {
        let done = Report { status: Some("done".to_string()), ..Default::default() };
        assert!(completion_error(&done, None, false).is_none());
        assert!(completion_error(&done, Some(&json!({ "verdict": "approve" })), true).is_none());
        assert!(completion_error(&done, None, true).is_some());
        assert!(completion_error(&done, Some(&json!({ "verdict": "changes", "summary": "test is failing" })), true).is_none());
        assert!(completion_error(&done, Some(&json!({ "verdict": "review failed", "summary": "timed out" })), true).is_some());
        assert!(completion_error(&done, Some(&json!({ "verdict": "no verdict" })), true).is_none());
    }

    #[test]
    fn redacts_exact_values_including_unicode() {
        let values = ["short".to_string(), "密钥-value".to_string()];
        let redactor = SecretRedactor::new(values.iter());
        assert_eq!(
            redactor.redact("short and 密钥-value remain private"),
            "[REDACTED] and [REDACTED] remain private"
        );
    }

    #[test]
    fn preserves_report_status_when_secret_matches_control_value() {
        let values = ["done".to_string()];
        let redactor = SecretRedactor::new(values.iter());
        let report = redactor.redact_report(crate::world::prompts::parse_report(
            r#"<!-- report {"status":"done","summary":"done"} report -->"#,
        ).unwrap());
        assert_eq!(report.status.as_deref(), Some("done"));
        assert_eq!(report.summary.as_deref(), Some("[REDACTED]"));
    }
}
