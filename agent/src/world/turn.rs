use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use loka_shared::guests::in_thread;
use loka_shared::models::AgentTask;
use loka_shared::world::{
    AgentSession, Rewind, InboxItem, NewEvent, NewMessage, NewRun, NewSession, ReviewerChoice, RunOutcome, WorldAgent,
};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::prompts::{self, Action};
use super::{runs, Ctx};
use crate::cli::{self, CliOptions};

const TURN_BATCH: i64 = 20;
const TURN_MAX_TURNS: u32 = 8;
const TURN_TIMEOUT: Duration = Duration::from_secs(600);
const PROGRESS_THROTTLE: Duration = Duration::from_millis(1500);

pub async fn process(ctx: &Ctx, agent_id: Uuid) -> Result<()> {
    let agent = ctx
        .store
        .get_agent(agent_id)
        .await?
        .ok_or_else(|| anyhow!("agent {agent_id} missing"))?;
    if let Err(e) = ctx.store.dispatch_guest_inbox(agent_id).await {
        warn!(agent = %agent.slug, error = %e, "Could not hand guest messages to an aside");
    }
    loop {
        let items = ctx.store.take_pending_inbox(agent_id, TURN_BATCH).await?;
        if items.is_empty() {
            break;
        }
        let (reset, rest) = loka_shared::world::partition_reset_items(items);
        let (compact_items, rest) = loka_shared::context::partition_compact_items(rest);
        if !reset.is_empty() {
            reset_context(ctx, &agent, &reset).await;
        }
        if !compact_items.is_empty() {
            compact_on_request(ctx, &agent, &compact_items, !reset.is_empty()).await;
        }
        if !reset.is_empty() || !compact_items.is_empty() {
            let _ = ctx.store.announce_context(agent_id).await;
        }
        if rest.is_empty() {
            continue;
        }
        let items = rest;
        let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
        ctx.store.set_agent_status(agent_id, "thinking", None).await?;
        let heartbeat = {
            let store = ctx.store.clone();
            let worker = ctx.worker.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let _ = store.heartbeat_agent_claim(agent_id, &worker).await;
                }
            })
        };
        let typing = ctx.store.turn_typing_places(&agent, &items).await;
        let _ = ctx.store.set_typing(&agent, &typing, true, None, None).await;
        let _ = ctx.store.mark_seen(&agent, &items).await;
        let result = take_turn(ctx, &agent, &items).await;
        heartbeat.abort();
        let _ = ctx.store.set_typing(&agent, &typing, false, None, None).await;
        match result {
            Ok(()) => ctx.store.finish_inbox(&ids, "done", None).await?,
            Err(e) => {
                warn!(agent = %agent.slug, error = %e, "Turn failed, requeueing events");
                ctx.store.requeue_inbox(&ids, &e.to_string()).await?;
                ctx.store
                    .set_agent_status(agent_id, "error", Some(&loka_shared::cli_errors::status_detail(&e.to_string())))
                    .await?;
                ctx.store.pause_for_rejected_key(&agent, &e.to_string()).await?;
                return Err(e);
            }
        }
    }
    if let Some(agent) = ctx.store.get_agent(agent_id).await?.filter(|a| a.status != "waiting") {
        ctx.store.set_agent_status(agent_id, "idle", agent.status_detail.as_deref()).await?;
    }
    Ok(())
}

async fn reset_context(ctx: &Ctx, agent: &WorldAgent, items: &[InboxItem]) {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    if let Err(e) = reset_steps(ctx, agent, &ids).await {
        warn!(agent = %agent.slug, error = %e, "Context reset failed");
        let _ = ctx.store.finish_inbox(&ids, "failed", Some(&loka_shared::cli_errors::error_text(&e.to_string()))).await;
    }
}

async fn compact_on_request(ctx: &Ctx, agent: &WorldAgent, items: &[InboxItem], after_reset: bool) {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let main = match ctx.store.main_session(agent.id).await {
        Ok(main) => main.filter(|main| main.cli_session_id.is_some()),
        Err(e) => {
            let _ = ctx.store.requeue_inbox(&ids, &e.to_string()).await;
            return;
        }
    };
    let compacted = match main {
        Some(main) if !after_reset && loka_shared::context::compact_unsupported(&main.backend).is_none() => {
            let _ = ctx.store.set_agent_status(agent.id, "thinking", None).await;
            let environment = ctx.store.secrets_for_agent(agent.id).await.unwrap_or_default();
            let done = compact(ctx, agent, &main, main.context_tokens, &environment).await;
            let _ = ctx.store.set_agent_status(agent.id, "idle", agent.status_detail.as_deref()).await;
            done
        }
        _ => true,
    };
    if !compacted {
        let _ = system_note(ctx, agent, "Compacting the context did not work this time. Try again, or reset the context instead.").await;
    }
    let _ = ctx.store.finish_inbox(&ids, "done", None).await;
}

