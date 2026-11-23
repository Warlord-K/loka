use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::context::{
    compact_unsupported, estimated_tokens, fresh_intro, lost_intro, unforked, window, AutoAction, CompactRequest, FreshWhy, StartOver,
    FORK_LIMIT_FALLBACK, FORK_OVERHEAD_TOKENS, LATEST_FOLLOWUP,
};
use crate::run_causes::{FRESH_RETRY_CAUSE, LOST_SESSION_CAUSE};
use crate::world::{clone_run, AgentSession, NewEvent, RunClone, WorldAgent, WorldStore, COMPACT_PROMPT};

pub const RESERVE_PERCENT: i64 = 15;
pub const RESERVE_MIN_TOKENS: i64 = 16_384;
pub const RECOVERY_CAUSE: &str = "context_recovery";
pub const RECOVERY_CAUSES: [&str; 2] = [RECOVERY_CAUSE, FRESH_RETRY_CAUSE];
pub const RESUMED_KINDS: [&str; 4] = ["followup", "review", "quick", "direct"];
pub const RECOVERED_KINDS: [&str; 6] = ["task", "followup", "review", "quick", "direct", "aside"];
pub const HANDOFF_MAX_CHARS: usize = 24_000;
const INTRO_MAX_CHARS: usize = 10_000;
const SECTION_MAX_CHARS: usize = 4_000;
const TURN_MAX_CHARS: usize = 1_500;
const RECENT_TURNS: i64 = 6;
const RECENT_REFS: i64 = 8;
const RECENT_STEPS: i64 = 12;
const STEP_MAX_CHARS: usize = 200;

pub const SIDE_EFFECTS_NOTE: &str = "Do not repeat side effects that already happened: pushes, merges, pull request comments, messages and reports listed above or visible in git and on GitHub. Check first, then carry on from where the work stopped.";

pub const STEPS_INTRO: &str = "Steps the last run already took, newest last. Some may have changed things outside this session (a push, a merge, a message), so check before you repeat any of them:";

pub const REVIEW_RANGES_NOTE: &str = "[context recovered] An earlier pass of this review ran out of context. Start with `git diff --stat` against the base, then read the changes one file at a time and in ranges. Never print the whole diff at once.";

pub fn reserve(window: i64) -> i64 {
    (window * RESERVE_PERCENT / 100).max(RESERVE_MIN_TOKENS)
}

pub fn fixed_limit() -> Option<i64> {
    std::env::var("WORLD_COMPACT_TOKENS").ok().and_then(|raw| raw.trim().parse::<i64>().ok())
}

pub fn budget_with(window: Option<i64>, fixed: Option<i64>) -> i64 {
    let natural = window.map_or(FORK_LIMIT_FALLBACK, |tokens| tokens - reserve(tokens));
    fixed.filter(|fixed| *fixed > 0 && *fixed < natural).unwrap_or(natural)
}

pub fn budget(cli: &str, model: &str) -> i64 {
    budget_with(window(cli, model), fixed_limit())
}

pub fn main_compact_limit(cli: &str, model: &str) -> Option<i64> {
    (fixed_limit() != Some(0)).then(|| budget(cli, model))
}

pub fn need(used: i64, incoming_bytes: i64) -> i64 {
    used.max(0) + estimated_tokens(0, incoming_bytes)
}

