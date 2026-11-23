use std::borrow::Cow;
use std::path::Path;

use anyhow::{anyhow, Result};
use loka_shared::context::{ForkChoice, QuestContext};
use loka_shared::db::Database;
use loka_shared::milestones::{self, MilestoneStore};
use loka_shared::models::AgentTask;
use loka_shared::quests::{BestOfAttempt, QuestChoice, QuestOptions, AUTOMATIC_PLAN, PLAN_FIRST};
use loka_shared::pr_actions;
use loka_shared::world::BeforeBigWork;
use loka_shared::world::AgentSchedule;
use loka_shared::world::{AgentSession, InboxItem, NewEvent, NewMessage, NewRun, NewSession, ReviewerChoice, SessionRun, TaskRef, WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

use super::prompts;

pub enum QuestEvidence<'a> {
    Merged(&'a str),
    Completed {
        report: &'a Value,
        parsed: &'a prompts::Report,
        error: Option<&'a str>,
        pr_url: Option<&'a str>,
        changes_requested: bool,
        suppress_announcement: bool,
    },
}

pub struct ReconciledQuest {
    pub task: Option<AgentTask>,
    pub status: &'static str,
    pub watching: bool,
    pub announce: bool,
}

pub struct QuestReconciliation {
    pub completed: Option<ReconciledQuest>,
}

pub async fn reconcile_quest(
    store: &WorldStore,
    db: &Database,
    session: &AgentSession,
    evidence: QuestEvidence<'_>,
) -> Result<QuestReconciliation> {
    match evidence {
        QuestEvidence::Merged(pr_url) => {
            if let Some(task_id) = session.task_id {
                if store.settle_pr(task_id, pr_url, true).await? {
                    store.set_session_status(session.id, "completed", None).await?;
                }
            }
            Ok(QuestReconciliation { completed: None })
        }
        QuestEvidence::Completed { report, parsed, error, pr_url, changes_requested, suppress_announcement } => {
            let duplicate = store.finished_report_unchanged(session.id, report, pr_url, error).await?;
            let task = match session.task_id {
                Some(task_id) => db.get_task(task_id).await?,
                None => None,
            };
            let completion = match task.as_ref() {
                Some(task) => store
                    .reconcile_task_completion(
                        task.id,
                        report.get("summary").and_then(Value::as_str),
                        &parsed.milestone_positions(),
                        &parsed.milestone_titles(),
                        error,
                        pr_url,
                        changes_requested,
                    )
                    .await?,
                None => None,
            };
            let (status, watching) = completion.unwrap_or(("failed", false));
            if task.is_none() || completion.is_some() {
                store
                    .set_session_status(session.id, if error.is_some() { "failed" } else { "completed" }, None)
                    .await?;
            }
            let announce = (task.is_none() || completion.is_some()) && !duplicate && !suppress_announcement;
            Ok(QuestReconciliation {
                completed: Some(ReconciledQuest { task, status, watching, announce }),
            })
        }
    }
}

pub async fn apply_choice(store: &WorldStore, task_id: Uuid, choice: &QuestChoice) -> Result<()> {
    let valid = choice.validate().map_err(|e| anyhow!(e))?;
    store.set_quest_options(task_id, &valid).await
}

pub async fn seed_milestones(store: &WorldStore, agent_id: Uuid, task_id: Uuid, titles: &[String]) {
    if titles.is_empty() {
        return;
    }
    if let Err(error) = MilestoneStore::new(store.pool().clone()).seed(task_id, agent_id, titles).await {
        tracing::warn!(task = %task_id, error = %error, "Could not add milestones to the quest");
    }
}

pub async fn assigned_elsewhere(store: &WorldStore, task_id: Uuid, agent_id: Uuid) -> bool {
    sqlx::query_scalar::<_, Option<Uuid>>("SELECT assigned_agent_id FROM agent_tasks WHERE id = $1")
        .bind(task_id)
        .fetch_optional(store.pool())
        .await
        .ok()
        .flatten()
        .flatten()
        .is_some_and(|assignee| assignee != agent_id)
}

pub const MIN_TASK_PREFIX: usize = 8;

pub fn unique_task(raw: &str, matches: &[TaskRef], action: &str) -> Result<TaskRef, String> {
    let raw = raw.trim();
    match matches {
        [] => Err(format!("Could not {action} {raw}: no task of yours has that id. list_tasks shows the ids.")),
        [one] => Ok(one.clone()),
        many => Err(format!(
            "Could not {action} {raw}: it matches {} tasks ({}). Use more characters of the id.",
            many.len(),
            many.iter().map(|t| format!("{} {}", t.id, t.title)).collect::<Vec<_>>().join(", ")
        )),
    }
}

#[derive(Debug, PartialEq)]
pub enum TaskKey {
    Full(Uuid),
    Prefix(String),
}

pub fn task_key(raw: &str, action: &str) -> Result<TaskKey, String> {
    let raw = raw.trim();
    if let Ok(id) = raw.parse::<Uuid>() {
        return Ok(TaskKey::Full(id));
    }
    let hex = raw.len() >= MIN_TASK_PREFIX && raw.len() < 32 && raw.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if hex {
        Ok(TaskKey::Prefix(raw.to_lowercase()))
    } else {
        Err(format!("Could not {action} {raw}: that is not a task id. Use the full id or its first {MIN_TASK_PREFIX} characters."))
    }
}

pub async fn find_task(store: &WorldStore, org: Option<Uuid>, raw: &str, action: &str) -> Result<TaskRef, String> {
    let key = match task_key(raw, action)? {
        TaskKey::Full(id) => id.to_string(),
        TaskKey::Prefix(prefix) => prefix,
    };
    let matches = store
        .find_tasks_by_prefix_in(org, &key)
        .await
        .map_err(|e| format!("Could not {action} {key}: looking it up failed ({e})."))?;
    unique_task(&key, &matches, action)
}

pub async fn start_target(store: &WorldStore, agent: &WorldAgent, raw: &str) -> Result<Uuid, String> {
    let id = find_task(store, agent.org_id, raw, "start task").await?.id;
    if assigned_elsewhere(store, id, agent.id).await {
        return Err(format!("Could not start task {id}: it is assigned to another agent. Ask that agent, or reassign it first."));
    }
    Ok(id)
}

pub struct Started {
    pub resumed: bool,
    pub session_id: Uuid,
    pub run_id: Uuid,
}

pub const PROBLEM_KIND: &str = "actions.problem";

pub async fn tell_agent(store: &WorldStore, agent: &WorldAgent, run_id: Uuid, items: &[InboxItem], problems: &[String]) -> Result<()> {
    let mut fresh = Vec::new();
    for problem in problems {
        let known = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM agent_messages WHERE agent_id = $1 AND author_kind = 'system' AND run_id = $2 AND body = $3)",
        )
        .bind(agent.id)
        .bind(run_id)
        .bind(problem)
        .fetch_one(store.pool())
        .await?;
        if known {
            continue;
        }
        store
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: None,
                body: problem,
                session_id: None,
                run_id: Some(run_id),
                meta: Value::Null,
            })
            .await?;
        fresh.push(problem.clone());
    }
    if fresh.is_empty() {
        return Ok(());
    }
    let wake = problem_wake(store, agent.id, items).await.then_some((agent.id, PROBLEM_KIND));
    store
        .emit(NewEvent::new(PROBLEM_KIND).agent(agent.id).run(run_id).payload(json!({ "problems": fresh })), wake)
        .await?;
    Ok(())
}

const PROBLEM_WAKES_PER_HOUR: i64 = 3;

async fn problem_wake(store: &WorldStore, agent_id: Uuid, items: &[InboxItem]) -> bool {
    if !items.iter().any(|item| item.kind == PROBLEM_KIND) {
        return true;
    }
    let recent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE agent_id = $1 AND type = $2 AND created_at > NOW() - INTERVAL '1 hour'")
        .bind(agent_id)
        .bind(PROBLEM_KIND)
        .fetch_one(store.pool())
        .await
        .unwrap_or(PROBLEM_WAKES_PER_HOUR);
    recent < PROBLEM_WAKES_PER_HOUR
}

pub const REPO_NOTES_PROMPT_CAP: usize = 4096;

pub struct Wrapup {
    pub wake: bool,
    pub pace: String,
    pub timing: Value,
    pub decisions: Option<String>,
}

impl Wrapup {
    pub fn add_decisions(&self, payload: &mut Value) {
        if let (Some(decisions), Some(map)) = (&self.decisions, payload.as_object_mut()) {
            map.insert("decisions".into(), json!(decisions));
        }
    }
}

pub fn wakes_main(options: &QuestOptions, report: &prompts::Report, error: Option<&str>, changes_requested: bool) -> bool {
    options.reports_back()
        || error.is_some()
        || changes_requested
        || report.status.as_deref() != Some("done")
        || report.findings.as_deref().is_some_and(|f| !f.trim().is_empty())
        || !report.followups.is_empty()
        || !report.review_comments.is_empty()
}

