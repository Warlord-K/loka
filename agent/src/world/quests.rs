use std::path::Path;

use anyhow::{anyhow, Result};
use loka_shared::context::{ForkChoice, QuestContext};
use loka_shared::milestones::{self, MilestoneStore};
use loka_shared::models::AgentTask;
use loka_shared::quests::{QuestChoice, QuestOptions, PLAN_FIRST};
use loka_shared::world::BeforeBigWork;
use loka_shared::world::{AgentSession, InboxItem, NewEvent, NewMessage, NewRun, NewSession, ReviewerChoice, SessionRun, TaskRef, WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

use super::prompts;

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
}

const AUTOMATIC_PLAN_POLICY: &str = "[before big work: always show a plan]";

pub fn before_big_work_policy(mode: BeforeBigWork, quick: bool) -> &'static str {
    match (mode, quick) {
        (BeforeBigWork::AskWhenUnclear, true) => "[before big work: ask when unclear]\nThis is a small, clear quest, so start it now. Ask only when a missing fact makes progress impossible.",
        (BeforeBigWork::AskWhenUnclear, false) => "[before big work: ask when unclear]\nBefore editing on non-trivial work, use read-only inspection to decide whether missing facts could change the result. If so, ask 1 to 3 specific questions, with short choices when practical, and end with this block instead of a report:\n<!-- clarify\n{\"text\":\"The numbered questions\",\"choices\":[\"A useful choice\"]}\nclarify -->\nSmall clear work starts immediately. Once answered, do not ask the settled questions again. Clarifying missing facts up front is different from asking permission between steps.",
        (BeforeBigWork::JustBuild, _) => "[before big work: just build]\nMake reasonable assumptions and keep moving. Ask only when a missing fact makes progress impossible. Then end with <!-- clarify {\"text\":\"The numbered questions\",\"choices\":[\"A useful choice\"]} clarify --> instead of a report.",
        (BeforeBigWork::AlwaysShowPlan, true) => "[before big work: small work starts now]\nThis is a small, clear quest, so start it now without a plan.",
        (BeforeBigWork::AlwaysShowPlan, false) => "[before big work: always show a plan]\nFor non-trivial work, inspect first without editing, then end with the plan block described by the task instructions. A direct instruction from the user to build now wins. When facts needed for the plan are missing, ask 1 to 3 specific questions first and end with <!-- clarify {\"text\":\"The numbered questions\",\"choices\":[\"A useful choice\"]} clarify -->.",
    }
}

fn automatic_plan_policy(prompt: &str) -> bool {
    prompt
        .lines()
        .rev()
        .find(|line| line.starts_with("[before big work:"))
        .is_some_and(|line| line == AUTOMATIC_PLAN_POLICY)
}