pub fn fits_fresh(prompt_bytes: i64, budget: i64) -> bool {
    estimated_tokens(0, prompt_bytes) + FORK_OVERHEAD_TOKENS < budget
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPlan {
    Send,
    Compact,
    Checkpoint,
}

pub fn request_plan(need: i64, budget: i64, native: bool, compacted_last: bool) -> RequestPlan {
    if need < budget {
        RequestPlan::Send
    } else if native && !compacted_last {
        RequestPlan::Compact
    } else {
        RequestPlan::Checkpoint
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPath {
    Native,
    Checkpoint,
    Recovery,
}

impl ContextPath {
    pub fn as_str(self) -> &'static str {
        match self {
            ContextPath::Native => "native",
            ContextPath::Checkpoint => "checkpoint",
            ContextPath::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    Send,
    Compact,
    Checkpoint { prompt: String },
}

impl Prepared {
    pub fn compact_first(&self) -> bool {
        matches!(self, Prepared::Compact)
    }
}

pub fn checks_before_sending(kind: &str, fork_from: Option<Uuid>, resumes: bool) -> bool {
    RESUMED_KINDS.contains(&kind) && fork_from.is_none() && resumes
}

pub fn recovers(kind: &str) -> bool {
    RECOVERED_KINDS.contains(&kind)
}

pub fn recovery_kind(kind: &str) -> &str {
    if kind == "followup" { "task" } else { kind }
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str(" [...]");
    out
}

fn clip_tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    format!("[...] {}", text.chars().skip(count - max).collect::<String>())
}

pub fn report_block(output: &str) -> Option<&str> {
    let start = output.rfind("<!-- report")?;
    let rest = &output[start..];
    let end = rest.find("report -->").map_or(rest.len(), |at| at + "report -->".len());
    Some(&rest[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

impl PrState {
    pub fn of(quest_status: &str, merge_milestone: Option<&str>) -> Self {
        match merge_milestone {
            Some("done") => PrState::Merged,
            Some("failed") => PrState::Closed,
            _ if quest_status == "done" => PrState::Merged,
            _ => PrState::Open,
        }
    }
}

pub fn pr_line(url: &str, state: PrState) -> String {
    match state {
        PrState::Open => format!("Pull request open for this quest: {url}"),
        PrState::Merged => format!("Pull request for this quest is already merged: {url}"),
        PrState::Closed => format!("Pull request for this quest was closed without merging: {url}"),
    }
}

pub fn step_line(tool: Option<&str>, body: &str) -> String {
    let body = clip(&body.split_whitespace().collect::<Vec<_>>().join(" "), STEP_MAX_CHARS);
    match tool.map(str::trim).filter(|tool| !tool.is_empty()) {
        Some(tool) => format!("{tool}: {body}"),
        None => body,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handoff {
    pub intro: String,
    pub brief: Option<String>,
    pub worktree: Option<String>,
    pub last_report: Option<String>,
    pub done_already: Vec<String>,
    pub message_refs: Vec<String>,
    pub steps: Vec<String>,
    pub conversation: Vec<(String, String)>,
    pub note: Option<String>,
    pub latest: Option<(String, String)>,
}

impl Handoff {
    pub fn render(&self) -> String {
        let mut summary = Vec::new();
        let mut push = |title: &str, body: String| {
            if !body.trim().is_empty() {
                summary.push(format!("{title}\n{}", clip(&body, SECTION_MAX_CHARS)));
            }
        };
        if let Some(worktree) = &self.worktree {
            push("[worktree]", worktree.clone());
        }
        if let Some(report) = &self.last_report {
            push("[your last report: decisions, follow-ups and checks]", report.clone());
        }
        if !self.conversation.is_empty() {
            let turns = self
                .conversation
                .iter()
                .map(|(asked, answered)| format!("> {}\n{}", clip(asked, TURN_MAX_CHARS).replace('\n', "\n> "), clip_tail(answered, TURN_MAX_CHARS)))
                .collect::<Vec<_>>()
                .join("\n\n");
            push("[the conversation so far, oldest first]", turns);
        }
        let mut lines = self.done_already.iter().map(|line| format!("- {line}")).collect::<Vec<_>>();
        if !self.message_refs.is_empty() {
            lines.push(format!("- Messages already posted: {}", self.message_refs.join(", ")));
        }
        if !self.steps.is_empty() {
            lines.push(STEPS_INTRO.to_string());
            lines.extend(self.steps.iter().map(|step| format!("  - {step}")));
        }
        lines.push(SIDE_EFFECTS_NOTE.to_string());
        push("[already done]", lines.join("\n"));
        let summary = clip(&summary.join("\n\n"), HANDOFF_MAX_CHARS - INTRO_MAX_CHARS);
        let mut parts = vec![clip(&self.intro, INTRO_MAX_CHARS)];
        if let Some(brief) = self.brief.as_deref().filter(|brief| !brief.trim().is_empty()) {
            parts.push(format!("[the quest brief, as first given]\n{}", brief.trim()));
        }
        parts.push(summary);
        if let Some(note) = &self.note {
            parts.push(note.trim().to_string());
        }
        if let Some((title, latest)) = &self.latest {
            parts.push(format!("{title}\n{latest}"));
        }
        parts.into_iter().filter(|part| !part.trim().is_empty()).collect::<Vec<_>>().join("\n\n")
    }
}

pub fn context_line(path: &str, note: &Value) -> Option<String> {
    let thousands = |key: &str| note.get(key).and_then(Value::as_i64).map(|tokens| tokens / 1000);
    let sizes = match (thousands("need"), thousands("budget")) {
        (Some(need), Some(budget)) => format!(" (about {need} thousand tokens against a budget of {budget} thousand)"),
        _ => String::new(),
    };
    let short = |key: &str| note.get(key).and_then(Value::as_str).map(|id| id.chars().take(8).collect::<String>());
    match path {
        "native" => Some(format!("Context: compacted the session before this request{sizes}.")),
        "checkpoint" => Some(format!("Context: continued in a fresh session from a checkpoint before this request{sizes}.")),
        "recovery" => Some(match (short("recovers"), note.get("action").and_then(Value::as_str)) {
            (Some(failed), _) => format!("Context: continues run {failed} once in a fresh session after it ran out of context."),
            (None, Some(action)) => format!("Context: this turn ran out of context, so the main session was queued to {action}."),
            _ => "Context: recovered after running out of context.".to_string(),
        }),
        _ => None,
    }
    .map(|line| match short("recovered_by") {
        Some(retry) if path != "recovery" => format!("{line} It ran out of context and continues in run {retry}."),
        _ => line,
    })
}

pub fn checkpoint_intro(name: &str, backend: &str, used: i64, budget: i64) -> String {
    format!(
        "[context checkpoint] You are {name}. This session's context reached about {} thousand tokens, over the {} thousand this model can safely take, so Loka continued it in a fresh {backend} session. What follows is a bounded summary of where things stand; trust it, then check the worktree or files before changing anything.",
        used / 1000,
        budget / 1000
    )
}

pub fn recovery_intro(name: &str, backend: &str) -> String {
    format!(
        "[context recovered] You are {name}. Your last run hit the model's context limit, so Loka continued it once in a fresh {backend} session. What follows is a bounded summary of where things stand; trust it, then check the worktree or files before changing anything."
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Checkpoint { used: i64, budget: i64 },
    Recovery,
    Lost,
}

enum Restart {
    Overflow,
    Lost(Box<Lost>),
}

struct Lost {
    target: AgentSession,
    held: Option<String>,
    main_memory: Option<String>,
    fork_memories: Vec<(String, String)>,
}

impl Restart {
    fn why(&self) -> Why {
        match self {
            Restart::Overflow => Why::Recovery,
            Restart::Lost(_) => Why::Lost,
        }
    }

    fn cause(&self) -> &'static str {
        match self {
            Restart::Overflow => RECOVERY_CAUSE,
            Restart::Lost(_) => LOST_SESSION_CAUSE,
        }
    }

    fn activity(&self) -> &'static str {
        match self {
            Restart::Overflow => "Continuing in a fresh session after the context filled up",
            Restart::Lost(_) => "Starting fresh from notes",
        }
    }
}

const FRESH_START_CAUSES: [&str; 3] = [RECOVERY_CAUSE, FRESH_RETRY_CAUSE, LOST_SESSION_CAUSE];

struct RunFacts {
    id: Uuid,
    session_id: Uuid,
    agent_id: Uuid,
    task_id: Option<Uuid>,
    kind: String,
    prompt: String,
    fork_from: Option<Uuid>,
    retry_cause: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    pub used: i64,
    pub reported: bool,
    pub compacted_last: bool,
}

impl WorldStore {
    async fn run_facts(&self, run_id: Uuid) -> Result<Option<RunFacts>> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<Uuid>, String, String, Option<Uuid>, Option<String>, chrono::DateTime<chrono::Utc>)>(
            "SELECT id, session_id, agent_id, task_id, kind, prompt, fork_from, retry_cause, created_at FROM session_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(id, session_id, agent_id, task_id, kind, prompt, fork_from, retry_cause, created_at)| RunFacts {
            id,
            session_id,
            agent_id,
            task_id,
            kind,
            prompt,
            fork_from,
            retry_cause,
            created_at,
        }))
    }

    pub async fn measure_session(&self, session: &AgentSession, before_run: Uuid) -> Result<Measured> {
        let used = self.session_context_tokens(session, Some(before_run)).await?;
        let compacted_last: bool = sqlx::query_scalar(
            "SELECT COALESCE((SELECT context_path = 'native' FROM session_runs
               WHERE session_id = $1 AND id <> $2 AND status IN ('completed', 'failed', 'timed_out')
               ORDER BY created_at DESC, id DESC LIMIT 1), false)",
        )
        .bind(session.id)
        .bind(before_run)
        .fetch_one(self.pool())
        .await?;
        Ok(Measured { used, reported: session.context_tokens > 0, compacted_last })
    }

    pub async fn prepare_request(&self, run_id: Uuid) -> Result<Prepared> {
        let Some(run) = self.run_facts(run_id).await? else {
            return Ok(Prepared::Send);
        };
        let Some(session) = self.get_session(run.session_id).await? else {
            return Ok(Prepared::Send);
        };
        if !checks_before_sending(&run.kind, run.fork_from, session.cli_session_id.is_some()) {
            return Ok(Prepared::Send);
        }
        let measured = self.measure_session(&session, run.id).await?;
        let need = need(measured.used, run.prompt.len() as i64);
        let budget = budget(&session.backend, &session.model);
        let native = compact_unsupported(&session.backend).is_none();
        let plan = request_plan(need, budget, native, measured.compacted_last);
        let note = json!({
            "need": need,
            "budget": budget,
            "window": window(&session.backend, &session.model),
            "used": measured.used,
            "reported": measured.reported,
            "compacted_last": measured.compacted_last,
        });
        match plan {
            RequestPlan::Send => Ok(Prepared::Send),
            RequestPlan::Compact => {
                self.mark_context_path(run.id, ContextPath::Native, note).await?;
                self.forget_sent_context(session.id).await?;
                tracing::info!(run = %run.id, session = %session.id, need, budget, "Compacting the session before sending the next request");
                Ok(Prepared::Compact)
            }
            RequestPlan::Checkpoint => {
                let Some(agent) = self.get_agent(run.agent_id).await? else {
                    return Ok(Prepared::Send);
                };
                let prompt = self.continuation(&agent, &session, &run, Why::Checkpoint { used: need, budget }).await?;
                let checkpointed = self.restart_cli_session(&session, &run, &prompt, ContextPath::Checkpoint, note).await?;
                if !checkpointed {
                    return Ok(Prepared::Send);
                }
                tracing::info!(run = %run.id, session = %session.id, need, budget, "Session over its context budget, continuing in a fresh session from a checkpoint");
                Ok(Prepared::Checkpoint { prompt })
            }
        }
    }

    pub async fn run_context(&self, run_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (Option<String>, Option<Value>)>("SELECT context_path, context_note FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.and_then(|(path, note)| {
            let note = note.unwrap_or(Value::Null);
            match path {
                Some(path) => context_line(&path, &note),
                None => note.get("recovered_by").and_then(Value::as_str).map(|retry| {
                    format!("Context: it ran out of context and continues in run {}.", retry.chars().take(8).collect::<String>())
                }),
            }
        }))
    }

    async fn mark_context_path(&self, run_id: Uuid, path: ContextPath, note: Value) -> Result<()> {
        sqlx::query("UPDATE session_runs SET context_path = $2, context_note = COALESCE(context_note, '{}'::jsonb) || $3 WHERE id = $1")
            .bind(run_id)
            .bind(path.as_str())
            .bind(note)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn restart_cli_session(&self, session: &AgentSession, run: &RunFacts, prompt: &str, path: ContextPath, note: Value) -> Result<bool> {
        let mut tx = self.pool().begin().await?;
        let live = sqlx::query_scalar::<_, bool>("SELECT status IN ('queued', 'running') FROM session_runs WHERE id = $1 FOR UPDATE")
            .bind(run.id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
        let open = sqlx::query_scalar::<_, String>("SELECT status FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some_and(|status| status != "archived");
        if !live || !open {
            return Ok(false);
        }
        fresh_cli_session(&mut tx, session.id, None, None).await?;
        sqlx::query(
            "UPDATE session_runs SET prompt = $2, context_path = $3, context_note = COALESCE(context_note, '{}'::jsonb) || $4
             WHERE id = $1",
        )
        .bind(run.id)
        .bind(prompt)
        .bind(path.as_str())
        .bind(note)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn continuation(&self, agent: &WorldAgent, session: &AgentSession, run: &RunFacts, why: Why) -> Result<String> {
        let lost = why == Why::Lost;
        let intro = match why {
            Why::Checkpoint { used, budget } => checkpoint_intro(&agent.name, &session.backend, used, budget),
            Why::Recovery => recovery_intro(&agent.name, &session.backend),
            Why::Lost => self.fresh_start_memory(agent, &lost_intro(&agent.name, &session.backend, false)).await?,
        };
        let mut handoff = Handoff { intro, ..Handoff::default() };
        match (run.kind.as_str(), run.task_id) {
            ("aside", _) => {
                if !lost {
                    handoff.intro = self.fresh_start_memory(agent, &fresh_intro(&agent.name, &session.backend, None)).await?;
                }
                handoff.latest = Some(("Question:".to_string(), run.prompt.clone()));
            }
            ("task", _) if run.fork_from.is_some() => {
                let used = match run.fork_from {
                    Some(main) => sqlx::query_scalar::<_, i64>("SELECT context_tokens FROM agent_sessions WHERE id = $1")
                        .bind(main)
                        .fetch_optional(self.pool())
                        .await?
                        .unwrap_or(0),
                    None => 0,
                };
                if !lost {
                    handoff.intro = self.fresh_start_memory(agent, &fresh_intro(&agent.name, &session.backend, Some(used))).await?;
                }
                handoff.brief = Some(unforked(&run.prompt));
            }
            ("review", _) => {
                let first = self.first_review_prompt(session.id).await?;
                if first.as_ref().is_some_and(|first| *first != run.prompt) {
                    handoff.latest = Some(("Latest request for this review:".to_string(), run.prompt.clone()));
                }
                handoff.brief = Some(first.unwrap_or_else(|| run.prompt.clone()));
                handoff.note = Some(REVIEW_RANGES_NOTE.to_string());
            }
            ("task" | "followup", Some(task_id)) if session.role == "task" => {
                if !lost {
                    handoff.intro = self.fresh_start_memory(agent, &handoff.intro).await?;
                }
                if let Some((_, _, brief)) = self.quest_session_brief(session.id, task_id).await? {
                    handoff.brief = Some(brief);
                }
                self.fill_worktree(&mut handoff, session, task_id, run.id).await?;
                if run.kind == "followup" {
                    handoff.latest = Some((LATEST_FOLLOWUP.to_string(), run.prompt.clone()));
                } else if run.retry_cause.is_none() && handoff.brief.is_none() {
                    handoff.brief = Some(run.prompt.clone());
                }
            }
            _ => {
                handoff.conversation = self.recent_turns(session.id, run.id).await?;
                handoff.latest = Some(("Latest message:".to_string(), run.prompt.clone()));
            }
        }
        self.fill_already_done(&mut handoff, session, run, why).await?;
        Ok(handoff.render())
    }

    async fn first_review_prompt(&self, session_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT prompt FROM session_runs WHERE session_id = $1 AND kind = 'review' AND retry_cause IS NULL AND nudge IS NULL
               AND context_path IS DISTINCT FROM 'checkpoint'
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn fill_worktree(&self, handoff: &mut Handoff, session: &AgentSession, task_id: Uuid, run_id: Uuid) -> Result<()> {
        let quest = sqlx::query_as::<_, (Option<String>, Option<String>)>("SELECT repo, branch FROM agent_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(self.pool())
            .await?;
        let mut worktree = vec![format!(
            "The work so far is in this folder{}. Run git status and git log before you change anything.",
            session.working_dir.as_deref().map(|dir| format!(" ({dir})")).unwrap_or_default()
        )];
        if let Some((Some(repo), branch)) = quest {
            worktree.push(format!("Repository: {repo}{}", branch.map(|b| format!(", branch {b}")).unwrap_or_default()));
        }
        handoff.worktree = Some(worktree.join("\n"));
        handoff.last_report = sqlx::query_scalar::<_, Option<String>>(
            "SELECT output FROM session_runs WHERE session_id = $1 AND id <> $2 AND status = 'completed'
               AND output LIKE '%<!-- report%'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(session.id)
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?
        .flatten()
        .as_deref()
        .and_then(report_block)
        .map(str::to_string);
        Ok(())
    }

    async fn fill_already_done(&self, handoff: &mut Handoff, session: &AgentSession, run: &RunFacts, why: Why) -> Result<()> {
        if let Some(task_id) = run.task_id.or(session.task_id) {
            let quest = sqlx::query_as::<_, (Option<String>, String, Option<String>)>(
                "SELECT t.pr_url, t.status,
                        (SELECT m.status FROM task_milestones m WHERE m.task_id = t.id AND m.kind = 'merge' ORDER BY m.updated_at DESC LIMIT 1)
                 FROM agent_tasks t WHERE t.id = $1",
            )
            .bind(task_id)
            .fetch_optional(self.pool())
            .await?;
            if let Some((Some(url), status, merge)) = quest {
                handoff.done_already.push(pr_line(&url, PrState::of(&status, merge.as_deref())));
            }
            let published = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND kind = 'publish' AND status = 'completed'",
            )
            .bind(task_id)
            .fetch_one(self.pool())
            .await?;
            if published > 0 {
                handoff.done_already.push(format!("Loka already published this quest's work {published} time(s)."));
            }
            for title in sqlx::query_scalar::<_, String>("SELECT title FROM world_reports WHERE task_id = $1 ORDER BY updated_at DESC LIMIT 4")
                .bind(task_id)
                .fetch_all(self.pool())
                .await?
            {
                handoff.done_already.push(format!("Report already written: {title}"));
            }
        }
        let stopped = match why {
            Why::Recovery | Why::Lost => Some(run.id),
            Why::Checkpoint { .. } => sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM session_runs WHERE session_id = $1 AND id <> $2 AND status IN ('completed', 'failed', 'timed_out')
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(session.id)
            .bind(run.id)
            .fetch_optional(self.pool())
            .await?,
        };
        if let Some(stopped) = stopped {
            handoff.steps = sqlx::query_as::<_, (Option<String>, String)>(
                "SELECT tool, body FROM session_run_events WHERE run_id = $1 AND kind = 'tool' ORDER BY id DESC LIMIT $2",
            )
            .bind(stopped)
            .bind(RECENT_STEPS)
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .rev()
            .map(|(tool, body)| step_line(tool.as_deref(), &body))
            .collect();
        }
        let refs = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_messages
             WHERE agent_id = $1 AND author_kind <> 'user'
               AND (run_id = $2 OR ($3::uuid IS NOT NULL AND meta->>'task_id' = $3::text))
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(session.agent_id)
        .bind(stopped)
        .bind(run.task_id.or(session.task_id))
        .bind(RECENT_REFS)
        .fetch_all(self.pool())
        .await?;
        handoff.message_refs = refs.into_iter().rev().map(|id| format!("msg:{}", &id.simple().to_string()[..8])).collect();
        Ok(())
    }

    async fn recent_turns(&self, session_id: Uuid, before_run: Uuid) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT prompt, output FROM session_runs
             WHERE session_id = $1 AND id <> $2 AND status = 'completed' AND prompt <> $3 AND output IS NOT NULL
             ORDER BY created_at DESC, id DESC LIMIT $4",
        )
        .bind(session_id)
        .bind(before_run)
        .bind(COMPACT_PROMPT)
        .bind(RECENT_TURNS)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().rev().map(|(prompt, output)| (prompt, output.unwrap_or_default())).collect())
    }

    pub async fn recover_overflow(&self, run_id: Uuid, overflowed: bool) -> Result<Option<Uuid>> {
        if !overflowed {
            return Ok(None);
        }
        let Some(run) = self.run_facts(run_id).await? else {
            return Ok(None);
        };
        self.restart_failed_run(run, Restart::Overflow).await
    }

    pub async fn start_over(&self, run_id: Uuid, error: &str) -> Result<StartOver> {
        if let Some(retry) = self.recover_overflow(run_id, crate::cli_errors::is_context_too_long(error)).await? {
            return Ok(StartOver::Retried(retry));
        }
        self.start_over_lost_session(run_id).await
    }

    pub async fn start_over_lost_session(&self, run_id: Uuid) -> Result<StartOver> {
        let row = sqlx::query_as::<_, (Option<String>, Option<String>)>("SELECT error, resumed_cli_id FROM session_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(self.pool())
            .await?;
        let (Some(run), Some((Some(error), resumed_cli_id))) = (self.run_facts(run_id).await?, row) else {
            return Ok(StartOver::Kept);
        };
        let turn = run.kind == "turn";
        let fresh_already = run.retry_cause.as_deref().is_some_and(|cause| FRESH_START_CAUSES.contains(&cause));
        if fresh_already || !(turn || recovers(&run.kind)) || !crate::cli_errors::is_session_missing(&error) {
            return Ok(StartOver::Kept);
        }
        let (Some(agent), Some(session)) = (self.get_agent(run.agent_id).await?, self.get_session(run.session_id).await?) else {
            return Ok(StartOver::Kept);
        };
        if session.status == "archived" {
            return Ok(StartOver::Kept);
        }
        let target = match run.fork_from {
            Some(parent) => Some(parent),
            None if session.cli_session_id.is_some() => Some(session.id),
            None if session.role == crate::quick::ROLE => session.parent_session_id,
            None => None,
        };
        let target = match target {
            Some(id) if id == session.id => Some(session.clone()),
            Some(id) => self.get_session(id).await?,
            None => None,
        };
        let Some(target) = target else {
            return Ok(StartOver::Kept);
        };
        let named = crate::cli_errors::lost_session_id(&error);
        let used = resumed_cli_id.filter(|used| named.as_deref().is_none_or(|named| named == used.as_str()));
        let held = used.filter(|used| target.cli_session_id.as_deref() == Some(used.as_str()) && target.status != "archived");
        let own = target.id == session.id;
        if own && held.is_none() {
            return Ok(StartOver::Kept);
        }
        let main_lost = held.is_some() && target.role == "main";
        let main_memory = match main_lost {
            true => Some(self.fresh_start_memory(&agent, &lost_intro(&agent.name, &target.backend, false)).await?),
            false => None,
        };
        let fork_memories = match main_lost {
            true => self.queued_fork_memories(agent.id, target.id, crate::world::EVERY_FORK, FreshWhy::Lost).await?,
            false => Vec::new(),
        };
        let lost = Lost { target, held, main_memory, fork_memories };
        if !turn {
            return Ok(match self.restart_failed_run(run, Restart::Lost(Box::new(lost))).await? {
                Some(retry) => StartOver::Retried(retry),
                None => StartOver::Kept,
            });
        }
        let mut tx = self.pool().begin().await?;
        if !self.forget_lost(&mut tx, &lost).await? {
            return Ok(StartOver::Kept);
        }
        tx.commit().await?;
        tracing::info!(agent = %agent.slug, session = %lost.target.id, "CLI session was lost, the next turn starts fresh from notes");
        let _ = self.announce_context(agent.id).await;
        Ok(StartOver::Forgotten)
    }

    async fn forget_lost(&self, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, lost: &Lost) -> Result<bool> {
        let Some(held) = lost.held.as_deref() else {
            return Ok(false);
        };
        if !fresh_cli_session(tx, lost.target.id, Some(held), lost.main_memory.as_deref()).await? {
            return Ok(false);
        }
        if lost.target.role == "main" {
            crate::world::unfork_queued_runs(tx, lost.target.id, crate::world::EVERY_FORK, &lost.fork_memories).await?;
        }
        Ok(true)
    }

    async fn restart_failed_run(&self, run: RunFacts, restart: Restart) -> Result<Option<Uuid>> {
        let causes: &[&str] = match restart {
            Restart::Overflow => &RECOVERY_CAUSES,
            Restart::Lost(_) => &FRESH_START_CAUSES,
        };
        if !recovers(&run.kind) || run.retry_cause.as_deref().is_some_and(|cause| causes.contains(&cause)) {
            return Ok(None);
        }
        let Some(session) = self.get_session(run.session_id).await? else {
            return Ok(None);
        };
        let Some(agent) = self.get_agent(run.agent_id).await? else {
            return Ok(None);
        };
        if session.role == "task" && run.task_id.is_none() {
            return Ok(None);
        }
        let prompt = self.continuation(&agent, &session, &run, restart.why()).await?;
        let budget = budget(&session.backend, &session.model);
        if !fits_fresh(prompt.len() as i64, budget) {
            tracing::warn!(run = %run.id, budget, bytes = prompt.len(), "A fresh session would not fit the model either, not retrying");
            return Ok(None);
        }
        let mut tx = self.pool().begin().await?;
        if let Some(task_id) = run.task_id {
            let status = sqlx::query_scalar::<_, String>("SELECT status FROM agent_tasks WHERE id = $1 FOR NO KEY UPDATE")
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?;
            let closed: &[&str] = if run.kind == "task" { &["cancelled", "done"] } else { &["cancelled"] };
            if status.is_none_or(|status| closed.contains(&status.as_str())) {
                return Ok(None);
            }
        }
        let open = sqlx::query_scalar::<_, String>("SELECT status FROM agent_sessions WHERE id = $1 FOR UPDATE")
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some_and(|status| status != "archived");
        let restarted = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM session_runs WHERE session_id = $1 AND retry_cause = ANY($2) AND created_at >= $3)",
        )
        .bind(session.id)
        .bind(causes.iter().map(|cause| cause.to_string()).collect::<Vec<_>>())
        .bind(run.created_at)
        .fetch_one(&mut *tx)
        .await?;
        if !open || restarted {
            return Ok(None);
        }
        let main_forgotten = match &restart {
            Restart::Lost(lost) => {
                let forgot = self.forget_lost(&mut tx, lost).await?;
                if !forgot && lost.target.id == session.id {
                    return Ok(None);
                }
                forgot && lost.target.role == "main"
            }
            Restart::Overflow => false,
        };
        fresh_cli_session(&mut tx, session.id, None, None).await?;
        let retry = clone_run(
            &mut tx,
            run.id,
            RunClone {
                kind: Some(recovery_kind(&run.kind)),
                fork_from: None,
                prompt: &prompt,
                activity: Some(restart.activity()),
                retry_cause: Some(restart.cause()),
                not_before: None,
                retry: true,
            },
        )
        .await?
        .id;
        crate::world::transfer_retry_clarification_resources(&mut tx, run.id, retry).await?;
        let note = json!({ "recovers": run.id, "budget": budget, "window": window(&session.backend, &session.model), "prompt_tokens": estimated_tokens(0, prompt.len() as i64) });
        sqlx::query("UPDATE session_runs SET context_path = 'recovery', context_note = $2, created_at = $3 WHERE id = $1")
            .bind(retry)
            .bind(&note)
            .bind(run.created_at)
            .execute(&mut *tx)
            .await?;
        match restart {
            Restart::Overflow => {
                let note = json!({ "recovers": run.id, "budget": budget, "window": window(&session.backend, &session.model), "prompt_tokens": estimated_tokens(0, prompt.len() as i64) });
                sqlx::query("UPDATE session_runs SET context_path = 'recovery', context_note = $2 WHERE id = $1")
                    .bind(retry)
                    .bind(&note)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE session_runs SET context_note = COALESCE(context_note, '{}'::jsonb) || jsonb_build_object('recovered_by', $2::text) WHERE id = $1")
                    .bind(run.id)
                    .bind(retry.to_string())
                    .execute(&mut *tx)
                    .await?;
            }
            Restart::Lost(_) => {
                sqlx::query(
                    "UPDATE session_runs SET diagnosis = jsonb_set(diagnosis, '{reason}', to_jsonb($2::text)) || '{\"auto_retried\": true}'::jsonb
                     WHERE id = $1 AND diagnosis IS NOT NULL",
                )
                .bind(run.id)
                .bind(crate::cli_errors::LOST_SESSION_RESTARTED)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        let compacting = matches!(restart, Restart::Overflow)
            && run.kind == "task"
            && run.fork_from.is_some()
            && compact_unsupported(&session.backend).is_none()
            && matches!(
                self.request_compact(&agent, json!({ "origin": "fork_failed", "failed_run_id": run.id })).await?,
                CompactRequest::Queued | CompactRequest::AlreadyPending
            );
        if compacting || main_forgotten {
            let _ = self.announce_context(agent.id).await;
        }
        let mut payload = json!({
            "task_id": run.task_id,
            "session_id": session.id,
            "failed_run_id": run.id,
            "retry_run_id": retry,
            "fresh": true,
            "quiet": true,
        });
        let detail = match restart {
            Restart::Overflow => json!({ "cause": RECOVERY_CAUSE, "compacting": compacting, "context_path": ContextPath::Recovery.as_str() }),
            Restart::Lost(_) => json!({
                "cause": crate::run_causes::Cause::CliError.as_str(),
                "reason": crate::cli_errors::LOST_SESSION_RESTARTED,
                "failure": crate::cli_errors::FailureKind::SessionMissing.as_str(),
            }),
        };
        if let (Some(payload), Value::Object(detail)) = (payload.as_object_mut(), detail) {
            payload.extend(detail);
        }
        self.emit(
            NewEvent::new("run.retrying").agent(agent.id).session(session.id).run(retry).task(run.task_id).payload(payload),
            None,
        )
        .await?;
        tracing::info!(agent = %agent.slug, run = %run.id, retry = %retry, kind = %run.kind, cause = restart.cause(), "Continuing once in a fresh session");
        Ok(Some(retry))
    }

    pub async fn recover_main_overflow(&self, agent_id: Uuid, error: &str, compaction_failed: bool) -> Result<Option<AutoAction>> {
        if !crate::cli_errors::is_context_too_long(error) {
            return Ok(None);
        }
        let Some(agent) = self.get_agent(agent_id).await? else {
            return Ok(None);
        };
        let Some(main) = self.main_session(agent.id).await?.filter(|main| main.cli_session_id.is_some()) else {
            return Ok(None);
        };
        let compacted_last: bool = sqlx::query_scalar(
            "SELECT COALESCE((SELECT prompt = $2 FROM session_runs
               WHERE session_id = $1 AND kind = 'turn' AND status = 'completed'
               ORDER BY created_at DESC, id DESC LIMIT 1), false)",
        )
        .bind(main.id)
        .bind(COMPACT_PROMPT)
        .fetch_one(self.pool())
        .await?;
        let native = compact_unsupported(&main.backend).is_none();
        let action = main_overflow_action(native, compacted_last || compaction_failed);
        let detail = json!({
            "origin": "overflow",
            "used_tokens": main.context_tokens,
            "budget": budget(&main.backend, &main.model),
            "action": action.as_str(),
        });
        let queued = match action {
            AutoAction::Compact => matches!(self.request_compact(&agent, detail.clone()).await?, CompactRequest::Queued | CompactRequest::AlreadyPending),
            AutoAction::Reset => self.request_context_reset(agent.id, detail.clone()).await?,
        };
        if !queued {
            return Ok(None);
        }
        let failed_turn = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM session_runs WHERE session_id = $1 AND kind = 'turn' AND status IN ('failed', 'timed_out')
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(main.id)
        .fetch_optional(self.pool())
        .await?;
        if let Some(run) = failed_turn {
            self.mark_context_path(run, ContextPath::Recovery, detail.clone()).await?;
        }
        tracing::info!(agent = %agent.slug, action = action.as_str(), "Main session overflowed, shrinking it before the next turn");
        let _ = self.announce_context(agent.id).await;
        Ok(Some(action))
    }
}

pub fn main_overflow_action(native: bool, compaction_spent: bool) -> AutoAction {
    if native && !compaction_spent { AutoAction::Compact } else { AutoAction::Reset }
}

async fn fresh_cli_session(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, session_id: Uuid, held: Option<&str>, memory: Option<&str>) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE agent_sessions SET cli_session_id = NULL, usage_baseline = '{}'::jsonb, context_tokens = 0, context_sent = '{}'::jsonb,
           rules_hash = NULL, compact_floor = NULL, auto_compact_floor = NULL, rewind = NULL, summary = COALESCE($3, summary),
           asked_by = CASE WHEN role = 'quick' THEN COALESCE(asked_by, '{}'::jsonb) || jsonb_build_object('switched', true) ELSE asked_by END,
           status = CASE WHEN status = 'archived' THEN status ELSE 'active' END, updated_at = NOW()
         WHERE id = $1 AND ($2::text IS NULL OR (cli_session_id = $2 AND status <> 'archived'))",
    )
    .bind(session_id)
    .bind(held)
    .bind(memory)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_the_window_minus_a_reserve_and_a_smaller_fixed_limit_wins() {
        assert_eq!(budget_with(Some(200_000), None), 170_000);
        assert_eq!(budget_with(Some(272_000), None), 231_200);
        assert_eq!(budget_with(Some(1_000_000), None), 850_000);
        assert_eq!(budget_with(Some(64_000), None), 64_000 - RESERVE_MIN_TOKENS);
        assert_eq!(budget_with(None, None), FORK_LIMIT_FALLBACK);
        assert_eq!(budget_with(Some(200_000), Some(120_000)), 120_000, "a fixed limit that fits wins");
        assert_eq!(budget_with(Some(200_000), Some(500_000)), 170_000, "a fixed limit past the window does not");
        assert_eq!(budget_with(Some(200_000), Some(0)), 170_000);
    }

    #[test]
    fn need_is_the_session_size_plus_what_is_about_to_be_sent() {
        assert_eq!(need(150_000, 8_000), 150_000 + 2_000);
        assert_eq!(need(-5, 4_000), 1_000);
        assert_eq!(need(0, 0), 0);
    }

    #[test]
    fn the_plan_compacts_natively_once_then_checkpoints() {
        assert_eq!(request_plan(100_000, 170_000, true, false), RequestPlan::Send);
        assert_eq!(request_plan(170_000, 170_000, true, false), RequestPlan::Compact);
        assert_eq!(request_plan(190_000, 170_000, true, true), RequestPlan::Checkpoint, "a compaction that did not shrink it");
        assert_eq!(request_plan(190_000, 170_000, false, false), RequestPlan::Checkpoint, "a CLI that cannot compact");
    }

    #[test]
    fn only_resumed_sessions_are_checked_before_sending() {
        for kind in RESUMED_KINDS {
            assert!(checks_before_sending(kind, None, true), "{kind}");
            assert!(!checks_before_sending(kind, None, false), "{kind} on a fresh session");
            assert!(!checks_before_sending(kind, Some(Uuid::new_v4()), true), "{kind} forking");
        }
        for kind in ["task", "turn", "publish", "aside"] {
            assert!(!checks_before_sending(kind, None, true), "{kind}");
        }
    }

    #[test]
    fn every_work_and_chat_kind_recovers_and_followups_restart_as_tasks() {
        for kind in ["task", "followup", "review", "quick", "direct", "aside"] {
            assert!(recovers(kind), "{kind}");
        }
        assert!(!recovers("turn") && !recovers("publish"));
        assert_eq!(recovery_kind("followup"), "task");
        assert_eq!(recovery_kind("review"), "review");
    }

    #[test]
    fn a_main_session_that_overflows_compacts_once_then_resets() {
        assert_eq!(main_overflow_action(true, false), AutoAction::Compact);
        assert_eq!(main_overflow_action(true, true), AutoAction::Reset, "compaction already ran or failed");
        assert_eq!(main_overflow_action(false, false), AutoAction::Reset, "a CLI that cannot compact");
    }

    #[test]
    fn a_recovery_fits_only_with_room_for_the_cli_itself() {
        assert!(fits_fresh(40_000, 170_000));
        assert!(!fits_fresh((170_000 - FORK_OVERHEAD_TOKENS) * crate::context::BYTES_PER_TOKEN, 170_000));
    }

    #[test]
    fn the_handoff_bounds_its_summary_and_keeps_the_brief_and_latest_request_whole() {
        let latest = "Please also handle the empty state. ".repeat(400);
        let brief = "[task session] Ship it. End with the report block.\n".repeat(300);
        let handoff = Handoff {
            intro: format!("{}\n{}", checkpoint_intro("Ferris", "claude", 190_000, 170_000), "notes ".repeat(10_000)),
            brief: Some(brief.clone()),
            worktree: Some("The work so far is in this folder.".into()),
            last_report: Some("<!-- report {\"status\": \"blocked\"} report -->".into()),
            done_already: vec!["Pull request already open for this quest: https://github.com/o/r/pull/1".into()],
            message_refs: vec!["msg:1234abcd".into()],
            steps: vec![step_line(Some("Bash"), "git push origin quest-branch\n  --force-with-lease")],
            conversation: vec![("asked".into(), "x".repeat(50_000))],
            note: None,
            latest: Some((LATEST_FOLLOWUP.to_string(), latest.clone())),
        };
        let text = handoff.render();
        assert!(text.starts_with("[context checkpoint] You are Ferris."));
        assert!(text.contains(brief.trim()), "the brief carries the task rules and the report format, so it is never cut");
        assert!(text.ends_with(&format!("{LATEST_FOLLOWUP}\n{latest}")), "the latest request, whole, last");
        let rest = text.chars().count() - brief.trim().chars().count() - latest.chars().count();
        assert!(rest <= HANDOFF_MAX_CHARS + 200, "{rest}");
        assert!(text.contains("https://github.com/o/r/pull/1") && text.contains("msg:1234abcd") && text.contains(SIDE_EFFECTS_NOTE));
        assert!(text.contains(STEPS_INTRO) && text.contains("Bash: git push origin quest-branch --force-with-lease"));
        assert!(text.contains("190 thousand") && text.contains("170 thousand"));
        assert!(!text.contains('\u{2014}'));
    }

    #[test]
    fn the_run_log_says_which_context_path_a_run_took() {
        let note = json!({ "need": 175_000, "budget": 170_000 });
        assert_eq!(
            context_line("native", &note).as_deref(),
            Some("Context: compacted the session before this request (about 175 thousand tokens against a budget of 170 thousand).")
        );
        assert!(context_line("checkpoint", &note).is_some_and(|line| line.contains("fresh session from a checkpoint")));
        let recovery = json!({ "recovers": "0123456789abcdef" });
        assert_eq!(
            context_line("recovery", &recovery).as_deref(),
            Some("Context: continues run 01234567 once in a fresh session after it ran out of context.")
        );
        assert!(context_line("recovery", &json!({ "action": "compact" })).is_some_and(|line| line.contains("queued to compact")));
        assert!(context_line("native", &json!({ "recovered_by": "fedcba9876543210" })).is_some_and(|line| line.ends_with("continues in run fedcba98.")));
        assert_eq!(context_line("other", &note), None);
    }

    #[test]
    fn every_handoff_carries_the_side_effect_section_and_the_real_pr_state() {
        let bare = Handoff { intro: "intro".into(), latest: Some(("Latest message:".into(), "hi".into())), ..Handoff::default() };
        assert!(bare.render().contains(SIDE_EFFECTS_NOTE), "a chat handoff with nothing recorded still says not to repeat side effects");
        assert_eq!(PrState::of("in_progress", None), PrState::Open);
        assert_eq!(PrState::of("review", Some("active")), PrState::Open);
        assert_eq!(PrState::of("in_progress", Some("done")), PrState::Merged);
        assert_eq!(PrState::of("done", None), PrState::Merged);
        assert_eq!(PrState::of("failed", Some("failed")), PrState::Closed);
        assert!(pr_line("u", PrState::Merged).contains("already merged") && pr_line("u", PrState::Closed).contains("closed without merging"));
        assert_eq!(step_line(None, &"x".repeat(500)).chars().count(), STEP_MAX_CHARS + 6);
    }

    #[test]
    fn the_last_report_block_is_found_at_the_end_of_an_output() {
        let output = "work\n<!-- report {\"status\": \"done\"} report -->\nmore <!-- report {\"status\": \"blocked\"} report --> tail";
        assert_eq!(report_block(output), Some("<!-- report {\"status\": \"blocked\"} report -->"));
        assert_eq!(report_block("no block"), None);
    }

    struct Fixture {
        pool: sqlx::PgPool,
        store: WorldStore,
        owner: Uuid,
        agent: Uuid,
        main: Uuid,
        tag: String,
    }

    impl Fixture {
        async fn new(url: &str, cli: &str, model: &str) -> anyhow::Result<Self> {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(crate::world::tests::test_connections())
                .connect(url)
                .await?;
            let store = WorldStore::new(pool.clone());
            let tag = Uuid::new_v4().simple().to_string();
            let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
                .bind(format!("budget-{tag}@example.test"))
                .fetch_one(&pool)
                .await?;
            let agent: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Ferris', $2, $3, $4) RETURNING id",
            )
            .bind(format!("budget-{tag}"))
            .bind(cli)
            .bind(model)
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            let main: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, context_tokens)
                 VALUES ($1, 'main', $2, $3, $4, 20000) RETURNING id",
            )
            .bind(agent)
            .bind(cli)
            .bind(model)
            .bind(format!("main-{tag}"))
            .fetch_one(&pool)
            .await?;
            Ok(Self { pool, store, owner, agent, main, tag })
        }

        async fn quest(&self, status: &str) -> anyhow::Result<Uuid> {
            Ok(sqlx::query_scalar(
                "INSERT INTO agent_tasks (user_id, title, status, pr_url) VALUES ($1, 'Ship the blue widget', $2, 'https://github.com/o/r/pull/7') RETURNING id",
            )
            .bind(self.owner)
            .bind(status)
            .fetch_one(&self.pool)
            .await?)
        }

        async fn session(&self, role: &str, cli: &str, task: Option<Uuid>, context: i64, asked_by: Option<Value>) -> anyhow::Result<Uuid> {
            Ok(sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, task_id, cli_session_id, context_tokens, asked_by, working_dir)
                 VALUES ($1, $2, $3, 'sonnet', $4, $5, $6, $7, $8, '/work/quest') RETURNING id",
            )
            .bind(self.agent)
            .bind(role)
            .bind(cli)
            .bind(self.main)
            .bind(task)
            .bind(format!("{role}-{}", Uuid::new_v4().simple()))
            .bind(context)
            .bind(asked_by)
            .fetch_one(&self.pool)
            .await?)
        }

        async fn run(&self, session: Uuid, task: Option<Uuid>, kind: &str, status: &str, prompt: &str, output: Option<&str>) -> anyhow::Result<Uuid> {
            Ok(sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, error, started_at, finished_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7,
                         CASE WHEN $5 = 'failed' THEN 'API Error: 400 prompt is too long: 210000 tokens > 200000 maximum' END,
                         NOW() - INTERVAL '1 minute', CASE WHEN $5 IN ('queued', 'running') THEN NULL ELSE NOW() - INTERVAL '30 seconds' END)
                 RETURNING id",
            )
            .bind(session)
            .bind(self.agent)
            .bind(task)
            .bind(kind)
            .bind(status)
            .bind(prompt)
            .bind(output)
            .fetch_one(&self.pool)
            .await?)
        }

        async fn set_context(&self, session: Uuid, tokens: i64) -> anyhow::Result<()> {
            sqlx::query("UPDATE agent_sessions SET context_tokens = $2 WHERE id = $1").bind(session).bind(tokens).execute(&self.pool).await?;
            Ok(())
        }

        async fn path_of(&self, run: Uuid) -> anyhow::Result<(Option<String>, Option<Value>)> {
            Ok(sqlx::query_as("SELECT context_path, context_note FROM session_runs WHERE id = $1").bind(run).fetch_one(&self.pool).await?)
        }

        async fn cli_of(&self, session: Uuid) -> anyhow::Result<(Option<String>, i64)> {
            Ok(sqlx::query_as("SELECT cli_session_id, context_tokens FROM agent_sessions WHERE id = $1").bind(session).fetch_one(&self.pool).await?)
        }

        async fn retry_of(&self, run: Uuid) -> anyhow::Result<(String, Option<Uuid>, String, Option<String>, Option<String>)> {
            Ok(sqlx::query_as("SELECT kind, fork_from, prompt, retry_cause, context_path FROM session_runs WHERE id = $1")
                .bind(run)
                .fetch_one(&self.pool)
                .await?)
        }

        async fn cleanup(self) -> anyhow::Result<()> {
            sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(self.agent).execute(&self.pool).await?;
            sqlx::query("DELETE FROM agent_tasks WHERE user_id = $1").bind(self.owner).execute(&self.pool).await?;
            sqlx::query("DELETE FROM users WHERE id = $1").bind(self.owner).execute(&self.pool).await?;
            let _ = &self.tag;
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_resumed_session_over_budget_compacts_natively_once_then_continues_from_a_checkpoint() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = crate::world::tests::steer_db().read().await;
        let f = Fixture::new(&url, "claude", "sonnet").await?;
        let quest = f.quest("in_progress").await?;
        let session = f.session("task", "claude", Some(quest), 100_000, None).await?;
        f.run(session, Some(quest), "task", "completed", "[task session] Build the blue widget.", Some("done\n<!-- report {\"status\": \"done\", \"decisions\": [\"Kept the old API\"]} report -->")).await?;

        let under = f.run(session, Some(quest), "followup", "queued", "Rename the button.", None).await?;
        assert_eq!(f.store.prepare_request(under).await?, Prepared::Send, "a session under its budget sends as is");
        assert_eq!(f.path_of(under).await?.0, None);
        sqlx::query("UPDATE session_runs SET status = 'completed', output = 'ok' WHERE id = $1").bind(under).execute(&f.pool).await?;

        f.set_context(session, 175_000).await?;
        sqlx::query("UPDATE agent_sessions SET context_sent = $2 WHERE id = $1")
            .bind(session)
            .bind(json!({ "skills": crate::context::block_hash("skills"), "instructions": crate::context::block_hash("rules") }))
            .execute(&f.pool)
            .await?;
        let native = f.run(session, Some(quest), "followup", "queued", "Add the empty state.", None).await?;
        assert_eq!(f.store.prepare_request(native).await?, Prepared::Compact, "over budget on a CLI that compacts");
        let (path, note) = f.path_of(native).await?;
        assert_eq!(path.as_deref(), Some("native"));
        let note = note.expect("note");
        assert_eq!((note["budget"].as_i64(), note["window"].as_i64()), (Some(170_000), Some(200_000)));
        assert!(f.cli_of(session).await?.0.is_some(), "native compaction keeps the CLI session");
        let sent: Value = sqlx::query_scalar("SELECT context_sent FROM agent_sessions WHERE id = $1").bind(session).fetch_one(&f.pool).await?;
        assert_eq!(sent, json!({}), "compaction drops what the session was told, so the next request sends the rules and skills again");
        sqlx::query("UPDATE session_runs SET status = 'completed', output = 'ok' WHERE id = $1").bind(native).execute(&f.pool).await?;

        let stuck = f.run(session, Some(quest), "followup", "running", "Fix the red review comment.", None).await?;
        let prompt = match f.store.prepare_request(stuck).await? {
            Prepared::Checkpoint { prompt } => prompt,
            other => panic!("a compaction that left the session over budget must checkpoint, got {other:?}"),
        };
        assert!(prompt.contains("[context checkpoint] You are Ferris."), "{prompt}");
        assert!(prompt.contains("Build the blue widget."), "the quest brief");
        assert!(prompt.contains("Kept the old API"), "the last report's decisions");
        assert!(prompt.contains("/work/quest") && prompt.contains("git log"), "the worktree");
        assert!(prompt.contains("https://github.com/o/r/pull/7") && prompt.contains(SIDE_EFFECTS_NOTE), "side effects already done");
        assert!(prompt.ends_with(&format!("{LATEST_FOLLOWUP}\nFix the red review comment.")), "the latest request, whole, last");
        let (stored, kind): (String, String) = sqlx::query_as("SELECT prompt, kind FROM session_runs WHERE id = $1").bind(stuck).fetch_one(&f.pool).await?;
        assert_eq!((stored.as_str(), kind.as_str()), (prompt.as_str(), "followup"));
        assert_eq!(f.path_of(stuck).await?.0.as_deref(), Some("checkpoint"));
        assert!(f.store.run_context(stuck).await?.is_some_and(|line| line.contains("checkpoint")), "run_log shows the path");
        assert_eq!(f.cli_of(session).await?, (None, 0), "the next request starts a fresh CLI session");
        sqlx::query("UPDATE session_runs SET status = 'completed', output = 'ok' WHERE id = $1").bind(stuck).execute(&f.pool).await?;

        let after = f.run(session, Some(quest), "followup", "queued", "One more thing.", None).await?;
        assert_eq!(f.store.prepare_request(after).await?, Prepared::Send, "a fresh session is never checked again before it resumes");

        let cursor_session = f.session("task", "cursor", Some(quest), 0, None).await?;
        f.run(cursor_session, Some(quest), "task", "completed", "[task session] Build it.", Some(&"x".repeat(800_000))).await?;
        let cursor_run = f.run(cursor_session, Some(quest), "followup", "queued", "Next step.", None).await?;
        assert!(
            matches!(f.store.prepare_request(cursor_run).await?, Prepared::Checkpoint { .. }),
            "a CLI that cannot compact and reports no size is measured from the stored transcript and checkpoints"
        );
        assert_eq!(f.path_of(cursor_run).await?.0.as_deref(), Some("checkpoint"));
        f.cleanup().await
    }

    #[tokio::test]
    async fn an_overflow_at_any_turn_recovers_once_in_a_fresh_session_for_every_kind() -> anyhow::Result<()> {
        use crate::run_causes::{diagnose, RunEnd};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = crate::world::tests::steer_db().read().await;
        let f = Fixture::new(&url, "claude", "sonnet").await?;
        let quest = f.quest("in_progress").await?;
        let session = f.session("task", "claude", Some(quest), 150_000, None).await?;
        f.run(session, Some(quest), "task", "completed", "[task session] Build the blue widget.", Some("ok")).await?;
        let followup = f.run(session, Some(quest), "followup", "failed", "Handle the empty state.", None).await?;
        let late = diagnose(&RunEnd { kind: "followup", error: Some("Prompt is too long"), output: "", turns: 20, timed_out: false });
        assert!(late.overflowed() && late.turns == 20);
        let retry = f.store.auto_retry(followup, &late).await?.expect("an overflow at turn 20 recovers");
        let (kind, fork_from, prompt, cause, path) = f.retry_of(retry).await?;
        assert_eq!((kind.as_str(), fork_from, cause.as_deref(), path.as_deref()), ("task", None, Some(RECOVERY_CAUSE), Some("recovery")));
        assert!(prompt.contains("[context recovered]") && prompt.contains("Build the blue widget.") && prompt.contains("Handle the empty state."));
        assert_eq!(f.path_of(followup).await?.1.expect("note")["recovered_by"], json!(retry.to_string()));
        assert_eq!(f.cli_of(session).await?, (None, 0));
        assert!(f.store.run_context(followup).await?.is_some_and(|line| line.contains(&retry.simple().to_string()[..8])), "run_log points at the recovery");
        assert!(f.store.run_context(retry).await?.is_some_and(|line| line.contains("once in a fresh session")));
        assert_eq!(f.store.auto_retry(followup, &late).await?, None, "once per overflow");
        sqlx::query("UPDATE session_runs SET status = 'failed' WHERE id = $1").bind(retry).execute(&f.pool).await?;
        assert_eq!(f.store.recover_overflow(retry, true).await?, None, "a recovery that overflows again is never retried");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE session_id = $1").bind(session).fetch_one(&f.pool).await?;
        assert_eq!(runs, 3);

        let review = f.session("review", "claude", Some(quest), 190_000, None).await?;
        f.run(review, Some(quest), "review", "completed", "Review the diff of quest blue widget.", Some("partial")).await?;
        let nudge = f.run(review, Some(quest), "review", "failed", "[verdict missing] Give the verdict.", None).await?;
        sqlx::query("UPDATE session_runs SET nudge = 'verdict' WHERE id = $1").bind(nudge).execute(&f.pool).await?;
        let retry = f.store.recover_overflow(nudge, true).await?.expect("a review recovers");
        let (kind, _, prompt, _, _) = f.retry_of(retry).await?;
        assert_eq!(kind, "review");
        assert!(prompt.contains("Review the diff of quest blue widget.") && prompt.contains(REVIEW_RANGES_NOTE) && prompt.contains("[verdict missing]"), "{prompt}");

        let asked_by = json!({ "kind": "user", "user_id": f.owner, "quick": true });
        let quick = f.session("quick", "claude", None, 190_000, Some(asked_by)).await?;
        f.run(quick, None, "quick", "completed", "What did we ship today?", Some("The blue widget.")).await?;
        let failed_quick = f.run(quick, None, "quick", "failed", "And tomorrow?", None).await?;
        let retry = f.store.recover_overflow(failed_quick, true).await?.expect("a quick chat recovers");
        let (kind, _, prompt, _, _) = f.retry_of(retry).await?;
        assert_eq!(kind, "quick");
        assert!(prompt.contains("What did we ship today?") && prompt.contains("The blue widget.") && prompt.ends_with("Latest message:\nAnd tomorrow?"));
        let switched: Option<bool> = sqlx::query_scalar("SELECT (asked_by->>'switched')::boolean FROM agent_sessions WHERE id = $1").bind(quick).fetch_one(&f.pool).await?;
        assert_eq!(switched, Some(true), "a recovered quick chat never forks the main session again");
        let lost = f.store.quick_runs_without_reply(Some(quick), 20).await?;
        assert!(lost.iter().all(|run| run.id != failed_quick), "a recovered quick run gets no failure reply");

        let direct = f.session("direct", "claude", None, 190_000, None).await?;
        let failed_direct = f.run(direct, None, "direct", "failed", "Keep going.", None).await?;
        assert_eq!(f.retry_of(f.store.recover_overflow(failed_direct, true).await?.expect("a folder session recovers")).await?.0, "direct");

        let aside = f.session("aside", "claude", None, 0, None).await?;
        let failed_aside = f.run(aside, None, "aside", "failed", "How far along is the widget?", None).await?;
        sqlx::query("UPDATE session_runs SET fork_from = $2 WHERE id = $1").bind(failed_aside).bind(f.main).execute(&f.pool).await?;
        let (kind, fork_from, prompt, _, _) = f.retry_of(f.store.recover_overflow(failed_aside, true).await?.expect("an aside recovers")).await?;
        assert_eq!((kind.as_str(), fork_from), ("aside", None), "the recovered aside no longer forks the main session");
        assert!(prompt.ends_with("How far along is the widget?"));

        let closed = f.quest("cancelled").await?;
        let closed_session = f.session("task", "claude", Some(closed), 190_000, None).await?;
        let closed_run = f.run(closed_session, Some(closed), "followup", "failed", "x", None).await?;
        assert_eq!(f.store.recover_overflow(closed_run, true).await?, None, "a cancelled quest is never recovered");

        let huge = f.run(direct, None, "direct", "failed", &"y".repeat(800_000), None).await?;
        sqlx::query("UPDATE session_runs SET created_at = NOW() + INTERVAL '1 minute' WHERE id = $1").bind(huge).execute(&f.pool).await?;
        assert_eq!(f.store.recover_overflow(huge, true).await?, None, "no retry when the fresh request would not fit either");
        assert_eq!(f.store.recover_overflow(failed_direct, false).await?, None, "only context overflows recover");
        f.cleanup().await
    }

    #[tokio::test]
    async fn a_main_session_that_overflows_compacts_first_and_resets_when_that_did_not_help() -> anyhow::Result<()> {
        use crate::context::COMPACT_INBOX_KIND;
        use crate::world::RESET_INBOX_KIND;
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = crate::world::tests::steer_db().read().await;
        let f = Fixture::new(&url, "claude", "sonnet").await?;
        let failed = f.run(f.main, None, "turn", "failed", "hello", None).await?;
        assert_eq!(f.store.recover_main_overflow(f.agent, "API Error: 529 Overloaded", false).await?, None);
        assert_eq!(f.store.recover_main_overflow(f.agent, "Prompt is too long", false).await?, Some(AutoAction::Compact));
        assert_eq!(f.path_of(failed).await?.0.as_deref(), Some("recovery"));
        let pending = |kind: &'static str| {
            let pool = f.pool.clone();
            let agent = f.agent;
            async move {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = $2 AND status = 'pending'")
                    .bind(agent)
                    .bind(kind)
                    .fetch_one(&pool)
                    .await
            }
        };
        assert_eq!(pending(COMPACT_INBOX_KIND).await?, 1);
        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(f.agent).execute(&f.pool).await?;
        f.run(f.main, None, "turn", "completed", COMPACT_PROMPT, Some("compacted")).await?;
        assert_eq!(
            f.store.recover_main_overflow(f.agent, "Prompt is too long", false).await?,
            Some(AutoAction::Reset),
            "a compaction that did not make room resets with the notes instead"
        );
        assert_eq!(pending(RESET_INBOX_KIND).await?, 1);
        f.cleanup().await
    }

    #[tokio::test]
    async fn a_report_reminder_still_counts_after_a_checkpoint_rewrote_its_prompt() -> anyhow::Result<()> {
        use crate::run_causes::Required;
        use crate::world::{Nudge, REPORT_NUDGE, REPORT_NUDGE_LAST, REPORT_NUDGE_MARK};
        let Ok(url) = std::env::var("STEER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _db = crate::world::tests::steer_db().read().await;
        let f = Fixture::new(&url, "cursor", "composer-2.5").await?;
        let quest = f.quest("in_progress").await?;
        let session = f.session("task", "cursor", Some(quest), 190_000, None).await?;
        let first = f.run(session, Some(quest), "task", "completed", "[task session] Build the blue widget.", Some("I changed the files.")).await?;
        let Nudge::Queued(reminder) = f.store.nudge_missing(first, session, Required::Report).await? else {
            panic!("a run without a report gets a reminder");
        };
        assert_eq!(f.store.run_nudge(reminder).await?, Some(Required::Report));
        sqlx::query("UPDATE session_runs SET status = 'running' WHERE id = $1").bind(reminder).execute(&f.pool).await?;
        assert!(matches!(f.store.prepare_request(reminder).await?, Prepared::Checkpoint { .. }));
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1").bind(reminder).fetch_one(&f.pool).await?;
        assert!(!prompt.starts_with(REPORT_NUDGE_MARK) && prompt.contains(REPORT_NUDGE), "the reminder text survives inside the checkpoint");
        assert_eq!(f.store.run_nudge(reminder).await?, Some(Required::Report), "the rewrite never loses what the run is");
        sqlx::query("UPDATE session_runs SET status = 'completed', output = 'Still working.' WHERE id = $1").bind(reminder).execute(&f.pool).await?;
        sqlx::query("UPDATE agent_sessions SET cli_session_id = 'fresh' WHERE id = $1").bind(session).execute(&f.pool).await?;
        let Nudge::Queued(last) = f.store.nudge_missing(reminder, session, Required::Report).await? else {
            panic!("the second reminder is still owed");
        };
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1").bind(last).fetch_one(&f.pool).await?;
        assert_eq!(prompt, REPORT_NUDGE_LAST, "the checkpointed reminder counted as the first one");
        sqlx::query("UPDATE session_runs SET status = 'completed', output = 'Still working.' WHERE id = $1").bind(last).execute(&f.pool).await?;
        assert_eq!(f.store.nudge_missing(last, session, Required::Report).await?, Nudge::Exhausted);
        f.cleanup().await
    }
}