#[allow(clippy::too_many_arguments)]
pub async fn wrapup(
    store: &WorldStore,
    agent: &WorldAgent,
    session: &AgentSession,
    task: Option<&AgentTask>,
    report: &prompts::Report,
    error: Option<&str>,
    changes_requested: bool,
    extra_publish_secs: Option<u64>,
) -> Wrapup {
    let options = match session.task_id {
        Some(id) => store.quest_options(id).await.unwrap_or_default(),
        None => QuestOptions::default(),
    };
    if let (Some(owner), Some(repo)) = (agent.owner_user_id, task.and_then(|t| t.repo.as_deref())) {
        if let Err(e) = store.record_repo_notes(owner, agent.id, repo, session.task_id, &report.repo_notes).await {
            tracing::warn!(agent = %agent.slug, error = %e, "Could not save repo notes");
        }
    }
    let mut timing = store.quest_timing(session.id).await.unwrap_or(Value::Null);
    if let (Some(secs), Some(map)) = (extra_publish_secs, timing.as_object_mut()) {
        let current = map.get("publish_secs").and_then(Value::as_f64).unwrap_or(0.0);
        map.insert("publish_secs".into(), json!(current + secs as f64));
    }
    Wrapup {
        wake: wakes_main(&options, report, error, changes_requested),
        pace: if options.pace.is_empty() { "normal".into() } else { options.pace },
        timing,
        decisions: match session.task_id {
            Some(id) => store.quest_decisions_note(id).await.ok().flatten(),
            None => None,
        },
    }
}

pub fn attempt_dir(dir: &str, attempt: i16) -> String {
    if attempt <= 1 {
        dir.to_string()
    } else {
        format!("{dir}-{attempt}")
    }
}

pub struct QuestStart<'a> {
    pub agent: &'a WorldAgent,
    pub main: &'a AgentSession,
    pub task: &'a AgentTask,
    pub brief: Option<&'a str>,
    pub review: bool,
    pub reviewer: Option<&'a ReviewerChoice>,
    pub rules: &'a str,
    pub options: &'a QuestOptions,
    pub world_tools: bool,
}

pub fn clarification_for_start<'a>(
    items: &'a [InboxItem],
    task_id: Uuid,
    source: Option<&str>,
    allow_implicit: bool,
) -> std::result::Result<Option<&'a InboxItem>, String> {
    if let Some(item) = items.iter().find(|item| loka_shared::world::clarification_continuation_task(item) == Some(task_id)) {
        return Ok(Some(item));
    }
    let candidates: Vec<&InboxItem> = items
        .iter()
        .filter(|item| loka_shared::world::clarification_continuation_unbound(item))
        .collect();
    if let Some(source) = source.map(str::trim).filter(|source| !source.is_empty()) {
        return candidates
            .into_iter()
            .find(|item| prompts::clarification_source(item).as_deref() == Some(source))
            .map(Some)
            .ok_or_else(|| format!("Clarification answer {source} is not available in this turn"));
    }
    if !allow_implicit || candidates.len() != 1 {
        return Ok(None);
    }
    let actionable = items
        .iter()
        .filter(|item| {
            (loka_shared::world::OWNER_SPEECH_KINDS.contains(&item.kind.as_str())
                && item.payload.get("from_owner").and_then(Value::as_bool) == Some(true)
                && item.payload.get("shell_share").and_then(Value::as_bool) != Some(true))
                || (item.kind == "task.assigned" && item.payload.get("subscribed").and_then(Value::as_bool) != Some(true))
                || (item.kind == "onboarding.finished" && item.payload.get("from_owner").and_then(Value::as_bool) == Some(true))
        })
        .count();
    Ok((actionable == 1).then_some(candidates[0]))
}

pub enum StartBrief<'a> {
    Ordinary(Option<&'a str>),
    Clarification(&'a InboxItem),
}

impl<'a> StartBrief<'a> {
    pub(crate) fn text(&self) -> Option<Cow<'a, str>> {
        match self {
            Self::Ordinary(brief) => brief.map(Cow::Borrowed),
            Self::Clarification(item) => prompts::clarification_continuation_brief(item)
                .map(Cow::Owned)
                .or_else(|| item.payload.get("brief").and_then(Value::as_str).map(|brief| Cow::Owned(brief.to_string()))),
        }
    }

    pub(crate) fn continuation(&self) -> Option<&'a InboxItem> {
        match self {
            Self::Ordinary(_) => None,
            Self::Clarification(item) => Some(*item),
        }
    }
}

fn base_task_prompt(
    task: &AgentTask,
    dir: &str,
    options: &QuestOptions,
    context: [&str; 3],
    handoff: Option<&str>,
    brief: Option<&str>,
) -> String {
    let mut prompt = if options.quick() { prompts::quick_task(task, dir) } else { prompts::task(task, dir, None) };
    for part in context {
        if !part.is_empty() {
            prompt.push_str(&format!("\n\n{part}"));
        }
    }
    if let Some(handoff) = handoff.filter(|value| !value.is_empty()) {
        prompt.push_str(&format!("\n\n{handoff}"));
    }
    if let Some(brief) = brief.filter(|value| !value.is_empty()) {
        prompt.push_str(&format!("\n\nYour brief:\n{brief}"));
    }
    prompt
}

fn best_of_prework_policy(attempts: i16) -> String {
    format!(
        "[best-of prework]\nThis is one read-only decision before {attempts} independent attempts start. Do not edit, create or delete files, commit or install anything in this session. Inspect what you need, then apply the before-big-work policy below once for every attempt. If it allows proceeding without a question or plan, do not implement here. End with a report block whose status is done, proceed_without_plan is true and summary carries the settled facts and assumptions to every attempt."
    )
}

pub fn before_big_work_policy(mode: BeforeBigWork, quick: bool) -> &'static str {
    match (mode, quick) {
        (BeforeBigWork::AskWhenUnclear, true) => "[before big work: ask when unclear]\nThis is a small, clear quest, so start it now. Ask only when a missing fact makes progress impossible.",
        (BeforeBigWork::AskWhenUnclear, false) => "[before big work: ask when unclear]\nBefore editing on non-trivial work, use read-only inspection to decide whether missing facts could change the result. If so, ask 1 to 3 specific questions, with short choices when practical, and end with this block instead of a report:\n<!-- clarify\n{\"text\":\"The numbered questions\",\"choices\":[\"A useful choice\"]}\nclarify -->\nSmall clear work starts immediately. Once answered, do not ask the settled questions again. Clarifying missing facts up front is different from asking permission between steps.",
        (BeforeBigWork::JustBuild, _) => "[before big work: just build]\nMake reasonable assumptions and keep moving. Ask only when a missing fact makes progress impossible. Then end with <!-- clarify {\"text\":\"The numbered questions\",\"choices\":[\"A useful choice\"]} clarify --> instead of a report.",
        (BeforeBigWork::AlwaysShowPlan, true) => "[before big work: small work starts now]\nThis is a small, clear quest, so start it now without a plan.",
        (BeforeBigWork::AlwaysShowPlan, false) => AUTOMATIC_PLAN,
    }
}

pub async fn append_single_prework_policy(store: &WorldStore, session_id: Uuid, run_id: Uuid, prompt: &mut String) -> Result<()> {
    let Some((mode, automatic, quick, planning)) = store.prepare_single_prework_run(session_id, run_id).await? else {
        return Ok(());
    };
    let policy = if planning {
        PLAN_FIRST
    } else if automatic {
        AUTOMATIC_PLAN
    } else {
        before_big_work_policy(mode, quick || mode == BeforeBigWork::AlwaysShowPlan)
    };
    if !prompt.trim_end().ends_with(policy) {
        prompt.push_str("\n\n");
        prompt.push_str(policy);
    }
    Ok(())
}

fn automatic_plan_policy(prompt: &str) -> bool {
    prompt
        .lines()
        .rev()
        .find(|line| line.starts_with("[before big work:"))
        .is_some_and(|line| line == AUTOMATIC_PLAN.lines().next().unwrap_or_default())
}

#[derive(Debug, PartialEq, Eq)]
enum AutomaticPrework {
    Plan,
    Proceed,
    RequirePlan,
}

fn automatic_prework(output: &str) -> AutomaticPrework {
    if prompts::has_plan(output) {
        AutomaticPrework::Plan
    } else if prompts::parse_report(output).is_some_and(|report| report.status.as_deref() == Some("done") && report.proceed_without_plan) {
        AutomaticPrework::Proceed
    } else {
        AutomaticPrework::RequirePlan
    }
}

fn report_finishes_prework(planning: bool, automatic: bool, output: &str) -> bool {
    !planning && !automatic && prompts::parse_report(output).is_some_and(|report| report.status.is_some())
}

fn best_of_can_fan_out(plan_state: Option<&str>, automatic: bool, output: &str) -> bool {
    matches!(plan_state, None | Some("approved"))
        && if automatic {
            automatic_prework(output) == AutomaticPrework::Proceed
        } else {
            prompts::parse_report(output)
                .is_some_and(|report| report.status.as_deref() == Some("done") && report.proceed_without_plan)
        }
}

fn best_of_automatic_policy(session: &AgentSession) -> bool {
    let Some(asked_by) = session.asked_by.as_ref() else {
        return false;
    };
    asked_by.get("before_big_work").and_then(Value::as_str) == Some(BeforeBigWork::AlwaysShowPlan.as_str())
        && asked_by.get("best_of_plan_approved").and_then(Value::as_bool) != Some(true)
}