async fn reset_steps(ctx: &Ctx, agent: &WorldAgent, ids: &[i64]) -> Result<()> {
    let Some(main) = ctx.store.main_session(agent.id).await? else {
        ctx.store.finish_inbox(ids, "done", None).await?;
        return Ok(());
    };
    if main.cli_session_id.is_none() && ctx.store.last_run_id(main.id).await?.is_none() {
        ctx.store.finish_inbox(ids, "done", None).await?;
        return Ok(());
    }
    ctx.warm.evict(main.id).await;
    let kit = super::kit::for_agent(ctx, agent.id, true).await?;
    let provider_keys = ctx.store.provider_keys_for_agent(agent.id).await?;
    let environment = ctx.store.secrets_for_agent(agent.id).await?;
    let omp = ctx.store.resolved_omp_config(agent.id, &main.backend).await?;
    let home = super::agent_home(ctx, &agent.slug);
    tokio::fs::create_dir_all(&home).await?;
    let system = prompts::persona(agent, &ctx.owner);
    let run = ctx
        .store
        .create_run(NewRun {
            session_id: main.id,
            agent_id: agent.id,
            task_id: None,
            kind: "turn",
            status: "running",
            fork_from: None,
            prompt: prompts::RESET_PROMPT,
            timeout_secs: TURN_TIMEOUT.as_secs() as i32,
        })
        .await?;
    let sid = match &main.cli_session_id {
        Some(sid) => Some(sid.clone()),
        None if main.backend == "claude" => Some(Uuid::new_v4().to_string()),
        None => None,
    };
    let wired = super::kit::wire(kit.servers.clone(), agent.owner_user_id, agent.id, &home).await?;
    let opts = CliOptions {
        working_dir: &home,
        prompt: prompts::RESET_PROMPT,
        system_prompt: Some(&system),
        allowed_tools: None,
        max_turns: TURN_MAX_TURNS,
        model: &main.model,
        mcp_config_json: wired.mcp_json.clone(),
        session_id: sid.as_deref(),
        resume: main.cli_session_id.is_some(),
        event_tx: None,
        image_paths: Vec::new(),
        fork: false,
        lean: true,
        effort: None,
        fast: false,
        environment: Default::default(),
        mcp_has_secrets: kit.has_secret_refs,
        sensitive: !environment.is_empty() || kit.has_secret_refs,
        rewind_to: None,
        approval: Default::default(),
        steer: Default::default(),
        omp_models: omp.map(|omp| omp.models_yml),
        provider_keys: provider_keys.clone(),
        readable_files: kit.skill_files.clone(),
    };
    let outcome = tokio::time::timeout(TURN_TIMEOUT, cli::record_spawns(wired.pids.clone(), cli::run(&main.backend, opts))).await;
    drop(wired);
    let abort = match outcome {
        Ok(Ok(result)) => {
            let output = redact_secrets(&result.output, environment.values());
            let acc = runs::account(&main, &result);
            let _ = ctx
                .store
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
                .await;
            let since = run.started_at.unwrap_or(run.created_at);
            match ctx.store.notes_updated_since(agent.id, agent.owner_user_id, since).await? {
                true => None,
                false => Some("the notes were not saved (update_my_notes was not called)".to_string()),
            }
        }
        Ok(Err(e)) => {
            let error = redact_secrets(&format!("{e:#}"), environment.values());
            finish_failed(ctx, run.id, &error).await;
            if loka_shared::cli_errors::is_context_too_long(&error) {
                system_note(ctx, agent, loka_shared::context::RESET_KEPT_NOTES).await?;
                None
            } else {
                Some(error)
            }
        }
        Err(_) => {
            finish_failed(ctx, run.id, "reset notes turn timed out").await;
            Some("the notes turn timed out".to_string())
        }
    };
    if let Some(reason) = abort {
        system_note(ctx, agent, &format!("Context reset aborted: {reason}. Nothing was forgotten; ask again when ready.")).await?;
        ctx.store.finish_inbox(ids, "done", None).await?;
        return Ok(());
    }
    let notes = ctx
        .store
        .own_notes_skill(agent.id, agent.owner_user_id)
        .await?
        .map(|skill| skill.body)
        .unwrap_or_default();
    let fresh = ctx.store.restart_main_session(agent.id, &agent.cli, &agent.model, &notes).await?;
    ctx.store.mark_context_reset(agent.id, fresh.id, main.id).await?;
    ctx.store.finish_inbox(ids, "done", None).await?;
    info!(agent = %agent.slug, old = %main.id, new = %fresh.id, "Main session reset after saving notes");
    Ok(())
}

