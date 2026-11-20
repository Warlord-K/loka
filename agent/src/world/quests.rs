use std::path::Path;

use anyhow::{anyhow, Result};
use loka_shared::milestones::{self, MilestoneStore};
use loka_shared::models::AgentTask;
use loka_shared::quests::{QuestChoice, QuestOptions, PLAN_FIRST};
use loka_shared::world::{AgentSession, NewRun, NewSession, ReviewerChoice, SessionRun, WorldAgent, WorldStore};
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
    let mut opened = Vec::new();
    for (i, dir) in dirs.iter().enumerate() {
        let attempt = i as i16 + 1;
        let variant = racing.then(|| options.variant(attempt)).flatten();
        let backend = variant.as_ref().and_then(|v| v.cli.clone()).unwrap_or_else(|| main.backend.clone());
        let model = variant.as_ref().map(|v| v.model.clone()).unwrap_or_else(|| main.model.clone());
        let fork = backend == main.backend;
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
        if let Some(brief) = brief {
            prompt.push_str(&format!("\n\nYour brief:\n{brief}"));
        }
        if racing {
            store.mark_attempt(session.id, attempt).await?;
            prompt.push_str(&format!(
                "\n\nYou are attempt {attempt} of {} at this quest. The other attempts work on the same task at the same time in their own workspaces, and an independent judge picks the one that ships. Work only in your workspace and do your best complete version.",
                dirs.len()
            ));
        } else if options.plan_first {
            store.begin_plan(session.id).await?;
            prompt.push_str(&format!("\n\n{PLAN_FIRST}"));
        }
        if !fork {
            let intro = format!(
                "[fresh session] You are {}. This is a fresh {backend} session for this quest, so you do not have your main conversation, only these notes.",
                agent.name
            );
            prompt = format!("{}\n\n{prompt}", store.handoff_memory(agent.id, &intro).await?);
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
            })
            .await?;
        opened.push((session, run));
    }
    Ok(opened)
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

pub fn started_payload(task: &AgentTask, options: &QuestOptions, attempts: usize) -> Value {
    let mut payload = json!({ "task_id": task.id, "title": task.title, "repo": task.repo });
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
}