fn prework_automatic_policy(session: &AgentSession, best_of: bool, prompt: &str) -> bool {
    if best_of {
        best_of_automatic_policy(session)
    } else {
        loka_shared::quests::single_prework_automatic(session.asked_by.as_ref()).unwrap_or_else(|| automatic_plan_policy(prompt))
    }
}

pub async fn open_sessions(
    store: &WorldStore,
    start: QuestStart<'_>,
    dirs: &[String],
    continuation: Option<&InboxItem>,
) -> Result<Vec<(AgentSession, SessionRun)>> {
    let QuestStart { agent, main, task, brief, review, reviewer, rules, options, world_tools } = start;
    let racing = options.attempts > 1;
    let best_of_prework = racing;
    if dirs.len() != 1 {
        return Err(anyhow!("quest workspaces do not match its attempts"));
    }
    let milestones = MilestoneStore::new(store.pool().clone()).for_task(task.id).await?;
    let block = milestones::prompt_block(&milestones);
    let brief = brief.filter(|b| !b.trim().is_empty());
    let notes = match (agent.owner_user_id, task.repo.as_deref()) {
        (Some(owner), Some(repo)) => store
            .repo_notes(owner, agent.org_id, repo)
            .await
            .ok()
            .flatten()
            .map(|r| loka_shared::reports::repo_notes_prompt(&r.body, REPO_NOTES_PROMPT_CAP))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let handoff = store.quest_handoff(task.id).await.ok().flatten();
    let room = store.room_asking(task.id).await?;
    let asking = match room.as_ref().filter(|room| world_tools && room.mode().asks()) {
        Some(room) => {
            let lead = match room.lead.filter(|lead| *lead != agent.id) {
                Some(lead) => store.get_agent(lead).await?.map(|a| a.name),
                None => None,
            };
            Some(prompts::asking_block(lead.as_deref().unwrap_or(&agent.name), store.decisions_note(room.room).await?.as_deref()))
        }
        None => None,
    };
    let before_big_work = store.agent_before_big_work(agent).await?;
    let launch_eligible = !options.quick()
        && !racing
        && prompts::feature_launch_active(loka_shared::features::LaunchScope::Quest);
    let recent = if launch_eligible { prompts::recent_feature_exposures(store, agent).await } else { Vec::new() };
    let mut opened = Vec::new();
    let mut choice = None;
    for dir in dirs {
        let backend = main.backend.clone();
        let model = main.model.clone();
        let too_large = match (backend == main.backend, choice) {
            (false, _) => None,
            (true, Some(known)) => Some(known),
            (true, None) => {
                let level = QuestContext::resolve(options.context.as_deref(), &agent.quest_context);
                let known = store.fork_choice(agent, main, level).await?;
                choice = Some(known);
                Some(known)
            }
        };
        let fork = matches!(too_large, Some(ForkChoice::Fork));
        let session = store
            .create_session(NewSession {
                agent_id: agent.id,
                role: "task",
                backend: &backend,
                model: &model,
                parent_session_id: Some(main.id),
                task_id: Some(task.id),
                working_dir: Some(dir),
                usage_baseline: if fork { main.usage_baseline.clone() } else { json!({}) },
            })
            .await?;
        let session = store.set_session_review(session.id, review && !racing && !options.quick(), reviewer).await?;
        let mut prompt = base_task_prompt(task, dir, options, [rules, &block, &notes], handoff.as_deref(), brief);
        if !options.quick() && !racing {
            prompt.push_str(&format!("\n\n{}", prompts::quest_feature_map("your person")));
        }
        if launch_eligible {
            let launch = prompts::quest_feature_launch(task, &recent, "your person");
            if !launch.is_empty() {
                prompt.push_str(&format!("\n\n{launch}"));
            }
        }
        if let Some(asking) = &asking {
            prompt.push_str(&format!("\n\n{asking}"));
        }
        if best_of_prework {
            let policy = before_big_work_policy(before_big_work, options.quick());
            prompt.push_str(&format!("\n\n{policy}\n\n{}", best_of_prework_policy(options.attempts)));
            store
                .set_session_asked_by(
                    session.id,
                    json!({
                        "best_of_prework": "pending",
                        "attempts": options.attempts,
                        "before_big_work": before_big_work.as_str(),
                        "options": options,
                        "brief": brief,
                        "run_request": loka_shared::run_request::RunRequest::task(
                            task.description.as_deref(),
                            if continuation.is_some() { None } else { brief },
                        ).value(),
                        "quest_handoff": handoff.clone(),
                    }),
                )
                .await?;
        } else {
            let policy = before_big_work_policy(before_big_work, options.quick());
            prompt.push_str(&format!("\n\n{policy}"));
            store
                .set_session_asked_by(
                    session.id,
                    json!({
                        "before_big_work": before_big_work.as_str(),
                        "before_big_work_phase": "pending",
                        "before_big_work_automatic": automatic_plan_policy(policy),
                    }),
                )
                .await?;
            if options.plan_first {
                store.begin_plan(session.id).await?;
                prompt.push_str(&format!("\n\n{PLAN_FIRST}"));
            }
        }
        if !fork {
            prompt = format!("{}\n\n{}", store.fresh_start_memory(agent, &fresh_intro(agent, &backend, too_large)).await?, prompts::unforked(&prompt));
        }
        let new_run = NewRun {
            session_id: session.id,
            agent_id: agent.id,
            task_id: Some(task.id),
            kind: "task",
            status: "queued",
            fork_from: fork.then_some(main.id),
            prompt: &prompt,
            timeout_secs: loka_shared::world::default_task_timeout_secs(),
            request: Some(loka_shared::run_request::RunRequest::task(
                task.description.as_deref(),
                if continuation.is_some() { None } else { brief },
            )),
        };
        let run = match continuation {
            Some(item) => match store.create_clarification_run(item, new_run).await? {
                Some(run) => run,
                None => continue,
            },
            None => store.create_run(new_run).await?,
        };
        opened.push((session, run));
    }
    Ok(opened)
}

pub fn best_of_options(session: &AgentSession, fallback: &QuestOptions) -> QuestOptions {
    session
        .asked_by
        .as_ref()
        .and_then(|value| value.get("options"))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| fallback.clone())
}

pub async fn fan_out_sessions(
    store: &WorldStore,
    start: QuestStart<'_>,
    dirs: &[String],
    prework_session: &AgentSession,
    prework_run: &SessionRun,
    output: &str,
) -> Result<Vec<(AgentSession, SessionRun)>> {
    let QuestStart { agent, main, task, brief, rules, options, .. } = start;
    if options.attempts < 2 || dirs.len() != options.attempts as usize {
        return Ok(Vec::new());
    }
    let block = milestones::prompt_block(&MilestoneStore::new(store.pool().clone()).for_task(task.id).await?);
    let brief = brief.filter(|value| !value.trim().is_empty());
    let notes = match (agent.owner_user_id, task.repo.as_deref()) {
        (Some(owner), Some(repo)) => store
            .repo_notes(owner, agent.org_id, repo)
            .await
            .ok()
            .flatten()
            .map(|report| loka_shared::reports::repo_notes_prompt(&report.body, REPO_NOTES_PROMPT_CAP))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let quest_handoff = match prework_session.asked_by.as_ref().and_then(|value| value.get("quest_handoff")) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Null) => None,
        _ => store.quest_handoff(task.id).await.ok().flatten(),
    };
    let prework_handoff = store.best_of_handoff(prework_session.id, output).await?;
    let request = prework_session
        .asked_by
        .as_ref()
        .and_then(|value| value.get("run_request"))
        .map(|value| loka_shared::run_request::RunRequest::from_value(Some(value)));
    let mut choice = None;
    let mut attempts = Vec::with_capacity(dirs.len());
    for (index, dir) in dirs.iter().enumerate() {
        let attempt = index as i16 + 1;
        let variant = options.variant(attempt);
        let backend = variant.as_ref().and_then(|value| value.cli.clone()).unwrap_or_else(|| main.backend.clone());
        let model = variant.as_ref().map(|value| value.model.clone()).unwrap_or_else(|| main.model.clone());
        let too_large = match (backend == main.backend, choice) {
            (false, _) => None,
            (true, Some(known)) => Some(known),
            (true, None) => {
                let level = QuestContext::resolve(options.context.as_deref(), &agent.quest_context);
                let known = store.fork_choice(agent, main, level).await?;
                choice = Some(known);
                Some(known)
            }
        };
        let fork = matches!(too_large, Some(ForkChoice::Fork));
        let mut prompt = base_task_prompt(task, dir, options, [rules, &block, &notes], quest_handoff.as_deref(), brief);
        prompt.push_str(&format!(
            "\n\nYou are attempt {attempt} of {} at this quest. The other attempts work on the same task at the same time in their own workspaces, and an independent judge picks the one that ships. Work only in your workspace and do your best complete version.\n\n[best-of prework handoff]\nThe preference check already ran once for every attempt. Do not repeat it or ask the settled questions again. Use this shared context:\n{prework_handoff}",
            options.attempts
        ));
        if !fork {
            prompt = format!(
                "{}\n\n{}",
                store.fresh_start_memory(agent, &fresh_intro(agent, &backend, too_large)).await?,
                prompts::unforked(&prompt)
            );
        }
        attempts.push(BestOfAttempt {
            backend,
            model,
            working_dir: dir.clone(),
            usage_baseline: if fork { main.usage_baseline.clone() } else { json!({}) },
            fork_from: fork.then_some(main.id),
            prompt,
            request: request.clone(),
        });
    }
    store.fan_out_best_of(agent.id, task.id, prework_session.id, prework_run.id, &attempts).await
}