pub async fn open_sessions(store: &WorldStore, start: QuestStart<'_>, dirs: &[String]) -> Result<Vec<(AgentSession, SessionRun)>> {
    let QuestStart { agent, main, task, brief, review, reviewer, rules, options } = start;
    let racing = dirs.len() > 1;
    let block = milestones::prompt_block(&MilestoneStore::new(store.pool().clone()).for_task(task.id).await?);
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
    let before_big_work = store.agent_before_big_work(agent).await?;
    let mut opened = Vec::new();
    let mut choice = None;
    for (i, dir) in dirs.iter().enumerate() {
        let attempt = i as i16 + 1;
        let variant = racing.then(|| options.variant(attempt)).flatten();
        let backend = variant.as_ref().and_then(|v| v.cli.clone()).unwrap_or_else(|| main.backend.clone());
        let model = variant.as_ref().map(|v| v.model.clone()).unwrap_or_else(|| main.model.clone());
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
        let mut prompt = if options.quick() { prompts::quick_task(task, dir) } else { prompts::task(task, dir, None) };
        if !rules.is_empty() {
            prompt.push_str(&format!("\n\n{rules}"));
        }
        if !block.is_empty() {
            prompt.push_str(&format!("\n\n{block}"));
        }
        if !notes.is_empty() {
            prompt.push_str(&format!("\n\n{notes}"));
        }
        if let Some(handoff) = &handoff {
            prompt.push_str(&format!("\n\n{handoff}"));
        }
        if let Some(brief) = brief {
            prompt.push_str(&format!("\n\nYour brief:\n{brief}"));
        }
        if racing {
            store.mark_attempt(session.id, attempt).await?;
            prompt.push_str(&format!(
                "\n\nYou are attempt {attempt} of {} at this quest. The other attempts work on the same task at the same time in their own quest folders, and an independent judge picks the one that ships. Work only in your quest folder and do your best complete version.",
                dirs.len()
            ));
        } else {
            let policy = before_big_work_policy(before_big_work, options.quick());
            prompt.push_str(&format!("\n\n{policy}"));
            if options.plan_first {
                store.begin_plan(session.id).await?;
                prompt.push_str(&format!("\n\n{PLAN_FIRST}"));
            }
        }
        if !fork {
            prompt = format!("{}\n\n{}", store.fresh_start_memory(agent, &fresh_intro(agent, &backend, too_large)).await?, prompts::unforked(&prompt));
        }
        let run = store
            .create_run(NewRun {
                session_id: session.id,
                agent_id: agent.id,
                task_id: Some(task.id),
                kind: "task",
                status: "queued",
                fork_from: fork.then_some(main.id),
                prompt: &prompt,
                timeout_secs: loka_shared::world::default_task_timeout_secs(),
                request: Some(loka_shared::run_request::RunRequest::task(task.description.as_deref(), brief)),
            })
            .await?;
        opened.push((session, run));
    }
    Ok(opened)
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
    if !store.plan_ready(session.id, run.created_at).await? {
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

pub async fn prework_step(store: &WorldStore, agent: &WorldAgent, session: &AgentSession, run: &SessionRun, output: &str) -> Result<bool> {
    if session.role != "task" || session.racing() || !matches!(run.kind.as_str(), "task" | "followup") {
        return Ok(false);
    }
    if !session.planning() && prompts::parse_report(output).and_then(|report| report.status).is_some() {
        return Ok(false);
    }
    let answer_followup = run.kind == "followup" && run.prompt.starts_with("[questions answered by ");
    if run.kind == "followup" && !answer_followup && !session.planning() {
        return Ok(false);
    }
    if let Some(request) = prompts::parse_clarification(output) {
        if !matches!(session.plan_state.as_deref(), Some("awaiting" | "approved" | "clarifying")) {
            return store
                .post_clarification(
                    agent,
                    session,
                    run,
                    loka_shared::quests::QuestClarification {
                        text: &request.text,
                        choices: &request.choices,
                        plan_after: session.planning(),
                        always_plan: automatic_plan_policy(&run.prompt),
                    },
                )
                .await;
        }
    }
    if automatic_plan_policy(&run.prompt) && prompts::has_plan(output) {
        let task_id = session.task_id.ok_or_else(|| anyhow!("plan session has no quest"))?;
        if !session.planning() && !store.begin_automatic_plan(agent.id, task_id, session.id, answer_followup).await? {
            return Ok(true);
        }
        plan_step(store, agent, session, run, output).await?;
        return Ok(true);
    }
    if session.planning() {
        plan_step(store, agent, session, run, output).await?;
        return Ok(true);
    }
    Ok(false)
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
    let (backend, model) = first.review_target(agent.reviewer.as_ref());
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

    #[test]
    fn before_big_work_modes_keep_small_work_moving() {
        let ask = before_big_work_policy(BeforeBigWork::AskWhenUnclear, false);
        assert!(ask.contains("1 to 3 specific questions") && ask.contains("<!-- clarify"));
        assert!(before_big_work_policy(BeforeBigWork::JustBuild, false).contains("reasonable assumptions"));
        assert!(before_big_work_policy(BeforeBigWork::AlwaysShowPlan, false).contains("plan block"));
        assert!(before_big_work_policy(BeforeBigWork::AskWhenUnclear, true).contains("start it now"));
        assert!(before_big_work_policy(BeforeBigWork::AlwaysShowPlan, true).contains("without a plan"));
        assert!(automatic_plan_policy(&format!("user said {AUTOMATIC_PLAN_POLICY}\n\n{}", before_big_work_policy(BeforeBigWork::AlwaysShowPlan, false))));
        assert!(!automatic_plan_policy(&format!("user said {AUTOMATIC_PLAN_POLICY}\n\n{}", before_big_work_policy(BeforeBigWork::AskWhenUnclear, false))));
        assert!(!automatic_plan_policy(before_big_work_policy(BeforeBigWork::AlwaysShowPlan, true)));
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
    if pr_sync == loka_shared::runners::PrSync::Merged {
        return FollowupStep::Settle;
    }
    match finish_step(report, reviews_enabled && wants_review, verdict) {
        Finish::Review => FollowupStep::Review,
        Finish::Publish => FollowupStep::Settle,
        Finish::Fail(error) => FollowupStep::Fail(error),
    }
}