async fn take_turn(ctx: &Ctx, agent: &WorldAgent, items: &[InboxItem]) -> Result<()> {
    let turn_started = Instant::now();
    let (main, handoff) = ensure_main_session(ctx, agent).await?;
    let tasks = referenced_tasks(ctx, items).await;
    let rules = ctx.store.rules_for(agent.owner_user_id, agent.org_id).await?;
    let kit = super::kit::for_agent(ctx, agent.id, true).await?;
    let provider_keys = ctx.store.provider_keys_for_agent(agent.id).await?;
    let environment = ctx.store.secrets_for_agent(agent.id).await?;
    let omp = ctx.store.resolved_omp_config(agent.id, &main.backend).await?;
    let teams = ctx.store.teams_of_agent(agent.id).await.unwrap_or_default();
    let rules_text = [prompts::rules(&rules, "talk", &ctx.owner), prompts::teams(&teams, agent.id, "talk"), kit.skills_index.clone()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let system = prompts::persona(agent, &ctx.owner);
    let rules_hash = prompts::rules_hash(&format!("{system}\n{rules_text}"));
    let fresh = main.cli_session_id.is_none();
    let mut prompt = prompts::turn(items, &tasks);
    if !fresh {
        let last = prompts::last_replies(&ctx.store.last_turn_replies(agent.id, main.id).await.unwrap_or_default());
        if !last.is_empty() {
            prompt = format!("{last}\n\n{prompt}");
        }
    }
    let memory = handoff.as_deref().or(main.summary.as_deref());
    prompt = prompts::prepend_memory_handoff(&prompt, memory, fresh);
    if main.rules_hash.as_deref() != Some(rules_hash.as_str()) {
        let update = if fresh {
            rules_text.clone()
        } else {
            format!("[instructions updated] These replace your earlier instructions:\n{system}\n\n{rules_text}")
        };
        if !update.trim().is_empty() {
            prompt = format!("[rules updated]\n{update}\n\n{prompt}");
        }
    }
    let home = super::agent_home(ctx, &agent.slug);
    tokio::fs::create_dir_all(&home).await?;
    let image_paths = materialize_uploads(ctx, &home, items).await;
    let mut options = TurnOptions::from_items(items);
    if main.backend == "claude" && !options.fast {
        options.fast = ctx.store.any_urgent(&loka_shared::quests::inbox_task_ids(items)).await.unwrap_or(false);
    }

    let rewind = Rewind::from(main.rewind.as_ref());
    if !matches!(rewind, Rewind::None) {
        ctx.warm.evict(main.id).await;
    }
    let (sid, resume) = match (&rewind, main.cli_session_id.clone()) {
        (Rewind::At { from, .. }, _) => (Some(from.clone()), true),
        (_, Some(sid)) => (Some(sid), true),
        (_, None) if main.backend == "claude" => (Some(Uuid::new_v4().to_string()), false),
        (_, None) => (None, false),
    };
    let metered = !super::warm::reports_usage(&main.backend) && ctx.store.metered(main.agent_id).await.unwrap_or(true);
    let warm = matches!(rewind, Rewind::None)
        && super::warm::supports_model(&main.backend, options.model.as_deref().unwrap_or(&main.model))
        && !metered
        && ctx.warm.enabled();
    let latest_run = if warm { ctx.store.last_run_id(main.id).await? } else { None };
    let run = ctx
        .store
        .create_run(NewRun {
            session_id: main.id,
            agent_id: agent.id,
            task_id: None,
            kind: "turn",
            status: "running",
            fork_from: None,
            prompt: &prompt,
            timeout_secs: TURN_TIMEOUT.as_secs() as i32,
        })
        .await?;
    let (progress_tx, progress) = spawn_progress(ctx.store.clone(), agent.id, run.id);
    let pids = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let progress_tx = runs::record_pids(progress_tx, pids.clone());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<String>();
    let stop_watch = runs::spawn_stop_watch(ctx, run.id, stop_tx);
    let wired = super::kit::wire(kit.servers.clone(), agent.owner_user_id, agent.id, &home).await?;
    let opts = CliOptions {
        working_dir: &home,
        prompt: &prompt,
        system_prompt: Some(&system),
        allowed_tools: None,
        max_turns: TURN_MAX_TURNS,
        model: options.model.as_deref().unwrap_or(&main.model),
        mcp_config_json: wired.mcp_json.clone(),
        session_id: sid.as_deref(),
        resume,
        event_tx: Some(progress_tx),
        image_paths,
        fork: false,
        lean: true,
        effort: options.effort.as_deref(),
        fast: options.fast,
        environment: Default::default(),
        mcp_has_secrets: kit.has_secret_refs,
        sensitive: !environment.is_empty() || kit.has_secret_refs,
        rewind_to: rewind.anchor(),
        approval: Default::default(),
        steer: Default::default(),
        omp_models: omp.map(|omp| omp.models_yml),
        provider_keys: provider_keys.clone(),
        readable_files: kit.skill_files.clone(),
    };
    let prep_ms = turn_started.elapsed().as_millis() as u64;
    let cli_started = Instant::now();
    let call = async {
        if warm {
            match ctx.warm.turn(&main.backend, main.id, latest_run, run.id, &opts).await {
                Err(e) if crate::acp::is_unoffered(&e) => cli::run(&main.backend, opts).await.map(|r| (r, None)),
                other => other.map(|(r, t)| (r, Some(t))),
            }
        } else if main.backend == "claude" {
            crate::claude::run_timed(opts).await.map(|(r, t)| (r, Some(t)))
        } else {
            cli::run(&main.backend, opts).await.map(|r| (r, None))
        }
    };
    let outcome = tokio::select! {
        outcome = tokio::time::timeout(TURN_TIMEOUT, cli::record_spawns(wired.pids.clone(), call)) => outcome,
        Ok(by) = stop_rx => {
            let roots = std::mem::take(&mut *pids.lock().unwrap());
            cli::kill_process_trees(roots).await;
            ctx.warm.evict(main.id).await;
            Ok(Err(anyhow!("Stopped by {by}.")))
        }
    };
    stop_watch.abort();
    drop(wired);
    progress.abort();
    let (result, timing) = match outcome {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let error = redact_secrets(&format!("{e:#}"), environment.values());
            finish_failed(ctx, run.id, &error).await;
            if rewind.anchor().is_some() && ctx.store.fall_back_to_replay(main.id).await.unwrap_or(false) {
                warn!(agent = %agent.slug, error = %error, "CLI rewind failed, restarting from the kept transcript");
            }
            return Err(anyhow!(error));
        }
        Err(_) => {
            finish_failed(ctx, run.id, "turn timed out").await;
            return Err(anyhow!("turn timed out"));
        }
    };
    let cli_ms = cli_started.elapsed().as_millis() as u64;
    let post_started = Instant::now();
    let output = redact_secrets(&result.output, environment.values());
    let acc = runs::account(&main, &result);
    ctx.store
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
            main.id,
            result.session_id.as_deref().or(sid.as_deref()),
            acc.baseline,
            runs::context_tokens(&result.usage),
        )
        .await?;
    let _ = ctx.store.announce_context(agent.id).await;
    ctx.store.set_session_rules_hash(main.id, &rules_hash).await?;
    ctx.store.set_run_checkpoint(run.id, result.checkpoint.as_deref()).await?;
    if !matches!(rewind, Rewind::None) {
        ctx.store.clear_session_rewind(main.id).await?;
        info!(agent = %agent.slug, "Main session rewound");
    }
    let main = ctx.store.get_session(main.id).await?.unwrap_or(main);

    let (text, actions) = prompts::split_actions(&result.output);
    let text = redact_secrets(&text, environment.values());
    let actions = actions
        .into_iter()
        .map(|action| redact_action(action, &environment))
        .collect::<Vec<_>>();
    info!(agent = %agent.slug, events = items.len(), actions = actions.len(), cost = acc.cost_usd, "Turn complete");
    apply_actions(ctx, agent, &main, run.id, items, &text, actions).await?;
    log_timing(agent, &main.backend, prep_ms, cli_ms, post_started.elapsed().as_millis() as u64, turn_started, timing.as_ref());
    let used = if main.backend == "claude" { runs::context_tokens(&result.usage) } else { result.usage.context_tokens as i64 };
    if let Some(limit) = prompts::compact_threshold().filter(|limit| prompts::compacts(&main.backend) && used > *limit) {
        if ctx.store.should_compact(main.id, run.id, used, limit).await.unwrap_or(true)
            && compact(ctx, agent, &main, used, &environment).await
        {
            let _ = ctx.store.announce_context(agent.id).await;
        }
    }
    if let Err(e) = ctx.store.auto_context_cleanup(agent.id, run.id).await {
        warn!(agent = %agent.slug, error = %e, "Automatic context cleanup check failed");
    }
    Ok(())
}