pub fn fresh_intro(agent: &WorldAgent, backend: &str, choice: Option<ForkChoice>) -> String {
    let used = match choice {
        Some(ForkChoice::Fresh { used, .. }) => Some(used),
        Some(ForkChoice::Lean { .. }) => {
            return loka_shared::context::lean_intro(&agent.name, backend, agent.persona.as_deref(), agent.system_prompt_addendum.as_deref());
        }
        _ => None,
    };
    loka_shared::context::fresh_intro(&agent.name, backend, used)
}

pub async fn label_attempts(store: &WorldStore, task: &AgentTask, opened: &[(AgentSession, SessionRun)]) -> Result<()> {
    for (session, run) in opened {
        let label = match session.attempt {
            Some(attempt) => format!("Working on \"{}\" (attempt {attempt})", task.title),
            None => format!("Working on \"{}\"", task.title),
        };
        store.set_run_activity(run, &label).await?;
    }
    Ok(())
}

pub fn started_payload(task: &AgentTask, options: &QuestOptions, attempts: usize, run_id: Uuid) -> Value {
    let mut payload = json!({ "task_id": task.id, "title": task.title, "repo": task.repo, "run_id": run_id, "queued": true });
    if attempts > 1 {
        payload["attempts"] = json!(attempts);
    }
    if options.plan_first && attempts == 1 {
        payload["plan_first"] = json!(true);
    }
    if !options.reports_back() {
        payload["pace"] = json!(options.pace);
    }
    payload
}

pub async fn plan_step(store: &WorldStore, agent: &WorldAgent, session: &AgentSession, run: &SessionRun, output: &str) -> Result<()> {
    if !store.plan_ready(session.id, run.id).await? {
        return Ok(());
    }
    let task_id = session.task_id.ok_or_else(|| anyhow!("plan session has no quest"))?;
    let title = sqlx::query_scalar::<_, String>("SELECT title FROM agent_tasks WHERE id = $1")
        .bind(task_id)
        .fetch_optional(store.pool())
        .await?
        .unwrap_or_else(|| "a quest".into());
    store
        .post_plan(agent.id, &agent.name, task_id, &title, session.id, run.id, &prompts::plan_text(output))
        .await
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreworkStep {
    Continue,
    Handled,
    FanOut,
}

async fn missing_single_prework(
    store: &WorldStore,
    session: &AgentSession,
    run: &SessionRun,
    policy: &str,
) -> Result<PreworkStep> {
    match store.nudge_single_prework(run.id, session.id, policy).await? {
        loka_shared::world::Nudge::Queued(_) => Ok(PreworkStep::Handled),
        loka_shared::world::Nudge::Open => Ok(PreworkStep::Handled),
        loka_shared::world::Nudge::Exhausted => {
            if let Some(task_id) = session.task_id {
                store
                    .fail_single_prework(
                        session.agent_id,
                        task_id,
                        session.id,
                        run.id,
                        run.output.as_deref().unwrap_or_default(),
                    )
                    .await?;
            }
            Ok(PreworkStep::Handled)
        }
    }
}

pub async fn prework_step(store: &WorldStore, agent: &WorldAgent, session: &AgentSession, run: &SessionRun, output: &str) -> Result<PreworkStep> {
    if session.role != "task" || session.racing() || !matches!(run.kind.as_str(), "task" | "followup") {
        return Ok(PreworkStep::Continue);
    }
    let session = store.get_session(session.id).await?.ok_or_else(|| anyhow!("task session disappeared"))?;
    let best_of = loka_shared::quests::is_best_of_prework(session.asked_by.as_ref());
    let single_prework = loka_shared::quests::single_prework_phase(session.asked_by.as_ref()).is_some();
    if single_prework && !store.owns_single_prework(session.id, run.id).await? {
        return Ok(PreworkStep::Handled);
    }
    let posted = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM world_events WHERE run_id = $1
           AND (type = 'task.plan_ready' OR type = 'attention.requested' AND payload->>'clarification' = 'true'))",
    )
    .bind(run.id)
    .fetch_one(store.pool())
    .await?;
    if posted {
        return Ok(PreworkStep::Handled);
    }
    let answer_followup = run.kind == "followup" && run.prompt.starts_with("[questions answered by ");
    let retry_prework = run.kind == "followup" && loka_shared::quests::single_prework_retry(session.asked_by.as_ref(), run.id);
    if run.kind == "followup" && !answer_followup && !session.planning() && !best_of && !retry_prework && !single_prework {
        return Ok(PreworkStep::Continue);
    }
    let automatic = prework_automatic_policy(&session, best_of, &run.prompt);
    if !best_of && report_finishes_prework(session.planning(), automatic, output) {
        if single_prework && !store.finish_single_prework(session.id, run.id).await? {
            return Ok(PreworkStep::Handled);
        }
        return Ok(PreworkStep::Continue);
    }
    if best_of && best_of_can_fan_out(session.plan_state.as_deref(), automatic, output) {
        return Ok(PreworkStep::FanOut);
    }
    if let Some(request) = prompts::parse_clarification(output) {
        if !matches!(session.plan_state.as_deref(), Some("awaiting" | "approved" | "clarifying"))
            && store
                .post_clarification(
                    agent,
                    &session,
                    run,
                    loka_shared::quests::QuestClarification {
                        text: &request.text,
                        choices: &request.choices,
                        plan_after: session.planning(),
                        always_plan: automatic,
                    },
                )
                .await?
        {
            return Ok(PreworkStep::Handled);
        }
    }
    let automatic_prework = automatic.then(|| automatic_prework(output));
    if automatic_prework == Some(AutomaticPrework::Plan) {
        let task_id = session.task_id.ok_or_else(|| anyhow!("plan session has no quest"))?;
        if !session.planning() && !store.begin_automatic_plan(agent.id, task_id, session.id, run.id, answer_followup || retry_prework).await? {
            return Ok(PreworkStep::Handled);
        }
        plan_step(store, agent, &session, run, output).await?;
        return Ok(PreworkStep::Handled);
    }
    if !session.planning() && automatic {
        if automatic_prework == Some(AutomaticPrework::Proceed) {
            return Ok(if best_of {
                PreworkStep::Handled
            } else {
                if single_prework && !store.finish_single_prework(session.id, run.id).await? {
                    return Ok(PreworkStep::Handled);
                }
                PreworkStep::Continue
            });
        }
        let task_id = session.task_id.ok_or_else(|| anyhow!("plan session has no quest"))?;
        store.require_automatic_plan(agent.id, task_id, session.id, run.id, answer_followup || retry_prework).await?;
        return Ok(PreworkStep::Handled);
    }
    if session.planning() {
        if best_of && !prompts::has_plan(output) {
            let task_id = session.task_id.ok_or_else(|| anyhow!("prework session has no quest"))?;
            store.require_best_of_prework(agent.id, task_id, session.id, run.id).await?;
            return Ok(PreworkStep::Handled);
        }
        if single_prework && !prompts::has_plan(output) {
            return missing_single_prework(store, &session, run, PLAN_FIRST).await;
        }
        plan_step(store, agent, &session, run, output).await?;
        return Ok(PreworkStep::Handled);
    }
    if best_of {
        let task_id = session.task_id.ok_or_else(|| anyhow!("prework session has no quest"))?;
        store.require_best_of_prework(agent.id, task_id, session.id, run.id).await?;
        return Ok(PreworkStep::Handled);
    }
    if single_prework {
        let mode = session
            .asked_by
            .as_ref()
            .and_then(|value| value.get("before_big_work"))
            .and_then(Value::as_str)
            .and_then(BeforeBigWork::parse)
            .unwrap_or_default();
        let quick = match session.task_id {
            Some(task_id) => store.quest_options(task_id).await?.quick(),
            None => false,
        };
        return missing_single_prework(store, &session, run, before_big_work_policy(mode, quick)).await;
    }
    Ok(PreworkStep::Continue)
}

pub async fn candidates(store: &WorldStore, ids: &[Uuid]) -> Result<Vec<AgentSession>> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(session) = store.get_session(*id).await? {
            out.push(session);
        }
    }
    out.sort_by_key(|s| s.attempt.unwrap_or(0));
    Ok(out)
}

fn relative_dir(from: &str, to: &str) -> String {
    let (from, to) = (Path::new(from), Path::new(to));
    if from == to {
        return ".".into();
    }
    match (from.parent(), to.parent(), to.file_name()) {
        (Some(a), Some(b), Some(name)) if a == b => format!("../{}", name.to_string_lossy()),
        _ => to.to_string_lossy().to_string(),
    }
}