fn log_timing(
    agent: &WorldAgent,
    backend: &str,
    prep_ms: u64,
    cli_ms: u64,
    post_ms: u64,
    started: Instant,
    timing: Option<&crate::cli::Timing>,
) {
    let total_ms = started.elapsed().as_millis() as u64;
    let Some(t) = timing else {
        info!(agent = %agent.slug, backend, total_ms, prep_ms, cli_ms, post_ms, "Turn timing");
        return;
    };
    let summary_ms = t.result_ms.zip(t.last_output_ms).map(|(r, l)| r.saturating_sub(l));
    info!(
        agent = %agent.slug,
        backend,
        warm = t.warm,
        spawned = t.spawned,
        total_ms,
        prep_ms,
        cli_ms,
        post_ms,
        spawn_ms = t.spawn_ms,
        init_ms = ?t.init_ms,
        first_output_ms = ?t.first_output_ms,
        last_output_ms = ?t.last_output_ms,
        result_ms = ?t.result_ms,
        summary_ms = ?summary_ms,
        api_ms = ?t.api_ms,
        model_turns = ?t.model_turns,
        tool_calls = t.tool_calls,
        "Turn timing"
    );
    let labels = [("backend", backend), ("start", if t.spawned { "cold" } else { "warm" })];
    if let Some(ms) = t.first_output_ms {
        loka_shared::telemetry::observe("turn_first_output_seconds", &labels, ms as f64 / 1000.0);
    }
    loka_shared::telemetry::observe("turn_result_seconds", &labels, total_ms as f64 / 1000.0);
}