pub async fn queue_compare(
    store: &WorldStore,
    agent: &WorldAgent,
    task: &AgentTask,
    base_branch: &str,
    rules: &str,
    candidates: &[AgentSession],
) -> Result<SessionRun> {
    let first = candidates.first().ok_or_else(|| anyhow!("no attempts to compare"))?;
    let home = first.working_dir.clone().unwrap_or_default();
    let (backend, model) = store.reviewer_for(agent, first).await?;
    let review = store
        .create_session(NewSession {
            agent_id: agent.id,
            role: "review",
            backend: &backend,
            model: &model,
            parent_session_id: Some(first.id),
            task_id: Some(task.id),
            working_dir: first.working_dir.as_deref(),
            usage_baseline: json!({}),
        })
        .await?;
    store
        .set_session_asked_by(review.id, json!({ "compare": candidates.iter().map(|c| c.id).collect::<Vec<_>>() }))
        .await?;
    let briefs: Vec<prompts::AttemptBrief> = candidates
        .iter()
        .map(|c| prompts::AttemptBrief {
            attempt: c.attempt.unwrap_or(1),
            dir: relative_dir(&home, c.working_dir.as_deref().unwrap_or_default()),
            setup: format!("{} {}", c.backend, c.model),
            report: prompts::stored_report(c.summary.as_deref()),
        })
        .collect();
    let mut prompt = prompts::compare(task, base_branch, &briefs);
    if !rules.is_empty() {
        prompt.push_str(&format!("\n\nThe authors were bound by these rules. A violation counts against an attempt:\n{rules}"));
    }
    let run = store
        .create_run(NewRun {
            session_id: review.id,
            agent_id: agent.id,
            task_id: Some(task.id),
            kind: "review",
            status: "queued",
            fork_from: None,
            prompt: &prompt,
            timeout_secs: loka_shared::world::default_task_timeout_secs(),
            request: None,
        })
        .await?;
    store
        .set_run_activity(&run, &format!("Judging {} attempts at \"{}\"", candidates.len(), task.title))
        .await?;
    Ok(run)
}

pub fn pick(output: Option<&str>, candidates: &[AgentSession]) -> (Uuid, Value) {
    let numbers: Vec<i16> = candidates.iter().filter_map(|c| c.attempt).collect();
    let picked = output.and_then(|o| prompts::parse_pick(o, &numbers));
    let winner = picked
        .and_then(|n| candidates.iter().find(|c| c.attempt == Some(n)))
        .or(candidates.first())
        .map(|c| c.id)
        .unwrap_or_default();
    let raw = output.and_then(|o| {
        let (_, block) = o.rsplit_once("<!-- verdict")?;
        let (body, _) = block.split_once("verdict -->")?;
        serde_json::from_str::<Value>(body.trim()).ok()
    });
    let verdict = match (picked, raw) {
        (Some(_), Some(mut raw)) => {
            raw["verdict"] = json!("pick");
            raw
        }
        _ => json!({
            "verdict": "pick",
            "winner": candidates.first().and_then(|c| c.attempt),
            "summary": "The judge gave no readable pick, so the first finished attempt ships.",
            "note": "unparsed",
        }),
    };
    (winner, verdict)
}

pub async fn settle(store: &WorldStore, agent: &WorldAgent, task_id: Uuid, winner: Uuid, why: &str) -> Result<AgentSession> {
    let lost = store.settle_attempts(task_id, winner).await?;
    let session = store.get_session(winner).await?.ok_or_else(|| anyhow!("winning attempt disappeared"))?;
    store.announce_attempts(agent.id, task_id, &session, &lost, why).await?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(id: i64, kind: &str, payload: Value) -> InboxItem {
        InboxItem {
            id,
            agent_id: Uuid::new_v4(),
            kind: kind.into(),
            event_seq: None,
            payload,
            status: "processing".into(),
            attempts: 1,
            created_at: chrono::Utc::now(),
        }
    }

    fn regular_answer(id: i64) -> InboxItem {
        inbox(
            id,
            "message",
            json!({
                "from_owner": true,
                "message_id": Uuid::new_v4(),
                "body": "Postgres",
                "clarification_continuation": Uuid::new_v4(),
                "clarification_handoff": {
                    "request": "Build the store",
                    "question": "Which database?",
                    "answer": "Postgres"
                }
            }),
        )
    }

    #[test]
    fn clarification_start_prefers_bound_work_and_requires_unambiguous_chat() {
        let task = Uuid::new_v4();
        let other = Uuid::new_v4();
        let regular = regular_answer(1);
        let bound = inbox(
            2,
            "task.assigned",
            json!({
                "task_id": task,
                "resume": true,
                "clarification_continuation": Uuid::new_v4(),
                "clarification_status": "todo",
                "clarification_resume": false
            }),
        );
        assert_eq!(
            clarification_for_start(&[regular.clone(), bound.clone()], task, None, true)
                .unwrap()
                .map(|item| item.id),
            Some(2)
        );
        assert_eq!(
            clarification_for_start(std::slice::from_ref(&regular), task, None, true)
                .unwrap()
                .map(|item| item.id),
            Some(1)
        );
        assert!(clarification_for_start(std::slice::from_ref(&regular), task, None, false).unwrap().is_none());

        let ordinary = inbox(3, "message", json!({ "from_owner": true, "body": "Also update the docs" }));
        assert!(clarification_for_start(&[regular.clone(), ordinary], task, None, true).unwrap().is_none());
        let assignment = inbox(4, "task.assigned", json!({ "task_id": other }));
        assert!(clarification_for_start(&[regular.clone(), assignment], task, None, true).unwrap().is_none());
        assert!(clarification_for_start(&[regular.clone(), regular_answer(5)], task, None, true).unwrap().is_none());

        let source = prompts::clarification_source(&regular).unwrap();
        assert_eq!(
            clarification_for_start(&[regular.clone(), regular_answer(6)], task, Some(&source), false)
                .unwrap()
                .map(|item| item.id),
            Some(1)
        );
        assert!(clarification_for_start(std::slice::from_ref(&regular), task, Some("msg:missing"), true).is_err());
        assert_eq!(
            clarification_for_start(&[regular, bound], task, Some("msg:missing"), true)
                .unwrap()
                .map(|item| item.id),
            Some(2)
        );
    }

    #[test]
    fn before_big_work_modes_keep_small_work_moving() {
        let ask = before_big_work_policy(BeforeBigWork::AskWhenUnclear, false);
        assert!(ask.contains("1 to 3 specific questions") && ask.contains("<!-- clarify"));
        assert!(before_big_work_policy(BeforeBigWork::JustBuild, false).contains("reasonable assumptions"));
        let always = before_big_work_policy(BeforeBigWork::AlwaysShowPlan, false);
        assert!(always.contains("<!-- plan") && always.contains("proceed_without_plan"));
        assert!(always.contains("Clear substantial work starts with a plan"));
        assert!(always.contains("build, fix or implement") && always.contains("does not by itself skip planning"));
        assert!(before_big_work_policy(BeforeBigWork::AskWhenUnclear, true).contains("start it now"));
        assert!(before_big_work_policy(BeforeBigWork::AlwaysShowPlan, true).contains("without a plan"));
        assert!(automatic_plan_policy(&format!("user said {}\n\n{always}", AUTOMATIC_PLAN.lines().next().unwrap())));
        assert!(!automatic_plan_policy(&format!(
            "user said {}\n\n{}",
            AUTOMATIC_PLAN.lines().next().unwrap(),
            before_big_work_policy(BeforeBigWork::AskWhenUnclear, false)
        )));
        assert!(!automatic_plan_policy(before_big_work_policy(BeforeBigWork::AlwaysShowPlan, true)));
        assert!(prompts::has_plan(always));
        assert!(!prompts::parse_report("<!-- report {\"status\":\"done\"} report -->").unwrap().proceed_without_plan);
        assert!(prompts::parse_report("<!-- report {\"status\":\"done\",\"proceed_without_plan\":true} report -->")
            .unwrap()
            .proceed_without_plan);
        assert_eq!(
            automatic_prework("<!-- plan {\"summary\":\"Inspect it\",\"steps\":[],\"checks\":[],\"risks\":[]} plan -->"),
            AutomaticPrework::Plan
        );
        assert_eq!(
            automatic_prework("<!-- report {\"status\":\"done\",\"proceed_without_plan\":true} report -->"),
            AutomaticPrework::Proceed
        );
        for output in [
            "",
            "I finished it",
            "<!-- plan {} plan -->",
            "<!-- report {\"status\":\"done\"} report -->",
            "<!-- report {\"status\":\"blocked\",\"proceed_without_plan\":true} report -->",
        ] {
            assert_eq!(automatic_prework(output), AutomaticPrework::RequirePlan, "{output}");
        }
        let proceed = "<!-- report {\"status\":\"done\",\"proceed_without_plan\":true} report -->";
        assert!(best_of_can_fan_out(None, false, proceed));
        assert!(best_of_can_fan_out(Some("approved"), false, proceed));
        assert!(!best_of_can_fan_out(Some("planning"), false, proceed));
        assert!(!best_of_can_fan_out(Some("awaiting"), true, proceed));
        assert!(!best_of_can_fan_out(Some("clarifying"), false, proceed));
        let completed_with_example = "<!-- clarify {\"text\":\"Example\"} clarify -->\n<!-- report {\"status\":\"done\"} report -->";
        assert!(report_finishes_prework(false, false, completed_with_example));
        assert!(!report_finishes_prework(false, true, completed_with_example));
    }

    #[tokio::test]
    async fn single_prework_survives_retry_and_ends_before_later_followups() -> Result<()> {
        use loka_shared::run_causes::{Cause, Diagnosis};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = loka_shared::db::Database::connect(&url).await?;
        let store = WorldStore::new(db.pool().clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("prework-{tag}@example.test"))
            .fetch_one(store.pool())
            .await?;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Planner', 'claude', 'sonnet', $2) RETURNING id",
        )
        .bind(format!("prework-{tag}"))
        .bind(owner)
        .fetch_one(store.pool())
        .await?;
        let main = store
            .create_session(NewSession {
                agent_id,
                role: "main",
                backend: "claude",
                model: "sonnet",
                parent_session_id: None,
                task_id: None,
                working_dir: None,
                usage_baseline: json!({}),
            })
            .await?;
        let agent = store.get_agent(agent_id).await?.expect("agent");
        let diagnosis = Diagnosis {
            cause: Cause::CliError,
            failure: Some("network".into()),
            stage: "run".into(),
            reason: "connection lost".into(),
            turns: 1,
            duration_secs: 1,
            tail: String::new(),
            auto_retried: false,
            resets_at: None,
        };
        for (name, automatic) in [("ask", false), ("always", true)] {
            let task: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, $2, 'in_progress', $3) RETURNING id",
            )
            .bind(owner)
            .bind(format!("{name} after retry"))
            .bind(agent_id)
            .fetch_one(store.pool())
            .await?;
            let session = store
                .create_session(NewSession {
                    agent_id,
                    role: "task",
                    backend: "claude",
                    model: "sonnet",
                    parent_session_id: Some(main.id),
                    task_id: Some(task),
                    working_dir: Some("tasks/prework/repo"),
                    usage_baseline: json!({}),
                })
                .await?;
            sqlx::query("UPDATE agent_sessions SET cli_session_id = $2 WHERE id = $1")
                .bind(session.id)
                .bind(format!("cli-{name}-{tag}"))
                .execute(store.pool())
                .await?;
            store
                .set_session_asked_by(
                    session.id,
                    json!({
                        "before_big_work": if automatic { "always_show_plan" } else { "ask_when_unclear" },
                        "before_big_work_phase": "pending",
                        "before_big_work_automatic": automatic,
                    }),
                )
                .await?;
            let session = store.get_session(session.id).await?.expect("session");
            let plan = "<!-- plan {\"summary\":\"Use Postgres\",\"steps\":[],\"checks\":[],\"risks\":[]} plan -->";

            let failed = if automatic {
                let initial = store
                    .create_run(NewRun {
                        session_id: session.id,
                        agent_id,
                        task_id: Some(task),
                        kind: "task",
                        status: "completed",
                        fork_from: None,
                        prompt: "initial task policy",
                        timeout_secs: 60,
                        request: None,
                    })
                    .await?;
                let clarification = "<!-- clarify {\"text\":\"Which database?\",\"choices\":[\"Postgres\",\"SQLite\"]} clarify -->";
                assert_eq!(prework_step(&store, &agent, &session, &initial, clarification).await?, PreworkStep::Handled);
                let card: Uuid = sqlx::query_scalar("SELECT id FROM agent_messages WHERE run_id = $1 AND meta ? 'clarification'")
                    .bind(initial.id)
                    .fetch_one(store.pool())
                    .await?;
                let (answer, _) = store
                    .answer_clarification(
                        agent_id,
                        card,
                        loka_shared::quests::ClarificationReply {
                            user_id: owner,
                            author: "Ana",
                            text: "Postgres",
                            meta: json!({ "answers": card, "task_id": task }),
                            reply_to: None,
                            event_payload: json!({ "author": "Ana", "author_kind": "user", "body": "Postgres", "answer_card": card }),
                        },
                    )
                    .await?;
                let loka_shared::quests::ClarificationAnswer::Answered { run_id, .. } = answer else {
                    panic!("clarification was not answered")
                };
                let answered = store.get_session(session.id).await?.expect("answered session");
                assert_eq!(answered.plan_state.as_deref(), Some("approved"));
                assert_eq!(loka_shared::quests::single_prework_phase(answered.asked_by.as_ref()), Some("pending"));
                sqlx::query("UPDATE session_runs SET status = 'failed' WHERE id = $1")
                    .bind(run_id)
                    .execute(store.pool())
                    .await?;
                store.get_run(run_id).await?.expect("answered run")
            } else {
                store
                    .create_run(NewRun {
                        session_id: session.id,
                        agent_id,
                        task_id: Some(task),
                        kind: "task",
                        status: "failed",
                        fork_from: None,
                        prompt: "initial task policy",
                        timeout_secs: 60,
                        request: None,
                    })
                    .await?
            };
            let retry_id = store.auto_retry(failed.id, &diagnosis).await?.expect("retry");
            let retry = store.get_run(retry_id).await?.expect("retry run");
            sqlx::query("UPDATE session_runs SET status = 'completed' WHERE id = $1")
                .bind(retry.id)
                .execute(store.pool())
                .await?;
            let retry = store.get_run(retry.id).await?.expect("completed retry run");
            assert_eq!(retry.kind, "followup");
            assert_eq!(retry.prompt, loka_shared::run_causes::RESUME_AFTER_ERROR);
            let session = store.get_session(session.id).await?.expect("session");
            assert_eq!(loka_shared::quests::single_prework_automatic(session.asked_by.as_ref()), Some(automatic));
            assert!(loka_shared::quests::single_prework_retry(session.asked_by.as_ref(), retry.id));

            if automatic {
                assert_eq!(prework_step(&store, &agent, &session, &retry, plan).await?, PreworkStep::Handled);
                let awaiting = store.get_session(session.id).await?.expect("awaiting plan");
                assert_eq!(awaiting.plan_state.as_deref(), Some("awaiting"));
                assert_eq!(loka_shared::quests::single_prework_phase(awaiting.asked_by.as_ref()), Some("planning"));
                let approval = store.approve_plan(task, "Ana", "chat", "Approved").await?;
                let loka_shared::quests::PlanApproval::Approved { run_id, .. } = approval else {
                    panic!("plan was not approved")
                };
                let implementation = store.get_run(run_id).await?.expect("implementation");
                let approved = store.get_session(session.id).await?.expect("approved session");
                assert_eq!(loka_shared::quests::single_prework_phase(approved.asked_by.as_ref()), None);
                assert_eq!(prework_step(&store, &agent, &approved, &implementation, plan).await?, PreworkStep::Continue);
                assert_eq!(store.get_session(session.id).await?.unwrap().plan_state.as_deref(), Some("approved"));
            } else {
                let clarification = "<!-- clarify {\"text\":\"Which database?\",\"choices\":[\"Postgres\",\"SQLite\"]} clarify -->";
                assert_eq!(prework_step(&store, &agent, &session, &retry, clarification).await?, PreworkStep::Handled);
                let clarifying = store.get_session(session.id).await?.expect("clarifying session");
                assert_eq!(clarifying.plan_state.as_deref(), Some("clarifying"));
                assert_eq!(loka_shared::quests::single_prework_phase(clarifying.asked_by.as_ref()), Some("pending"));
            }
        }
        Ok(())
    }

    #[test]
    fn best_of_keeps_captured_options_and_the_strongest_policy() {
        let session = |approved: bool| -> AgentSession {
            serde_json::from_value(json!({
                "id": Uuid::new_v4(), "agent_id": Uuid::new_v4(), "role": "task", "backend": "claude", "model": "opus",
                "cli_session_id": null, "parent_session_id": Uuid::new_v4(), "task_id": Uuid::new_v4(), "working_dir": "tasks/q/repo",
                "status": "active", "usage_baseline": {}, "context_tokens": 0, "review_rounds": 0, "summary": null,
                "rules_hash": null,
                "asked_by": {
                    "best_of_prework": "pending",
                    "attempts": 3,
                    "before_big_work": "always_show_plan",
                    "best_of_plan_approved": approved,
                    "quest_handoff": "Earlier finished work",
                    "options": {
                        "plan_first": false,
                        "attempts": 3,
                        "attempt_variants": [
                            { "model": "opus" },
                            { "cli": "codex", "model": "gpt-5" },
                            { "model": "sonnet" }
                        ],
                        "ci_feedback": true,
                        "pace": "urgent",
                        "context": "lean"
                    }
                },
                "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now()
            }))
            .unwrap()
        };
        let pending = session(false);
        let fallback = QuestOptions { attempts: 1, pace: "quick".into(), ..Default::default() };
        let options = best_of_options(&pending, &fallback);
        assert_eq!(options.attempts, 3);
        assert_eq!(options.pace, "urgent");
        assert_eq!(options.variant(2).unwrap().cli.as_deref(), Some("codex"));
        assert_eq!(options.variant(3).unwrap().model, "sonnet");
        assert!(best_of_automatic_policy(&pending));
        assert!(!best_of_automatic_policy(&session(true)));
        assert!(!prework_automatic_policy(
            &session(true),
            true,
            before_big_work_policy(BeforeBigWork::AlwaysShowPlan, false)
        ));
        assert_eq!(pending.asked_by.as_ref().unwrap()["quest_handoff"], json!("Earlier finished work"));
    }

    #[tokio::test]
    async fn start_target_resolves_ids_and_tell_agent_reaches_the_inbox() -> Result<()> {
        use loka_shared::db::Database;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let db = Database::connect(&url).await?;
        let pool = db.pool().clone();
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("start-{tag}@example.test"))
            .fetch_one(&pool)
            .await?;
        let mut agents = Vec::new();
        for name in ["start-a", "start-b"] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Bunny', 'claude', 'sonnet', $2) RETURNING id",
            )
            .bind(format!("{name}-{tag}"))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            agents.push(store.get_agent(id).await?.expect("agent"));
        }
        let (mine, other) = (&agents[0], &agents[1]);
        let prefix = &tag[..8];
        let ids: Vec<Uuid> = (1..=3).map(|n| format!("{prefix}-0000-4000-8000-00000000000{n}").parse().unwrap()).collect();
        let lone: Uuid = format!("{}-0000-4000-8000-000000000009", &Uuid::new_v4().simple().to_string()[..8]).parse()?;
        for (id, agent) in [(ids[0], mine), (ids[1], mine), (ids[2], other), (lone, mine)] {
            sqlx::query("INSERT INTO agent_tasks (id, user_id, title, status, assigned_agent_id) VALUES ($1, $2, 'quest', 'todo', $3)")
                .bind(id)
                .bind(owner)
                .bind(agent.id)
                .execute(&pool)
                .await?;
        }

        assert_eq!(start_target(&store, mine, &ids[0].to_string()).await, Ok(ids[0]));
        assert_eq!(start_target(&store, mine, &lone.to_string()[..8]).await, Ok(lone));
        assert_eq!(start_target(&store, mine, &lone.to_string()[..13]).await, Ok(lone));
        let ambiguous = start_target(&store, mine, prefix).await.unwrap_err();
        assert!(ambiguous.contains("matches 3 tasks"), "{ambiguous}");
        let unknown = start_target(&store, mine, "deadbeef").await.unwrap_err();
        assert!(unknown.contains("no task"), "{unknown}");
        let unknown_full = Uuid::new_v4();
        let unknown_full = start_target(&store, mine, &unknown_full.to_string()).await.unwrap_err();
        assert!(unknown_full.contains("no task"), "{unknown_full}");
        let short = start_target(&store, mine, "50eed2").await.unwrap_err();
        assert!(short.contains("not a task id"), "{short}");
        let elsewhere = start_target(&store, mine, &ids[2].to_string()).await.unwrap_err();
        assert!(elsewhere.contains("assigned to another agent"), "{elsewhere}");

        let run: Uuid = {
            let session: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'sonnet') RETURNING id",
            )
            .bind(mine.id)
            .fetch_one(&pool)
            .await?;
            sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at) VALUES ($1, $2, 'turn', 'running', 'x', NOW()) RETURNING id",
            )
            .bind(session)
            .bind(mine.id)
            .fetch_one(&pool)
            .await?
        };
        let problems = vec![ambiguous.clone()];
        tell_agent(&store, mine, run, &[], &problems).await?;
        tell_agent(&store, mine, run, &[], &problems).await?;
        let pending: Vec<Value> = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending'")
            .bind(mine.id)
            .bind(PROBLEM_KIND)
            .fetch_all(&pool)
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["problems"][0], json!(ambiguous));
        let notes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND author_kind = 'system' AND run_id = $2")
            .bind(mine.id)
            .bind(run)
            .fetch_one(&pool)
            .await?;
        assert_eq!(notes, 1);

        let later_run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at)
             SELECT session_id, agent_id, kind, status, prompt, started_at FROM session_runs WHERE id = $1 RETURNING id",
        )
        .bind(run)
        .fetch_one(&pool)
        .await?;
        let retry_item = InboxItem {
            id: 0,
            agent_id: mine.id,
            kind: PROBLEM_KIND.into(),
            event_seq: None,
            payload: json!({}),
            status: "processing".into(),
            attempts: 1,
            created_at: chrono::Utc::now(),
        };
        tell_agent(&store, mine, later_run, &[retry_item], &problems).await?;
        let notes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND author_kind = 'system'")
            .bind(mine.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(notes, 2);
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending'")
            .bind(mine.id)
            .bind(PROBLEM_KIND)
            .fetch_one(&pool)
            .await?;
        assert_eq!(pending, 2, "a repeated problem in a retry turn still reaches the agent");
        let retry_item = |id: i64| InboxItem { id, agent_id: mine.id, kind: PROBLEM_KIND.into(), event_seq: None, payload: json!({}), status: "processing".into(), attempts: 1, created_at: chrono::Utc::now() };
        for n in 0..4 {
            tell_agent(&store, mine, later_run, &[retry_item(n)], &[format!("Still failing {n}")]).await?;
        }
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending'")
            .bind(mine.id)
            .bind(PROBLEM_KIND)
            .fetch_one(&pool)
            .await?;
        assert_eq!(pending, 3, "retry turns wake the agent at most {PROBLEM_WAKES_PER_HOUR} times an hour so it cannot loop");
        Ok(())
    }

    fn task_ref(id: &str, title: &str) -> TaskRef {
        TaskRef { id: id.parse().unwrap(), title: title.into(), status: "todo".into(), assigned_agent_id: None, user_id: Uuid::nil() }
    }

    #[test]
    fn full_ids_and_prefixes_are_told_apart() {
        let full = "50eed204-1111-4222-8333-944455556666";
        assert_eq!(task_key(full, "start task"), Ok(TaskKey::Full(full.parse().unwrap())));
        assert_eq!(task_key(" 50EED204 ", "start task"), Ok(TaskKey::Prefix("50eed204".into())));
        assert_eq!(task_key("50eed204-1111", "start task"), Ok(TaskKey::Prefix("50eed204-1111".into())));
        assert!(task_key("50eed2", "start task").unwrap_err().contains("not a task id"));
        assert!(task_key("not-a-task-id", "start task").is_err());
        assert!(task_key("", "start task").is_err());
    }

    #[test]
    fn a_unique_prefix_resolves_and_bad_ones_say_why() {
        let a = task_ref("50eed204-1111-4222-8333-944455556666", "Fix start_task");
        let b = task_ref("50eed204-9999-4222-8333-944455556666", "Other quest");
        assert_eq!(unique_task("50eed204-1", std::slice::from_ref(&a), "start task").map(|task| task.id), Ok(a.id));
        let ambiguous = unique_task("50eed204", &[a.clone(), b.clone()], "start task").unwrap_err();
        assert!(ambiguous.contains("50eed204") && ambiguous.contains("matches 2 tasks") && ambiguous.contains("Other quest"));
        let unknown = unique_task("deadbeef", &[], "start task").unwrap_err();
        assert!(unknown.contains("deadbeef") && unknown.contains("no task"));
    }

    #[test]
    fn sibling_attempts_are_relative() {
        assert_eq!(relative_dir("tasks/t/repo", "tasks/t/repo"), ".");
        assert_eq!(relative_dir("tasks/t/repo", "tasks/t/repo-2"), "../repo-2");
        assert_eq!(relative_dir("/w/t/repo", "/x/repo-3"), "/x/repo-3");
        assert_eq!(attempt_dir("tasks/t/repo", 1), "tasks/t/repo");
        assert_eq!(attempt_dir("tasks/t/repo", 3), "tasks/t/repo-3");
    }

    #[test]
    fn unreadable_pick_falls_back_to_first() {
        let session = |attempt: i16| -> AgentSession {
            serde_json::from_value(json!({
                "id": Uuid::from_u128(attempt as u128), "agent_id": Uuid::nil(), "role": "task", "backend": "claude", "model": "opus",
                "cli_session_id": null, "parent_session_id": null, "task_id": null, "working_dir": null,
                "status": "active", "usage_baseline": {}, "context_tokens": 0, "review_rounds": 0, "summary": null,
                "rules_hash": null, "asked_by": null, "attempt": attempt,
                "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now()
            }))
            .unwrap()
        };
        let all = [session(1), session(3)];
        let (winner, verdict) = pick(Some("<!-- verdict {\"verdict\": \"pick\", \"winner\": 3, \"summary\": \"cleaner\"} verdict -->"), &all);
        assert_eq!(winner, Uuid::from_u128(3));
        assert_eq!(verdict["summary"], "cleaner");
        let (winner, verdict) = pick(Some("I like both"), &all);
        assert_eq!(winner, Uuid::from_u128(1));
        assert_eq!(verdict["note"], "unparsed");
    }

    #[test]
    fn only_clean_fast_quests_skip_the_report_back() {
        let pace = |p: &str| QuestOptions { attempts: 1, pace: p.into(), ..Default::default() };
        let done = prompts::Report { status: Some("done".into()), ..Default::default() };
        assert!(wakes_main(&pace("normal"), &done, None, false));
        assert!(wakes_main(&QuestOptions::default(), &done, None, false));
        assert!(!wakes_main(&pace("quick"), &done, None, false));
        assert!(!wakes_main(&pace("urgent"), &done, None, false));
        assert!(wakes_main(&pace("quick"), &done, Some("push failed"), false));
        assert!(wakes_main(&pace("urgent"), &done, None, true));
        let blocked = prompts::Report { status: Some("blocked".into()), ..Default::default() };
        assert!(wakes_main(&pace("quick"), &blocked, None, false));
        let open = prompts::Report { followups: vec!["Which table?".into()], ..done.clone() };
        assert!(wakes_main(&pace("quick"), &open, None, false));
        let found = prompts::Report { findings: Some("# Notes".into()), ..done };
        assert!(wakes_main(&pace("urgent"), &found, None, false));
    }

    #[test]
    fn one_finish_rule_reviews_what_was_never_reviewed_and_never_dead_ends() {
        use loka_shared::runners::PrSync;
        let report = |status: Option<&str>| prompts::Report { status: status.map(String::from), summary: Some("Ship it".into()), ..Default::default() };
        let done = report(Some("done"));
        let approve = json!({ "verdict": "approve" });
        let changes = json!({ "verdict": "changes", "summary": "test is failing" });
        let unreadable = json!({ "verdict": prompts::NO_VERDICT });
        let crashed = json!({ "verdict": "review failed", "summary": "timed out" });

        assert_eq!(finish_step(&done, true, None), Finish::Review, "wanted and never reviewed: review it");
        assert_eq!(finish_step(&done, false, None), Finish::Publish, "review off: publish");
        for verdict in [&approve, &changes, &unreadable] {
            assert_eq!(finish_step(&done, true, Some(verdict)), Finish::Publish, "{verdict}: a finished review publishes");
            assert_eq!(finish_step(&done, false, Some(verdict)), Finish::Publish, "{verdict}");
        }
        assert_eq!(finish_step(&done, true, Some(&crashed)), Finish::Fail("Independent review failed: timed out".into()));
        assert_eq!(finish_error(&done, Some(&json!({ "verdict": "review failed" }))).as_deref(), Some("Independent review failed"));

        let blocked = report(Some("blocked"));
        assert_eq!(finish_step(&blocked, true, None), Finish::Fail("Task reported blocked: Ship it".into()));
        assert_eq!(finish_step(&blocked, false, Some(&approve)), Finish::Fail("Task reported blocked: Ship it".into()));
        assert_eq!(finish_step(&prompts::Report::default(), true, None), Finish::Fail("Task did not provide a valid completion report".into()));
        let quiet = prompts::Report { status: Some("blocked".into()), summary: Some(" ".into()), ..Default::default() };
        assert_eq!(finish_step(&quiet, false, None), Finish::Fail("Task reported blocked".into()));
        let lost = prompts::Report::synthesized("No commit performed.", None);
        assert_eq!(finish_step(&lost, true, None), Finish::Fail(prompts::SYNTHESIZED_ERROR.into()));

        for was_completed in [false, true] {
            for has_repo in [false, true] {
                for sync in [PrSync::Changed, PrSync::Unchanged] {
                    let step = |reviews: bool, wants: bool, verdict: Option<&Value>| followup_step(was_completed, &done, reviews, wants, false, has_repo, sync, verdict);
                    assert_eq!(step(true, true, None), FollowupStep::Review, "{sync:?}: a done follow-up whose work has no review gets one");
                    assert_eq!(step(true, true, Some(&approve)), FollowupStep::Settle, "{sync:?}: reviewed work publishes without a new review");
                    assert_eq!(step(true, true, Some(&changes)), FollowupStep::Settle, "{sync:?}: rounds ran out, the work still publishes");
                    assert_eq!(step(true, false, None), FollowupStep::Settle, "{sync:?}: review off");
                    assert_eq!(step(false, true, None), FollowupStep::Settle, "{sync:?}: reviews off everywhere");
                    assert_eq!(followup_step(was_completed, &done, true, true, false, has_repo, PrSync::Merged, None), FollowupStep::Settle, "a merged PR settles");
                }
            }
        }
        assert_eq!(followup_step(false, &blocked, true, true, false, true, PrSync::Unchanged, None), FollowupStep::Fail("Task reported blocked: Ship it".into()));
        assert_eq!(followup_step(true, &blocked, true, true, false, true, PrSync::Unchanged, None), FollowupStep::Leave);
        assert_eq!(followup_step(false, &blocked, true, true, false, true, PrSync::Merged, None), FollowupStep::Settle);
        assert_eq!(followup_step(true, &blocked, true, true, false, true, PrSync::Merged, None), FollowupStep::Settle);
        assert_eq!(followup_step(false, &prompts::Report::default(), true, true, true, true, PrSync::Merged, None), FollowupStep::Settle);
    }
}