fn redact_secrets<'a>(text: &str, values: impl Iterator<Item = &'a String>) -> String {
    let mut values: Vec<&String> = values.chain(crate::cli::host_secrets()).filter(|value| !value.is_empty()).collect();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
        .into_iter()
        .fold(text.replace('\0', ""), |text, value| text.replace(value, "[REDACTED]"))
}

fn redact_action(
    action: Action,
    environment: &std::collections::BTreeMap<String, String>,
) -> Action {
    let redact = |text: String| redact_secrets(&text, environment.values());
    match action {
        Action::Reply { text, next } => Action::Reply { text: redact(text), next: next.into_iter().map(redact).collect() },
        Action::StartTask { task_id, brief, review, reviewer, time_limit_minutes, milestones, quest } => Action::StartTask {
            task_id,
            brief: brief.map(redact),
            review,
            reviewer,
            time_limit_minutes,
            milestones: milestones.into_iter().map(redact).collect(),
            quest,
        },
        Action::Status { text } => Action::Status { text: redact(text) },
        Action::Escalate { text } => Action::Escalate { text: redact(text) },
        Action::Ask { text, choices } => Action::Ask {
            text: redact(text),
            choices: choices.into_iter().map(redact).collect(),
        },
        Action::Drafts { text, items } => Action::Drafts {
            text: redact(text),
            items: items
                .into_iter()
                .map(|d| prompts::Draft {
                    to: d.to.map(redact),
                    subject: d.subject.map(redact),
                    body: redact(d.body),
                    diff: d.diff.map(redact),
                    ..d
                })
                .collect(),
        },
    }
}

async fn compact(
    ctx: &Ctx,
    agent: &WorldAgent,
    main: &AgentSession,
    used: i64,
    environment: &std::collections::BTreeMap<String, String>,
) -> bool {
    let Some(sid) = main.cli_session_id.clone() else {
        return false;
    };
    info!(agent = %agent.slug, tokens = used, backend = %main.backend, "Compacting main session");
    ctx.warm.evict(main.id).await;
    let home = super::agent_home(ctx, &agent.slug);
    let omp_models = ctx.store.resolved_omp_config(agent.id, &main.backend).await.ok().flatten().map(|omp| omp.models_yml);
    let Ok(run) = ctx
        .store
        .create_run(NewRun {
            session_id: main.id,
            agent_id: agent.id,
            task_id: None,
            kind: "turn",
            status: "running",
            fork_from: None,
            prompt: prompts::COMPACT_PROMPT,
            timeout_secs: TURN_TIMEOUT.as_secs() as i32,
        })
        .await
    else {
        return false;
    };
    let opts = CliOptions {
        working_dir: &home,
        prompt: prompts::COMPACT_PROMPT,
        system_prompt: None,
        allowed_tools: None,
        max_turns: 1,
        model: &main.model,
        mcp_config_json: None,
        session_id: Some(&sid),
        resume: true,
        event_tx: None,
        image_paths: Vec::new(),
        fork: false,
        lean: true,
        effort: None,
        fast: false,
        environment: Default::default(),
        mcp_has_secrets: false,
        sensitive: !environment.is_empty(),
        rewind_to: None,
        approval: Default::default(),
        steer: Default::default(),
        omp_models,
        provider_keys: ctx.store.provider_keys_for_agent(agent.id).await.unwrap_or_default(),
        readable_files: Vec::new(),
    };
    match tokio::time::timeout(TURN_TIMEOUT, cli::run(&main.backend, opts)).await {
        Ok(Ok(result)) => {
            let acc = runs::account(main, &result);
            let _ = ctx
                .store
                .finish_run(
                    run.id,
                    RunOutcome {
                        status: "completed",
                        output: Some("compacted"),
                        error: None,
                        cost_usd: acc.cost_usd,
                        input_tokens: acc.usage.input_tokens as i64,
                        cached_input_tokens: acc.usage.cached_input_tokens as i64,
                        output_tokens: acc.usage.output_tokens as i64,
                    },
                )
                .await;
            let _ = ctx.store.update_session_after_run(main.id, result.session_id.as_deref(), acc.baseline, 0).await;
            let _ = ctx.store.set_session_rules_hash(main.id, "").await;
            let _ = ctx.store.set_run_checkpoint(run.id, result.checkpoint.as_deref()).await;
            let _ = ctx
                .store
                .emit(NewEvent::new("session.compacted").agent(agent.id).session(main.id).payload(json!({ "tokens_before": used })), None)
                .await;
            true
        }
        Ok(Err(e)) => {
            let error = redact_secrets(&e.to_string(), environment.values());
            finish_failed(ctx, run.id, &error).await;
            false
        }
        Err(_) => {
            finish_failed(ctx, run.id, "compaction timed out").await;
            false
        }
    }
}