pub fn incomplete_report(report: &prompts::Report) -> Option<String> {
    if report.status.as_deref() == Some("done") {
        return None;
    }
    if report.synthesized {
        return Some(prompts::SYNTHESIZED_ERROR.to_string());
    }
    let summary = report.summary.as_deref().filter(|summary| !summary.trim().is_empty());
    Some(match (report.status.as_deref(), summary) {
        (status, Some(summary)) => format!("Task reported {}: {summary}", status.unwrap_or("no status")),
        (Some(status), None) => format!("Task reported {status}"),
        (None, None) => "Task did not provide a valid completion report".to_string(),
    })
}

pub fn salvages(report: &prompts::Report, has_repo: bool) -> bool {
    report.synthesized && has_repo
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowupStep {
    Nudge,
    Salvage,
    Fail(String),
    Leave,
    Review,
    Settle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Publish,
    Review,
    Fail(String),
}

pub fn finish_error(report: &prompts::Report, verdict: Option<&Value>) -> Option<String> {
    if let Some(error) = incomplete_report(report) {
        return Some(error);
    }
    if verdict.and_then(|value| value.get("verdict")).and_then(Value::as_str) != Some("review failed") {
        return None;
    }
    Some(match verdict.and_then(|value| value.get("summary")).and_then(Value::as_str).filter(|summary| !summary.trim().is_empty()) {
        Some(summary) => format!("Independent review failed: {summary}"),
        None => "Independent review failed".to_string(),
    })
}

pub fn finish_step(report: &prompts::Report, review_wanted: bool, verdict: Option<&Value>) -> Finish {
    if let Some(error) = finish_error(report, verdict) {
        Finish::Fail(error)
    } else if review_wanted && verdict.is_none() {
        Finish::Review
    } else {
        Finish::Publish
    }
}

pub async fn review_on_record(store: &WorldStore, session_id: Uuid, run_id: Uuid, pr_sync: loka_shared::runners::PrSync) -> Result<Option<String>> {
    if pr_sync == loka_shared::runners::PrSync::Unchanged {
        store.mark_unchanged_work(run_id).await?;
    }
    store.latest_review_output(session_id).await
}

#[allow(clippy::too_many_arguments)]
pub fn followup_step(
    was_completed: bool,
    report: &prompts::Report,
    reviews_enabled: bool,
    wants_review: bool,
    nudge_available: bool,
    has_repo: bool,
    pr_sync: loka_shared::runners::PrSync,
    verdict: Option<&Value>,
) -> FollowupStep {
    if pr_sync == loka_shared::runners::PrSync::Merged {
        return FollowupStep::Settle;
    }
    if let Some(error) = incomplete_report(report) {
        if was_completed {
            return FollowupStep::Leave;
        }
        if report.status.is_none() && nudge_available {
            return FollowupStep::Nudge;
        }
        if salvages(report, has_repo) {
            return FollowupStep::Salvage;
        }
        return FollowupStep::Fail(error);
    }
    match finish_step(report, reviews_enabled && wants_review, verdict) {
        Finish::Review => FollowupStep::Review,
        Finish::Publish => FollowupStep::Settle,
        Finish::Fail(error) => FollowupStep::Fail(error),
    }
}

pub async fn pr_action_facts(store: &WorldStore, agent: &WorldAgent, task_id: Uuid, repo: &str) -> Result<pr_actions::Facts> {
    let record = store.quest_record(task_id).await?.ok_or_else(|| anyhow!("the quest is gone"))?;
    let granted = store.grants_repo(agent.owner_user_id, agent.org_id, repo).await?;
    let review = match record.review_output {
        None => pr_actions::Review::Missing,
        Some(output) => {
            let verdict = output.as_deref().and_then(prompts::parse_verdict);
            pr_actions::review_from_verdict(verdict.as_ref().map(|v| v.verdict.as_str()))
        }
    };
    let checks = record
        .report_output
        .as_deref()
        .and_then(prompts::parse_report)
        .map(|report| pr_actions::checks_on_record(&report.checks))
        .unwrap_or(pr_actions::Checks::Missing);
    Ok(pr_actions::Facts {
        granted,
        quest_repo: record.repo,
        quest_pr: record.pr_number,
        quest_branch: record.branch,
        quest_cancelled: record.cancelled,
        published_head: record.published_head,
        review,
        checks,
    })
}

pub async fn pr_action_job(store: &WorldStore, schedule: &AgentSchedule) -> Result<pr_actions::ActionJob> {
    let spec: pr_actions::ActionSpec = serde_json::from_value(schedule.action.clone().ok_or_else(|| anyhow!("the schedule holds no action"))?)?;
    let task_id = schedule.task_id.ok_or_else(|| anyhow!("the action has no quest"))?;
    let agent = store.get_agent(schedule.agent_id).await?.ok_or_else(|| anyhow!("the agent is gone"))?;
    let facts = pr_action_facts(store, &agent, task_id, &spec.repo).await?;
    Ok(pr_actions::ActionJob {
        schedule_id: schedule.id,
        trigger_problem: pr_actions::trigger_problem(schedule.on_event.as_deref(), schedule.fired.as_ref()),
        spec,
        facts,
    })
}