async fn apply_actions(
    ctx: &Ctx,
    agent: &WorldAgent,
    main: &AgentSession,
    run_id: Uuid,
    items: &[InboxItem],
    text: &str,
    actions: Vec<Action>,
) -> Result<()> {
    let mut replied = false;
    let mut started: Vec<Uuid> = Vec::new();
    let thread = ctx.store.reply_thread(agent, items).await?;
    let root = loka_shared::replies::reply_root(items);
    for action in &actions {
        match action {
            Action::Reply { text, next } if !text.trim().is_empty() => {
                reply(ctx, agent, main, run_id, text, next, thread, root).await?;
                replied = true;
            }
            Action::StartTask { task_id, brief, review, reviewer, time_limit_minutes, milestones, quest } => match task_id.parse::<Uuid>() {
                Ok(id) if !started.contains(&id) && !super::quests::assigned_elsewhere(&ctx.store, id, agent.id).await => {
                    if let Err(e) = super::quests::apply_choice(&ctx.store, id, quest).await {
                        system_note(ctx, agent, &format!("Could not set the quest options on task {id}: {e}")).await?;
                        continue;
                    }
                    super::quests::seed_milestones(&ctx.store, agent.id, id, milestones).await;
                    let choice = reviewer.as_ref().and_then(ReviewerChoice::from_value);
                    match runs::start_task(ctx, agent, main, id, brief.as_deref(), review.unwrap_or(false) || choice.is_some(), choice.as_ref()).await {
                        Ok(()) => {
                            started.push(id);
                            if let Some(minutes) = time_limit_minutes {
                                if let Err(e) = ctx.store.set_task_time_limit(id, Some(*minutes)).await {
                                    system_note(ctx, agent, &format!("Could not set the time limit on task {id}: {e}")).await?;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(agent = %agent.slug, task = %id, error = %e, "Could not start task");
                            system_note(ctx, agent, &format!("Could not start task {id}: {e}")).await?;
                        }
                    }
                }
                _ => {}
            },
            Action::Escalate { text } | Action::Ask { text, .. } | Action::Drafts { text, .. } if !text.trim().is_empty() => {
                let drafts = match action {
                    Action::Drafts { items, .. } => prompts::clean_drafts(items),
                    _ => Vec::new(),
                };
                let text = if drafts.is_empty() { prompts::plain_dashes(text) } else { prompts::drafts_body(text, &drafts) };
                let text = ctx.store.expand_message_links(&text, agent.id).await;
                let choices: Vec<String> = match action {
                    Action::Ask { choices, .. } => choices
                        .iter()
                        .map(|c| prompts::plain_dashes(c.trim()))
                        .filter(|c| !c.is_empty())
                        .take(4)
                        .collect(),
                    _ => Vec::new(),
                };
                let msg = ctx
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
                ctx.store
                    .emit(
                        NewEvent::new("message.created")
                            .agent(agent.id)
                            .session(main.id)
                            .run(run_id)
                            .payload(json!({ "message_id": msg.id, "author": agent.name, "author_kind": "agent", "body": text, "ask": true })),
                        None,
                    )
                    .await?;
                replied = true;
                ctx.store
                    .emit(
                        NewEvent::new("attention.requested")
                            .agent(agent.id)
                            .session(main.id)
                            .run(run_id)
                            .payload(json!({ "text": text, "agent": agent.name, "message_id": msg.id, "choices": choices })),
                        None,
                    )
                    .await?;
                ctx.store.set_agent_status(agent.id, "waiting", Some(&truncate(&text, 120))).await?;
            }
            Action::Status { text } => {
                ctx.store.set_agent_status(agent.id, "thinking", Some(&truncate(text, 120))).await?;
            }
            _ => {}
        }
    }
    let had_message = items.iter().any(|i| {
        i.kind == "message" && i.payload.get("author_kind").and_then(Value::as_str) != Some("agent")
    });
    if had_message && !replied && !text.trim().is_empty() {
        reply(ctx, agent, main, run_id, text, &[], thread, root).await?;
        replied = true;
    }
    for item in items.iter().filter(|i| {
        i.kind == "task.assigned" && i.payload.get("subscribed").and_then(Value::as_bool) != Some(true)
    }) {
        let Some(id) = item.payload.get("task_id").and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok()) else {
            continue;
        };
        if started.contains(&id)
            || replied
            || ctx.store.quest_has_parts(id).await.unwrap_or(false)
            || super::quests::assigned_elsewhere(&ctx.store, id, agent.id).await
        {
            continue;
        }
        let brief = (!text.trim().is_empty()).then_some(text);
        if let Err(e) = runs::start_task(ctx, agent, main, id, brief, false, None).await {
            warn!(agent = %agent.slug, task = %id, error = %e, "Could not auto-start task");
            system_note(ctx, agent, &format!("Could not start task {id}: {e}")).await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn reply(ctx: &Ctx, agent: &WorldAgent, main: &AgentSession, run_id: Uuid, text: &str, next: &[String], thread: Option<Uuid>, root: Option<Uuid>) -> Result<()> {
    let text = &ctx.store.expand_message_links(&prompts::plain_dashes(text), agent.id).await;
    let msg = ctx
        .store
        .insert_reply(NewMessage {
            agent_id: agent.id,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent.id),
            body: text,
            session_id: Some(main.id),
            run_id: Some(run_id),
            meta: prompts::with_next(in_thread(Value::Null, thread), next),
        }, root)
        .await?;
    ctx.store
        .emit(
            NewEvent::new("message.created")
                .agent(agent.id)
                .session(main.id)
                .run(run_id)
                .payload(in_thread(json!({ "message_id": msg.id, "author": agent.name, "author_kind": "agent", "body": text, "reply_to": msg.reply_to }), thread)),
            None,
        )
        .await?;
    Ok(())
}

#[derive(Default)]
struct TurnOptions {
    model: Option<String>,
    effort: Option<String>,
    fast: bool,
}

impl TurnOptions {
    fn from_items(items: &[InboxItem]) -> Self {
        let Some(o) = items.iter().rev().find_map(|i| i.payload.get("options").filter(|o| o.is_object())) else {
            return Self::default();
        };
        let text = |k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.len() <= 80 && !v.starts_with('-'))
                .map(String::from)
        };
        Self {
            model: text("model"),
            effort: text("effort").filter(|e| matches!(e.as_str(), "minimal" | "low" | "medium" | "high" | "xhigh" | "max")),
            fast: o.get("fast").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

async fn materialize_uploads(ctx: &Ctx, home: &std::path::Path, items: &[InboxItem]) -> Vec<std::path::PathBuf> {
    let ids: Vec<Uuid> = items
        .iter()
        .filter_map(|i| i.payload.get("attachments").and_then(Value::as_array))
        .flatten()
        .filter(|a| a.get("mime").and_then(Value::as_str).is_none_or(prompts::is_image))
        .filter_map(|a| a.get("id").and_then(Value::as_str).and_then(|s| s.parse().ok()))
        .take(8)
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let dir = home.join(".uploads");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        warn!(error = %e, "Could not create uploads dir");
        return Vec::new();
    }
    let mut paths = Vec::new();
    for id in ids {
        match ctx.store.get_upload(id, None).await {
            Ok(Some((mime, bytes))) => {
                let ext = match mime.as_str() {
                    "image/jpeg" => "jpg",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    _ => "png",
                };
                let path = dir.join(format!("{id}.{ext}"));
                match tokio::fs::write(&path, bytes).await {
                    Ok(()) => paths.push(path),
                    Err(e) => warn!(error = %e, "Could not write upload"),
                }
            }
            Ok(None) => warn!(upload = %id, "Upload missing"),
            Err(e) => warn!(error = %e, "Could not read upload"),
        }
    }
    paths
}

async fn system_note(ctx: &Ctx, agent: &WorldAgent, text: &str) -> Result<()> {
    ctx.store
        .insert_message(NewMessage {
            agent_id: agent.id,
            author_kind: "system",
            author_user_id: None,
            author_agent_id: None,
            body: text,
            session_id: None,
            run_id: None,
            meta: serde_json::Value::Null,
        })
        .await?;
    Ok(())
}

async fn finish_failed(ctx: &Ctx, run_id: Uuid, error: &str) {
    let _ = ctx
        .store
        .finish_run(
            run_id,
            RunOutcome {
                status: "failed",
                output: None,
                error: Some(error),
                cost_usd: 0.0,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
            },
        )
        .await;
}

async fn ensure_main_session(ctx: &Ctx, agent: &WorldAgent) -> Result<(AgentSession, Option<String>)> {
    let mut handoff = None;
    if let Some(s) = ctx.store.main_session(agent.id).await? {
        if s.backend == agent.cli {
            if s.model != agent.model {
                ctx.store.set_session_model(s.id, &agent.model).await?;
                info!(agent = %agent.slug, from = %s.model, to = %agent.model, "Switched main session model");
                return Ok((ctx.store.get_session(s.id).await?.unwrap_or(s), None));
            }
            return Ok((s, None));
        }
        handoff = Some(write_handoff(ctx, agent, &s).await?);
        ctx.store.set_session_status(s.id, "archived", handoff.as_deref()).await?;
        info!(agent = %agent.slug, from = %s.backend, to = %agent.cli, "Moved agent to a new backend");
    }
    let home = super::agent_home(ctx, &agent.slug).to_string_lossy().to_string();
    let session = ctx
        .store
        .create_session(NewSession {
            agent_id: agent.id,
            role: "main",
            backend: &agent.cli,
            model: &agent.model,
            parent_session_id: None,
            task_id: None,
            working_dir: Some(&home),
            usage_baseline: json!({}),
        })
        .await?;
    info!(agent = %agent.slug, session = %session.id, "Created main session");
    Ok((session, handoff))
}

async fn write_handoff(ctx: &Ctx, agent: &WorldAgent, old: &AgentSession) -> Result<String> {
    ctx.warm.evict(old.id).await;
    let Some(sid) = old.cli_session_id.clone() else {
        return Ok("(no earlier memory)".to_string());
    };
    let provider_keys = ctx.store.provider_keys_for_agent(agent.id).await?;
    let environment = ctx.store.secrets_for_agent(agent.id).await?;
    let prompt = "[handoff] You are about to move to a new mind and this conversation will end. Write down everything your next self needs to stay you: who you are and how you talk, the people you work with and what they like, every task you worked on with its outcome and files, the decisions made and why, promises you made, and anything you were about to do. The brevity rules do not apply to this one message: be complete, use as many lines as needed. Plain text, no actions block.";
    let home = super::agent_home(ctx, &agent.slug);
    let run = ctx
        .store
        .create_run(NewRun {
            session_id: old.id,
            agent_id: agent.id,
            task_id: None,
            kind: "turn",
            status: "running",
            fork_from: None,
            prompt,
            timeout_secs: TURN_TIMEOUT.as_secs() as i32,
        })
        .await;
    let opts = CliOptions {
        working_dir: &home,
        prompt,
        system_prompt: None,
        allowed_tools: None,
        max_turns: TURN_MAX_TURNS,
        model: &old.model,
        mcp_config_json: None,
        session_id: Some(&sid),
        resume: true,
        event_tx: None,
        image_paths: Vec::new(),
        fork: false,
        lean: true,
        effort: None,
        fast: false,
        environment: Default::default(),
        mcp_has_secrets: false,
        sensitive: !environment.is_empty(),
        rewind_to: None,
        approval: Default::default(),
        steer: Default::default(),
        omp_models: ctx.store.resolved_omp_config(agent.id, &old.backend).await.ok().flatten().map(|omp| omp.models_yml),
        provider_keys: provider_keys.clone(),
        readable_files: Vec::new(),
    };
    let work = recent_work(ctx, agent).await;
    let narrative = match tokio::time::timeout(TURN_TIMEOUT, cli::run(&old.backend, opts)).await {
        Ok(Ok(result)) => {
            let output = redact_secrets(&result.output, environment.values());
            if let Ok(run) = run {
                let acc = runs::account(old, &result);
                let _ = ctx
                    .store
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
                    .await;
            }
            prompts::split_actions(&output).0
        }
        other => {
            let error = redact_secrets(&match other {
                Ok(Err(e)) => e.to_string(),
                _ => "timed out".to_string(),
            }, environment.values());
            warn!(agent = %agent.slug, error = %error, "Handoff failed; starting fresh");
            if let Ok(run) = run {
                finish_failed(ctx, run.id, &error).await;
            }
            format!("(your previous mind could not write a handoff: {error})")
        }
    };
    if work.is_empty() {
        Ok(narrative)
    } else {
        Ok(format!("{narrative}\n\nYour recent quests, from the Loka records:\n{work}"))
    }
}

async fn recent_work(ctx: &Ctx, agent: &WorldAgent) -> String {
    match ctx.store.agent_tasks(Some(agent.id), false, 20).await {
        Ok(tasks) => tasks
            .into_iter()
            .map(|t| {
                let mut line = format!("- [{}] {}", t.status, t.title);
                if let Some(pr) = t.pr_url {
                    line.push_str(&format!(" (PR {pr})"));
                }
                if let Some(r) = t.result_content.filter(|r| !r.trim().is_empty()) {
                    line.push_str(&format!(": {}", r.chars().take(300).collect::<String>()));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => {
            warn!(error = %e, "Could not load recent work for handoff");
            String::new()
        }
    }
}

async fn referenced_tasks(ctx: &Ctx, items: &[InboxItem]) -> Vec<AgentTask> {
    let mut tasks = Vec::new();
    for item in items {
        if let Some(id) = item
            .payload
            .get("task_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Uuid>().ok())
        {
            if tasks.iter().any(|t: &AgentTask| t.id == id) {
                continue;
            }
            if let Ok(Some(t)) = ctx.db.get_task(id).await {
                tasks.push(t);
            }
        }
    }
    tasks
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::redact_action;
    use crate::world::prompts::{self, Action};
    use std::collections::BTreeMap;

    #[test]
    fn preserves_action_type_when_secret_matches_control_value() {
        let environment = BTreeMap::from([("TOKEN".to_string(), "start_task".to_string())]);
        let (_, actions) = prompts::split_actions(
            r#"<!-- actions [{"type":"start_task","task_id":"abc","brief":"start_task"}] actions -->"#,
        );
        let action = redact_action(actions.into_iter().next().unwrap(), &environment);
        assert!(matches!(
            action,
            Action::StartTask { task_id, brief, .. }
                if task_id == "abc" && brief.as_deref() == Some("[REDACTED]")
        ));
    }
}

fn spawn_progress(
    store: loka_shared::world::WorldStore,
    agent_id: Uuid,
    run_id: Uuid,
) -> (cli::EventSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<cli::StreamEvent>();
    let task = tokio::spawn(async move {
        let mut last = Instant::now() - PROGRESS_THROTTLE;
        while let Some(event) = rx.recv().await {
            let cli::StreamEvent::ToolUse { tool, input_summary } = event else {
                continue;
            };
            if last.elapsed() < PROGRESS_THROTTLE {
                continue;
            }
            last = Instant::now();
            let text = loka_shared::progress::phrase(&tool, &input_summary);
            let _ = store.emit_progress(agent_id, run_id, "turn", &text).await;
        }
    });
    (tx, task)
}
