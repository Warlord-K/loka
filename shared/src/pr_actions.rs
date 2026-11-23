use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{AgentSchedule, NewEvent, WorldStore};

pub const KIND: &str = "action";
pub const RUN_TIMEOUT_SECS: i32 = 300;
pub const WAIT_DAYS_DEFAULT: i64 = 7;
pub const WAIT_DAYS_MAX: i64 = 30;
pub const EVENTS: &[&str] = &["task.finished", "task.merged", "task.pr_closed"];
pub const ACTION_NAMES: &[&str] = &["merge", "delete_branch", "close", "rerun_checks"];

pub const MERGE_RULE: &str = "Loka merges only when the independent review approved this exact head and the quest's local checks passed";
pub const NO_GRANT: &str = "No enabled allow rule in Settings > Rules lets agents act on pull requests in";
pub const SHARED_DEVICE: &str = "Shared devices do not act on GitHub for other people's agents. Move the agent to your own device.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Merge,
    DeleteBranch,
    Close,
    RerunChecks,
}

impl ActionKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "merge" => Some(Self::Merge),
            "delete_branch" => Some(Self::DeleteBranch),
            "close" => Some(Self::Close),
            "rerun_checks" | "rerun_check" | "rerun" => Some(Self::RerunChecks),
            _ => None,
        }
    }

    fn todo(self) -> &'static str {
        match self {
            Self::Merge => "Squash-merge",
            Self::DeleteBranch => "Delete the branch of",
            Self::Close => "Close",
            Self::RerunChecks => "Rerun failed checks on",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Self::Merge => "squash-merged",
            Self::DeleteBranch => "deleted the branch of",
            Self::Close => "closed",
            Self::RerunChecks => "reran failed checks on",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSpec {
    pub kind: ActionKind,
    pub repo: String,
    pub number: i64,
    pub sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}

impl ActionSpec {
    pub fn pr(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    pub fn title(&self) -> String {
        format!("{} {} at {}", self.kind.todo(), self.pr(), short(&self.sha))
    }

    pub fn done_line(&self) -> String {
        format!("{} {} at {}", capitalize(self.kind.done()), self.pr(), short(&self.sha))
    }
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn full_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    Approved,
    Changes,
    NoVerdict,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "failing")]
pub enum Checks {
    Passed,
    Failed(Vec<String>),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub granted: bool,
    pub quest_repo: Option<String>,
    pub quest_pr: Option<i64>,
    pub quest_branch: Option<String>,
    pub quest_cancelled: bool,
    pub published_head: Option<String>,
    pub review: Review,
    pub checks: Checks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePr {
    pub state: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub default_branch: String,
    pub cross_repo: bool,
    pub branch_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionJob {
    pub schedule_id: Uuid,
    pub spec: ActionSpec,
    pub facts: Facts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Act,
    AlreadyDone(String),
    Skip(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Ran,
    Skipped,
    Failed,
}

impl OutcomeStatus {
    pub fn event(self) -> &'static str {
        match self {
            Self::Ran => "action.ran",
            Self::Skipped => "action.skipped",
            Self::Failed => "action.failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub status: OutcomeStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_sha: Option<String>,
}

impl Outcome {
    pub fn ran(reason: impl Into<String>) -> Self {
        Self { status: OutcomeStatus::Ran, reason: reason.into(), merge_sha: None }
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        Self { status: OutcomeStatus::Skipped, reason: reason.into(), merge_sha: None }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self { status: OutcomeStatus::Failed, reason: reason.into(), merge_sha: None }
    }

    pub fn from_output(output: &str) -> Option<Self> {
        serde_json::from_str(output.trim()).ok()
    }
}

pub fn trigger_problem(on_event: Option<&str>, fired: Option<&Value>) -> Option<String> {
    let got = fired.and_then(|f| f.get("type")).and_then(Value::as_str);
    match (on_event, got) {
        (_, Some("task.cancelled")) => Some("the quest was cancelled".into()),
        (Some(_), None) => Some("it waited too long: the event it was waiting for never came".into()),
        (Some("task.finished"), Some("review.finished")) => None,
        (Some(wanted), Some(got)) if wanted != got => Some(format!("{} came first, not {}", event_words(got), event_words(wanted))),
        _ => None,
    }
}

pub fn event_words(event: &str) -> &'static str {
    match event {
        "task.finished" | "review.finished" => "the quest finishing its review",
        "task.merged" => "the pull request merging",
        "task.pr_closed" => "the pull request closing unmerged",
        _ => "another event",
    }
}

pub fn ownership_problem(spec: &ActionSpec, facts: &Facts) -> Option<String> {
    if !full_sha(&spec.sha) {
        return Some(format!("{} is not a full 40 character commit sha; pin the exact head", spec.sha));
    }
    if facts.quest_cancelled {
        return Some("the quest was cancelled".into());
    }
    if !facts.granted {
        return Some(format!("{NO_GRANT} {}", spec.repo));
    }
    let own = facts.quest_repo.as_deref().is_some_and(|repo| repo.eq_ignore_ascii_case(&spec.repo)) && facts.quest_pr == Some(spec.number);
    if !own {
        return Some(format!("{} is not this quest's own pull request; agents act only on the pull request Loka opened for the quest", spec.pr()));
    }
    None
}

pub fn loka_problem(job: &ActionJob) -> Option<String> {
    job.trigger_problem.clone().or_else(|| ownership_problem(&job.spec, &job.facts))
}

const PROTECTED_BRANCHES: &[&str] = &["main", "master", "trunk", "develop"];

pub fn decide(spec: &ActionSpec, facts: &Facts, live: &LivePr) -> Decision {
    if let Some(problem) = ownership_problem(spec, facts) {
        return Decision::Skip(problem);
    }
    if live.cross_repo {
        return Decision::Skip(format!("{} comes from a fork; agents act only on branches inside {}", spec.pr(), spec.repo));
    }
    let moved = (live.head_sha != spec.sha).then(|| format!("the head moved from {} to {}", short(&spec.sha), short(&live.head_sha)));
    match spec.kind {
        ActionKind::Merge => {
            match live.state.as_str() {
                "MERGED" => return Decision::Skip(format!("{} is already merged", spec.pr())),
                "CLOSED" => return Decision::Skip(format!("{} was closed", spec.pr())),
                _ => {}
            }
            if let Some(moved) = moved {
                return Decision::Skip(moved);
            }
            merge_record_problem(spec, facts).map(Decision::Skip).unwrap_or(Decision::Act)
        }
        ActionKind::Close | ActionKind::RerunChecks => {
            if live.state != "OPEN" {
                return Decision::Skip(format!("{} is {}", spec.pr(), live.state.to_ascii_lowercase()));
            }
            moved.map(Decision::Skip).unwrap_or(Decision::Act)
        }
        ActionKind::DeleteBranch => {
            let branch = live.head_branch.as_str();
            if facts.quest_branch.as_deref() != Some(branch) {
                return Decision::Skip(format!("{branch} is not this quest's own branch"));
            }
            if branch == live.base_branch || branch == live.default_branch || PROTECTED_BRANCHES.contains(&branch) {
                return Decision::Skip(format!("{branch} is a base branch and is never deleted"));
            }
            if live.state == "OPEN" {
                return Decision::Skip(format!("{} is still open; deleting its branch would close it", spec.pr()));
            }
            match live.branch_sha.as_deref() {
                None => Decision::AlreadyDone(format!("{branch} was already deleted")),
                Some(tip) if tip != spec.sha => Decision::Skip(format!("{branch} moved from {} to {}", short(&spec.sha), short(tip))),
                Some(_) => Decision::Act,
            }
        }
    }
}

pub fn merge_record_problem(spec: &ActionSpec, facts: &Facts) -> Option<String> {
    match facts.published_head.as_deref() {
        None => return Some(format!("Loka has no record of publishing a reviewed head since the quest's last change. {MERGE_RULE}")),
        Some(published) if published != spec.sha => return Some(format!("the review approved {}, not {}. {MERGE_RULE}", short(published), short(&spec.sha))),
        Some(_) => {}
    }
    match facts.review {
        Review::Approved => {}
        Review::Changes => return Some(format!("the review on record asks for changes. {MERGE_RULE}")),
        Review::NoVerdict => return Some(format!("the review ended without a verdict. {MERGE_RULE}")),
        Review::Missing => return Some(format!("no independent review is on record for this head. {MERGE_RULE}")),
    }
    match &facts.checks {
        Checks::Passed => None,
        Checks::Failed(names) => Some(format!("local checks failed ({}). {MERGE_RULE}", names.join(", "))),
        Checks::Missing => Some(format!("the quest's last report lists no passing local checks. {MERGE_RULE}")),
    }
}

pub const MAX_RULE_REPOS: usize = 20;

pub fn clean_repos(repos: &[String]) -> Result<Vec<String>> {
    let mut clean: Vec<String> = Vec::new();
    for repo in repos.iter().map(|r| r.trim().trim_start_matches("https://github.com/").trim_end_matches('/')).filter(|r| !r.is_empty()) {
        let valid = repo.split_once('/').is_some_and(|(owner, name)| {
            let part = |p: &str| !p.is_empty() && p.len() <= 100 && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && p != "." && p != "..";
            part(owner) && part(name) && !name.contains('/')
        });
        if !valid {
            bail!("{repo} is not a repository name like owner/repo.");
        }
        if !clean.iter().any(|known| known.eq_ignore_ascii_case(repo)) {
            clean.push(repo.to_string());
        }
    }
    if clean.len() > MAX_RULE_REPOS {
        bail!("An approved action can name at most {MAX_RULE_REPOS} repositories.");
    }
    Ok(clean)
}

pub fn checks_on_record(checks: &[Value]) -> Checks {
    let results: Vec<(String, String)> = checks
        .iter()
        .map(|check| match check {
            Value::Object(fields) => (
                fields.get("check").and_then(Value::as_str).unwrap_or("a check").to_string(),
                fields.get("result").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase(),
            ),
            other => (other.as_str().unwrap_or("a check").to_string(), String::new()),
        })
        .collect();
    let failing: Vec<String> = results.iter().filter(|(_, result)| result.starts_with("fail")).map(|(name, _)| name.clone()).collect();
    if !failing.is_empty() {
        return Checks::Failed(failing);
    }
    if results.iter().any(|(_, result)| result.starts_with("pass")) {
        Checks::Passed
    } else {
        Checks::Missing
    }
}

pub fn review_from_verdict(verdict: Option<&str>) -> Review {
    match verdict.map(str::trim) {
        Some("approve") | Some("approved") => Review::Approved,
        Some("changes") | Some("request_changes") => Review::Changes,
        Some(_) => Review::NoVerdict,
        None => Review::NoVerdict,
    }
}

#[derive(Debug, Clone)]
pub struct QuestRecord {
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub branch: Option<String>,
    pub cancelled: bool,
    pub published_head: Option<String>,
    pub review_output: Option<Option<String>>,
    pub report_output: Option<String>,
}

pub struct NewAction<'a> {
    pub user_id: Uuid,
    pub agent_id: Uuid,
    pub task_id: Uuid,
    pub spec: &'a ActionSpec,
    pub run_at: Option<DateTime<Utc>>,
    pub on_event: Option<&'a str>,
    pub wait_days: Option<i64>,
    pub note: Option<&'a str>,
}

impl WorldStore {
    pub async fn grants_repo(&self, owner: Option<Uuid>, org: Option<Uuid>, repo: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM world_rules
               WHERE enabled AND scope = 'allow'
                 AND ((user_id IS NULL AND org_id = $2) OR (user_id = $1 AND (org_id IS NULL OR org_id = $2)))
                 AND EXISTS (SELECT 1 FROM unnest(github_repos) granted WHERE lower(granted) = lower($3)))",
        )
        .bind(owner)
        .bind(org)
        .bind(repo)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn quest_record(&self, task_id: Uuid) -> Result<Option<QuestRecord>> {
        let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<i32>, Option<String>, String, Option<String>, Option<DateTime<Utc>>)>(
            "SELECT repo, pr_url, pr_number, branch, status, published_head, published_head_at FROM agent_tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?;
        let Some((repo, pr_url, pr_number, branch, status, published_head, published_at)) = row else {
            return Ok(None);
        };
        let tracked = pr_url
            .as_deref()
            .and_then(crate::git_names::TrackedPr::parse);
        let repo = tracked.map(|tracked| tracked.repo().to_string()).or(repo);
        let pr_number = tracked
            .map(|tracked| i64::from(tracked.number()))
            .or_else(|| pr_number.map(i64::from));
        let last_change = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT MAX(created_at) FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup') AND NOT unchanged_work",
        )
        .bind(task_id)
        .fetch_one(self.pool())
        .await?;
        let review_output = sqlx::query_scalar::<_, Option<String>>(
            "SELECT r.output FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id
             WHERE r.task_id = $1 AND r.kind = 'review' AND s.role = 'review'
               AND (r.status = 'completed' OR r.cause = 'no_verdict')
               AND s.asked_by->'compare' IS NULL
               AND ($2::timestamptz IS NULL OR r.created_at > $2)
             ORDER BY r.created_at DESC, r.id DESC LIMIT 1",
        )
        .bind(task_id)
        .bind(last_change)
        .fetch_optional(self.pool())
        .await?;
        let report_output = sqlx::query_scalar::<_, Option<String>>(
            "SELECT output FROM session_runs
             WHERE task_id = $1 AND kind IN ('task', 'followup') AND NOT unchanged_work AND status = 'completed'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        Ok(Some(QuestRecord {
            repo,
            pr_number,
            branch,
            cancelled: status == "cancelled",
            published_head: published_head.filter(|_| published_at.is_some_and(|at| last_change.is_none_or(|change| at > change))),
            review_output,
            report_output,
        }))
    }

    pub async fn set_rule_repos(&self, id: Uuid, user_id: Uuid, global_org: Option<Uuid>, repos: &[String]) -> Result<Option<crate::world::WorldRule>> {
        let repos = clean_repos(repos)?;
        Ok(sqlx::query_as::<_, crate::world::WorldRule>(
            "UPDATE world_rules SET github_repos = $4, updated_at = NOW()
             WHERE id = $1 AND scope = 'allow' AND (user_id = $2 OR (user_id IS NULL AND org_id = $3))
             RETURNING *",
        )
        .bind(id)
        .bind(user_id)
        .bind(global_org)
        .bind(repos)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn quest_for_pr(&self, org: Option<Uuid>, repo: &str, number: i64) -> Result<Option<Uuid>> {
        crate::git_names::validate_repo(repo)?;
        let number = i32::try_from(number).map_err(|_| anyhow!("pull request number is too large"))?;
        if number <= 0 {
            bail!("pull request number must be positive")
        }
        let url = format!("https://github.com/{repo}/pull/{number}");
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_tasks WHERE lower(pr_url) = lower($2) AND org_id IS NOT DISTINCT FROM $1
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(org)
        .bind(url)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn newest_task_session(&self, task_id: Uuid) -> Result<Option<crate::world::AgentSession>> {
        Ok(sqlx::query_as::<_, crate::world::AgentSession>(
            "SELECT * FROM agent_sessions WHERE task_id = $1 AND role = 'task' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn record_published_head(&self, task_id: Uuid, pr_url: &str, head: &str) -> Result<()> {
        if !full_sha(head) {
            return Ok(());
        }
        sqlx::query(
            "UPDATE agent_tasks SET published_head = $2, published_head_at = NOW()
             WHERE id = $1 AND lower(pr_url) = lower($3) AND status <> 'cancelled'",
        )
            .bind(task_id)
            .bind(head)
            .bind(pr_url)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn create_pr_action(&self, new: NewAction<'_>) -> Result<AgentSchedule> {
        if let Some(event) = new.on_event.filter(|e| !EVENTS.contains(e)) {
            bail!("on must be one of {}, not {event}", EVENTS.join(", "));
        }
        let waits = new.on_event.is_some();
        let days = new.wait_days.unwrap_or(WAIT_DAYS_DEFAULT).clamp(1, WAIT_DAYS_MAX);
        let fires_at = if waits { Utc::now() + Duration::days(days) } else { new.run_at.unwrap_or_else(Utc::now) };
        let action = serde_json::to_value(new.spec)?;
        let name = new.spec.title();
        let prompt = new.note.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&name).to_string();
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "INSERT INTO schedules (user_id, agent_id, name, prompt, run_at, next_run_at, kind, task_id, action, on_event)
             VALUES ($1, $2, $3, $4, $5, $5, 'action', $6, $7, $8)
             RETURNING id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at,
                       action, on_event, fired, outcome, run_id",
        )
        .bind(new.user_id)
        .bind(new.agent_id)
        .bind(name)
        .bind(prompt)
        .bind(fires_at)
        .bind(new.task_id)
        .bind(action)
        .bind(new.on_event)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn open_pr_actions(&self, task_id: Uuid) -> Result<Vec<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at,
                    action, on_event, fired, outcome, run_id
             FROM schedules WHERE task_id = $1 AND kind = 'action' AND outcome IS NULL ORDER BY created_at",
        )
        .bind(task_id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn queue_pr_action(&self, schedule: &AgentSchedule) -> Result<Option<Uuid>> {
        let mut tx = self.pool().begin().await?;
        let claimed = sqlx::query_as::<_, (Uuid, Option<Uuid>, Value)>(
            "SELECT s.agent_id, s.task_id, s.action FROM schedules s
             WHERE s.id = $1 AND s.kind = 'action' AND s.run_id IS NULL AND s.outcome IS NULL
             FOR UPDATE",
        )
        .bind(schedule.id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((agent_id, task_id, action)) = claimed else {
            tx.rollback().await?;
            return Ok(None);
        };
        let (backend, model) = sqlx::query_as::<_, (String, String)>("SELECT cli, model FROM agent_profiles WHERE id = $1 FOR SHARE")
            .bind(agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let session_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, title, working_dir, usage_baseline)
             VALUES ($1, 'action', $2, $3, $4, $5, 'agents/' || $1::text, '{}'::jsonb) RETURNING id",
        )
        .bind(agent_id)
        .bind(&backend)
        .bind(&model)
        .bind(task_id)
        .bind(&schedule.name)
        .fetch_one(&mut *tx)
        .await?;
        let prompt = json!({ "schedule_id": schedule.id, "action": action }).to_string();
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, timeout_secs, request)
             VALUES ($1, $2, $3, 'action', 'queued', $4, $5, $6) RETURNING id",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(task_id)
        .bind(prompt)
        .bind(RUN_TIMEOUT_SECS)
        .bind(json!({ "text": schedule.name }))
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE schedules SET run_id = $2 WHERE id = $1")
            .bind(schedule.id)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(run_id))
    }

    pub async fn pr_action_of_run(&self, run_id: Uuid) -> Result<Option<AgentSchedule>> {
        Ok(sqlx::query_as::<_, AgentSchedule>(
            "SELECT id, agent_id, name, prompt, cron_expression, run_at, next_run_at, enabled, kind, task_id, expires_at,
                    action, on_event, fired, outcome, run_id
             FROM schedules WHERE run_id = $1 AND kind = 'action'",
        )
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn settle_pr_action(&self, run_id: Uuid, outcome: &Outcome) -> Result<Option<ActionSpec>> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, Value, Option<String>)>(
            "UPDATE schedules SET outcome = $2, enabled = FALSE, next_run_at = NULL
             WHERE run_id = $1 AND kind = 'action' AND outcome IS NULL
             RETURNING id, agent_id, task_id, action, on_event",
        )
        .bind(run_id)
        .bind(json!({ "status": outcome.status, "reason": outcome.reason, "merge_sha": outcome.merge_sha, "at": Utc::now() }))
        .fetch_optional(&mut *tx)
        .await?;
        let Some((schedule_id, agent_id, task_id, action, on_event)) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let spec: ActionSpec = serde_json::from_value(action).map_err(|e| anyhow!("stored action is unreadable: {e}"))?;
        sqlx::query("UPDATE agent_sessions SET status = 'archived', summary = $2, updated_at = NOW() WHERE id = (SELECT session_id FROM session_runs WHERE id = $1)")
            .bind(run_id)
            .bind(&outcome.reason)
            .execute(&mut *tx)
            .await?;
        let line = outcome_line(&spec, outcome);
        let event = NewEvent::new(outcome.status.event())
            .agent(agent_id)
            .run(run_id)
            .task(task_id)
            .payload(json!({
                "schedule_id": schedule_id,
                "action": spec.kind,
                "repo": spec.repo,
                "number": spec.number,
                "sha": spec.sha,
                "on": on_event,
                "status": outcome.status,
                "reason": outcome.reason,
                "merge_sha": outcome.merge_sha,
                "line": line,
            }));
        crate::world::emit_in(&mut tx, &event, Some((agent_id, outcome.status.event()))).await?;
        tx.commit().await?;
        Ok(Some(spec))
    }
}

pub fn merged_by(spec: &ActionSpec, outcome: &Outcome) -> bool {
    spec.kind == ActionKind::Merge && outcome.status == OutcomeStatus::Ran
}

pub fn merged_pr_url<'a>(
    spec: &ActionSpec,
    outcome: &Outcome,
    task_pr_url: Option<&'a str>,
    _task_pr_number: Option<i32>,
) -> Option<&'a str> {
    let url = task_pr_url?;
    (merged_by(spec, outcome)
        && crate::runners::pr_url_matches(url, &spec.repo, spec.number))
    .then_some(url)
}

pub fn outcome_line(spec: &ActionSpec, outcome: &Outcome) -> String {
    match outcome.status {
        OutcomeStatus::Ran => match outcome.merge_sha.as_deref() {
            Some(merged) => format!("{}, merge commit {}", spec.done_line(), short(merged)),
            None if outcome.reason.is_empty() => spec.done_line(),
            None => format!("{}: {}", spec.done_line(), outcome.reason),
        },
        OutcomeStatus::Skipped => format!("Skipped: {}, because {}", lower_first(&spec.title()), outcome.reason),
        OutcomeStatus::Failed => format!("Could not {}: {}", lower_first(&spec.title()), outcome.reason),
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

pub fn schedule_state(schedule: &AgentSchedule) -> String {
    if let Some(outcome) = schedule.outcome.as_ref() {
        let status = outcome.get("status").and_then(Value::as_str).unwrap_or("");
        let reason = outcome.get("reason").and_then(Value::as_str).unwrap_or("");
        let merged = outcome.get("merge_sha").and_then(Value::as_str).map(|m| format!(", merge commit {}", short(m))).unwrap_or_default();
        return match status {
            "ran" => format!("ran{merged}{}", if reason.is_empty() { String::new() } else { format!(": {reason}") }),
            "skipped" => format!("skipped: {reason}"),
            _ => format!("failed: {reason}"),
        };
    }
    if schedule.run_id.is_some() || schedule.fired.is_some() {
        return "running now".into();
    }
    if !schedule.enabled {
        return "paused".into();
    }
    match schedule.on_event.as_deref() {
        Some(event) => format!(
            "waits for {} (gives up {})",
            event_words(event),
            schedule.next_run_at.or(schedule.run_at).map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| "later".into())
        ),
        None => match schedule.next_run_at.or(schedule.run_at) {
            Some(at) if at > Utc::now() => format!("runs at {}", at.format("%Y-%m-%d %H:%M UTC")),
            _ => "runs now".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn spec(kind: ActionKind) -> ActionSpec {
        ActionSpec { kind, repo: "acme/castle".into(), number: 264, sha: HEAD.into(), check: None }
    }

    fn facts() -> Facts {
        Facts {
            granted: true,
            quest_repo: Some("acme/castle".into()),
            quest_pr: Some(264),
            quest_branch: Some("quest-branch".into()),
            quest_cancelled: false,
            published_head: Some(HEAD.into()),
            review: Review::Approved,
            checks: Checks::Passed,
        }
    }

    fn live(state: &str) -> LivePr {
        LivePr {
            state: state.into(),
            head_sha: HEAD.into(),
            head_branch: "quest-branch".into(),
            base_branch: "main".into(),
            default_branch: "main".into(),
            cross_repo: false,
            branch_sha: Some(HEAD.into()),
        }
    }

    fn skipped(decision: Decision, words: &str) {
        match decision {
            Decision::Skip(reason) => assert!(reason.contains(words), "{reason} should say {words}"),
            other => panic!("expected a skip saying {words}, got {other:?}"),
        }
    }

    #[test]
    fn a_reviewed_checked_head_in_a_granted_repo_merges() {
        assert_eq!(decide(&spec(ActionKind::Merge), &facts(), &live("OPEN")), Decision::Act);
    }

    #[test]
    fn a_moved_head_skips_every_action_on_an_open_pr() {
        let mut moved = live("OPEN");
        moved.head_sha = OTHER.into();
        for kind in [ActionKind::Merge, ActionKind::Close, ActionKind::RerunChecks] {
            skipped(decide(&spec(kind), &facts(), &moved), "head moved from 11111111 to 22222222");
        }
    }

    #[test]
    fn merging_needs_an_approved_review_of_this_exact_head() {
        for (review, words) in [(Review::Changes, "asks for changes"), (Review::NoVerdict, "without a verdict"), (Review::Missing, "no independent review")] {
            skipped(decide(&spec(ActionKind::Merge), &Facts { review, ..facts() }, &live("OPEN")), words);
        }
        skipped(decide(&spec(ActionKind::Merge), &Facts { published_head: Some(OTHER.into()), ..facts() }, &live("OPEN")), "approved 22222222, not 11111111");
        skipped(decide(&spec(ActionKind::Merge), &Facts { published_head: None, ..facts() }, &live("OPEN")), "no record of publishing");
    }

    #[test]
    fn merging_needs_passing_local_checks() {
        skipped(decide(&spec(ActionKind::Merge), &Facts { checks: Checks::Failed(vec!["clippy".into()]), ..facts() }, &live("OPEN")), "local checks failed (clippy)");
        skipped(decide(&spec(ActionKind::Merge), &Facts { checks: Checks::Missing, ..facts() }, &live("OPEN")), "no passing local checks");
    }

    #[test]
    fn a_merged_or_closed_pr_is_never_merged_closed_or_rerun() {
        skipped(decide(&spec(ActionKind::Merge), &facts(), &live("MERGED")), "already merged");
        skipped(decide(&spec(ActionKind::Merge), &facts(), &live("CLOSED")), "was closed");
        skipped(decide(&spec(ActionKind::Close), &facts(), &live("CLOSED")), "is closed");
        skipped(decide(&spec(ActionKind::RerunChecks), &facts(), &live("MERGED")), "is merged");
        assert_eq!(decide(&spec(ActionKind::Close), &facts(), &live("OPEN")), Decision::Act);
        assert_eq!(decide(&spec(ActionKind::RerunChecks), &facts(), &live("OPEN")), Decision::Act);
    }

    #[test]
    fn only_the_quests_own_pr_in_a_granted_repo_is_touched() {
        skipped(decide(&spec(ActionKind::Merge), &Facts { granted: false, ..facts() }, &live("OPEN")), NO_GRANT);
        let mut elsewhere = spec(ActionKind::Merge);
        elsewhere.repo = "Warlord-K/loka".into();
        skipped(decide(&elsewhere, &facts(), &live("OPEN")), "not this quest's own pull request");
        let mut other_pr = spec(ActionKind::Merge);
        other_pr.number = 1;
        skipped(decide(&other_pr, &facts(), &live("OPEN")), "not this quest's own pull request");
        let mut fork = live("OPEN");
        fork.cross_repo = true;
        skipped(decide(&spec(ActionKind::Merge), &facts(), &fork), "from a fork");
        skipped(decide(&spec(ActionKind::Close), &Facts { quest_cancelled: true, ..facts() }, &live("OPEN")), "cancelled");
        let mut short_sha = spec(ActionKind::Merge);
        short_sha.sha = "1111111".into();
        skipped(decide(&short_sha, &facts(), &live("OPEN")), "full 40 character");
    }

    #[test]
    fn only_the_quests_own_branch_of_a_finished_pr_is_deleted() {
        assert_eq!(decide(&spec(ActionKind::DeleteBranch), &facts(), &live("MERGED")), Decision::Act);
        assert_eq!(decide(&spec(ActionKind::DeleteBranch), &facts(), &live("CLOSED")), Decision::Act);
        skipped(decide(&spec(ActionKind::DeleteBranch), &facts(), &live("OPEN")), "still open");
        let mut someone_else = live("MERGED");
        someone_else.head_branch = "feature-of-a-person".into();
        skipped(decide(&spec(ActionKind::DeleteBranch), &facts(), &someone_else), "not this quest's own branch");
        let mut main = live("MERGED");
        main.head_branch = "main".into();
        skipped(decide(&spec(ActionKind::DeleteBranch), &Facts { quest_branch: Some("main".into()), ..facts() }, &main), "never deleted");
        let mut gone = live("MERGED");
        gone.branch_sha = None;
        assert!(matches!(decide(&spec(ActionKind::DeleteBranch), &facts(), &gone), Decision::AlreadyDone(_)));
        let mut pushed = live("MERGED");
        pushed.branch_sha = Some(OTHER.into());
        skipped(decide(&spec(ActionKind::DeleteBranch), &facts(), &pushed), "moved from");
    }

    #[test]
    fn a_trigger_that_did_not_come_as_asked_is_a_skip_reason() {
        assert_eq!(trigger_problem(None, None), None);
        assert_eq!(trigger_problem(Some("task.finished"), Some(&json!({ "type": "task.finished" }))), None);
        assert!(trigger_problem(Some("task.finished"), Some(&json!({ "type": "task.merged" }))).unwrap().contains("pull request merging came first"));
        assert!(trigger_problem(Some("task.merged"), None).unwrap().contains("never came"));
        assert!(trigger_problem(None, Some(&json!({ "type": "task.cancelled" }))).unwrap().contains("cancelled"));
    }

    #[test]
    fn checks_pass_only_with_a_pass_and_no_failure() {
        assert_eq!(checks_on_record(&[json!({ "check": "clippy", "result": "pass" }), json!({ "check": "swift", "result": "not run, no Xcode" })]), Checks::Passed);
        assert_eq!(checks_on_record(&[json!({ "check": "clippy", "result": "pass" }), json!({ "check": "tests", "result": "fail" })]), Checks::Failed(vec!["tests".into()]));
        assert_eq!(checks_on_record(&[json!({ "check": "tests", "result": "not run" })]), Checks::Missing);
        assert_eq!(checks_on_record(&[]), Checks::Missing);
    }

    #[test]
    fn outcome_lines_say_what_happened_and_why() {
        let merged = Outcome { merge_sha: Some(OTHER.into()), ..Outcome::ran("") };
        assert_eq!(outcome_line(&spec(ActionKind::Merge), &merged), "Squash-merged acme/castle#264 at 11111111, merge commit 22222222");
        assert_eq!(
            outcome_line(&spec(ActionKind::Merge), &Outcome::skipped("the head moved")),
            "Skipped: squash-merge acme/castle#264 at 11111111, because the head moved"
        );
        assert!(ActionKind::parse("Delete branch") == Some(ActionKind::DeleteBranch) && ActionKind::parse("rerun") == Some(ActionKind::RerunChecks));
        for name in ACTION_NAMES {
            let kind = ActionKind::parse(name).expect("every listed name parses");
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
        }
    }

    struct Quest {
        pool: sqlx::PgPool,
        store: WorldStore,
        user: Uuid,
        agent: Uuid,
        org: Option<Uuid>,
        task: Uuid,
        session: Uuid,
    }

    async fn quest(url: &str) -> anyhow::Result<Quest> {
        quest_in(sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(url).await?).await
    }

    async fn quest_in(pool: sqlx::PgPool) -> anyhow::Result<Quest> {
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("pr-{tag}@test.dev"))
            .fetch_one(&pool)
            .await?;
        let (agent, org): (Uuid, Option<Uuid>) = sqlx::query_as(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Ferris', 'claude', 'haiku', $2) RETURNING id, org_id",
        )
        .bind(format!("ferris-{tag}"))
        .bind(user)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, status, repo, pr_number, pr_url, branch, assigned_agent_id)
             VALUES ($1, 'Fix the bell', 'review', 'Acme/Shop', 7, 'https://github.com/Acme/Shop/pull/7', $2, NULL) RETURNING id",
        )
        .bind(user)
        .bind(format!("quest-{tag}"))
        .fetch_one(&pool)
        .await?;
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'claude', 'haiku', $2) RETURNING id",
        )
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        Ok(Quest { pool, store, user, agent, org, task, session })
    }

    impl Quest {
        async fn run(&self, session: Uuid, kind: &str, output: &str) -> anyhow::Result<Uuid> {
            Ok(sqlx::query_scalar(
                "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, timeout_secs, finished_at)
                 VALUES ($1, $2, $3, $4, 'completed', 'p', $5, 60, NOW()) RETURNING id",
            )
            .bind(session)
            .bind(self.agent)
            .bind(self.task)
            .bind(kind)
            .bind(output)
            .fetch_one(&self.pool)
            .await?)
        }

        async fn review(&self, verdict: &str) -> anyhow::Result<()> {
            let reviewer: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, parent_session_id) VALUES ($1, 'review', 'claude', 'haiku', $2, $3) RETURNING id",
            )
            .bind(self.agent)
            .bind(self.task)
            .bind(self.session)
            .fetch_one(&self.pool)
            .await?;
            self.run(reviewer, "review", &format!("Looks fine.\n<!-- verdict {{\"verdict\": \"{verdict}\", \"findings\": []}} verdict -->")).await?;
            Ok(())
        }

        async fn action(&self, kind: ActionKind, on_event: Option<&str>) -> anyhow::Result<AgentSchedule> {
            let spec = ActionSpec { kind, repo: "Acme/Shop".into(), number: 7, sha: "1111111111111111111111111111111111111111".into(), check: None };
            self.store
                .create_pr_action(NewAction { user_id: self.user, agent_id: self.agent, task_id: self.task, spec: &spec, run_at: None, on_event, wait_days: None, note: None })
                .await
        }

        async fn row(&self, id: Uuid) -> anyhow::Result<AgentSchedule> {
            Ok(self.store.list_agent_schedules(self.agent).await?.into_iter().find(|s| s.id == id).expect("schedule is listed"))
        }

        async fn events(&self, kind: &str) -> anyhow::Result<i64> {
            Ok(sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = $2").bind(self.task).bind(kind).fetch_one(&self.pool).await?)
        }
    }

    #[tokio::test]
    async fn only_an_enabled_allow_rule_naming_the_repo_grants_actions() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        assert!(!q.store.grants_repo(Some(q.user), q.org, "Acme/Shop").await?);
        let talk = q.store.create_rule(Some(q.user), q.org, "talk", "Be brief").await?;
        assert!(q.store.set_rule_repos(talk.id, q.user, None, &["Acme/Shop".into()]).await?.is_none());
        let allow = q.store.create_rule(Some(q.user), q.org, "allow", "Merge reviewed PRs in Acme/Shop pinned to the head").await?;
        let saved = q.store.set_rule_repos(allow.id, q.user, None, &[" https://github.com/Acme/Shop ".into(), "acme/shop".into()]).await?.unwrap();
        assert_eq!(saved.github_repos, vec!["Acme/Shop".to_string()]);
        assert!(q.store.grants_repo(Some(q.user), q.org, "acme/shop").await?);
        assert!(!q.store.grants_repo(Some(q.user), q.org, "Acme/Other").await?);
        assert!(q.store.set_rule_repos(allow.id, q.user, None, &["not a repo".into()]).await.is_err());
        q.store.update_rule(allow.id, q.user, None, None, None, Some(false)).await?;
        assert!(!q.store.grants_repo(Some(q.user), q.org, "Acme/Shop").await?);
        Ok(())
    }

    #[tokio::test]
    async fn the_record_holds_only_a_review_and_publish_newer_than_the_last_change() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let empty = q.store.quest_record(q.task).await?.unwrap();
        assert!(empty.review_output.is_none() && empty.published_head.is_none() && empty.report_output.is_none());
        q.run(q.session, "task", "done\n<!-- report {\"status\": \"done\", \"checks\": [{\"check\": \"tests\", \"result\": \"pass\"}]} report -->").await?;
        q.review("approve").await?;
        q.store.record_published_head(q.task, "https://github.com/acme/shop/pull/7", "1111111111111111111111111111111111111111").await?;
        q.store.record_published_head(q.task, "https://github.com/Acme/Shop/pull/7", "not-a-sha").await?;
        q.store.record_published_head(q.task, "https://github.com/Acme/Shop/pull/8", "2222222222222222222222222222222222222222").await?;
        let record = q.store.quest_record(q.task).await?.unwrap();
        assert!(record.review_output.flatten().unwrap().contains("approve"));
        assert_eq!(record.repo.as_deref(), Some("Acme/Shop"));
        assert_eq!(record.pr_number, Some(7));
        assert_eq!(record.published_head.as_deref(), Some("1111111111111111111111111111111111111111"));
        assert!(record.report_output.unwrap().contains("pass"));
        let unchanged = q.run(q.session, "followup", "no change").await?;
        sqlx::query("UPDATE session_runs SET unchanged_work = TRUE WHERE id = $1").bind(unchanged).execute(&q.pool).await?;
        assert!(q.store.quest_record(q.task).await?.unwrap().published_head.is_some());
        q.run(q.session, "followup", "changed the code").await?;
        let moved = q.store.quest_record(q.task).await?.unwrap();
        assert!(moved.review_output.is_none(), "a review older than the last change does not count");
        assert!(moved.published_head.is_none(), "a publish older than the last change does not count");
        sqlx::query("UPDATE agent_tasks SET repo = 'Acme/Other', pr_number = 21 WHERE id = $1")
            .bind(q.task)
            .execute(&q.pool)
            .await?;
        let edited = q.store.quest_record(q.task).await?.unwrap();
        assert_eq!((edited.repo.as_deref(), edited.pr_number), (Some("Acme/Shop"), Some(7)));
        assert_eq!(q.store.quest_for_pr(q.org, "acme/shop", 7).await?, Some(q.task));
        assert_eq!(q.store.quest_for_pr(q.org, "Acme/Other", 21).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn an_action_is_queued_once_and_settled_once_with_one_wake() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let action = q.action(ActionKind::Merge, None).await?;
        assert!(action.next_run_at.is_some_and(|at| at <= Utc::now()));
        let claimed: Vec<_> = q.store.claim_due_schedules().await?.into_iter().filter(|s| s.id == action.id).collect();
        assert_eq!(claimed.len(), 1);
        assert!(q.store.claim_due_schedules().await?.iter().all(|s| s.id != action.id), "a claimed schedule is not claimed again");
        let (first, second) = tokio::join!(q.store.queue_pr_action(&claimed[0]), q.store.queue_pr_action(&claimed[0]));
        let queued: Vec<Uuid> = [first?, second?].into_iter().flatten().collect();
        assert_eq!(queued.len(), 1, "two fires queue one run");
        assert!(!q.store.fire_schedule(&claimed[0]).await?);
        let run = queued[0];
        let (kind, role, dir): (String, String, Option<String>) = sqlx::query_as(
            "SELECT r.kind, s.role, s.working_dir FROM session_runs r JOIN agent_sessions s ON s.id = r.session_id WHERE r.id = $1",
        )
        .bind(run)
        .fetch_one(&q.pool)
        .await?;
        assert_eq!((kind.as_str(), role.as_str(), dir), ("action", "action", Some(format!("agents/{}", q.agent))));
        assert_eq!(q.store.pr_action_of_run(run).await?.unwrap().id, action.id);
        let merged = Outcome { merge_sha: Some("2222222222222222222222222222222222222222".into()), ..Outcome::ran("") };
        assert!(q.store.settle_pr_action(run, &merged).await?.is_some());
        assert!(q.store.settle_pr_action(run, &Outcome::failed("late")).await?.is_none());
        assert_eq!(q.events("action.ran").await?, 1);
        assert_eq!(q.events("action.failed").await?, 0);
        let (wakes, line): (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), MAX(payload->>'line') FROM agent_inbox WHERE agent_id = $1 AND kind = 'action.ran'",
        )
        .bind(q.agent)
        .fetch_one(&q.pool)
        .await?;
        assert_eq!((wakes, line.as_str()), (1, "Squash-merged Acme/Shop#7 at 11111111, merge commit 22222222"));
        let settled = q.row(action.id).await?;
        assert_eq!(settled.outcome.as_ref().and_then(|o| o.get("status")).and_then(Value::as_str), Some("ran"));
        assert!(!settled.enabled);
        assert!(schedule_state(&settled).starts_with("ran, merge commit 22222222"));
        Ok(())
    }

    #[tokio::test]
    async fn quest_events_fire_waiting_actions_and_a_cancel_skips_them() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let merge = q.action(ActionKind::Merge, Some("task.finished")).await?;
        let cleanup = q.action(ActionKind::DeleteBranch, Some("task.merged")).await?;
        assert!(merge.next_run_at.is_some_and(|at| at > Utc::now() + Duration::days(6)), "waiting actions give up after a week");
        assert!(schedule_state(&merge).starts_with("waits for the quest finishing its review"));
        let other = quest_in(q.pool.clone()).await?;
        other.store.emit(NewEvent::new("task.merged").agent(other.agent).task(Some(other.task)), None).await?;
        assert!(q.row(cleanup.id).await?.fired.is_none(), "another quest's event fires nothing");
        q.store.emit(NewEvent::new("task.merged").agent(q.agent).task(Some(q.task)), None).await?;
        let fired_merge = q.row(merge.id).await?;
        let fired_cleanup = q.row(cleanup.id).await?;
        assert!(fired_cleanup.next_run_at.is_some_and(|at| at <= Utc::now()));
        assert_eq!(trigger_problem(fired_cleanup.on_event.as_deref(), fired_cleanup.fired.as_ref()), None);
        assert!(trigger_problem(fired_merge.on_event.as_deref(), fired_merge.fired.as_ref()).unwrap().contains("merging came first"));
        q.store.emit(NewEvent::new("task.finished").agent(q.agent).task(Some(q.task)), None).await?;
        assert_eq!(q.row(merge.id).await?.fired.unwrap()["type"], "task.merged", "the first event wins");

        let waiting = q.action(ActionKind::Close, Some("task.pr_closed")).await?;
        sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1").bind(q.task).execute(&q.pool).await?;
        assert!(q.row(waiting.id).await?.fired.is_none(), "a finished quest keeps its actions");
        sqlx::query("UPDATE agent_tasks SET status = 'cancelled' WHERE id = $1").bind(q.task).execute(&q.pool).await?;
        let cancelled = q.row(waiting.id).await?;
        assert!(trigger_problem(cancelled.on_event.as_deref(), cancelled.fired.as_ref()).unwrap().contains("cancelled"));
        assert!(q.row(merge.id).await?.fired.unwrap()["type"] == "task.merged", "a fired action keeps what fired it");
        Ok(())
    }

    #[tokio::test]
    async fn merge_on_finished_and_delete_branch_on_merged_both_act_in_turn() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let merge = q.action(ActionKind::Merge, Some("task.finished")).await?;
        let cleanup = q.action(ActionKind::DeleteBranch, Some("task.merged")).await?;
        q.store.emit(NewEvent::new("task.finished").agent(q.agent).task(Some(q.task)), None).await?;
        let merging = q.row(merge.id).await?;
        assert_eq!(trigger_problem(merging.on_event.as_deref(), merging.fired.as_ref()), None, "the merge fires on its own event");
        assert!(q.row(cleanup.id).await?.fired.is_none(), "the review finishing does not touch the cleanup waiting for the merge");
        q.store.emit(NewEvent::new("task.merged").agent(q.agent).task(Some(q.task)), None).await?;
        let cleaning = q.row(cleanup.id).await?;
        assert_eq!(trigger_problem(cleaning.on_event.as_deref(), cleaning.fired.as_ref()), None, "the cleanup fires on the merge");
        assert_eq!(q.row(merge.id).await?.fired.unwrap()["type"], "task.finished", "a fired action keeps what fired it");

        let other = quest_in(q.pool.clone()).await?;
        let late_cleanup = other.action(ActionKind::DeleteBranch, Some("task.merged")).await?;
        let close = other.action(ActionKind::Close, Some("task.pr_closed")).await?;
        other.store.emit(NewEvent::new("task.pr_closed").agent(other.agent).task(Some(other.task)), None).await?;
        let ruled_out = other.row(late_cleanup.id).await?;
        assert!(trigger_problem(ruled_out.on_event.as_deref(), ruled_out.fired.as_ref()).unwrap().contains("closing unmerged came first"));
        let closing = other.row(close.id).await?;
        assert_eq!(trigger_problem(closing.on_event.as_deref(), closing.fired.as_ref()), None);
        Ok(())
    }

    #[tokio::test]
    async fn a_finished_wait_fires_on_any_review_ending_but_never_a_compare_review() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let merge = q.action(ActionKind::Merge, Some("task.finished")).await?;
        let cleanup = q.action(ActionKind::DeleteBranch, Some("task.merged")).await?;
        let reviewer = |asked_by: Value| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_sessions (agent_id, role, backend, model, task_id, parent_session_id, asked_by)
                 VALUES ($1, 'review', 'claude', 'haiku', $2, $3, $4) RETURNING id",
            )
            .bind(q.agent)
            .bind(q.task)
            .bind(q.session)
            .bind(asked_by)
            .fetch_one(&q.pool)
        };
        let compare = reviewer(json!({ "compare": [q.session] })).await?;
        q.store.emit(NewEvent::new("review.finished").agent(q.agent).session(compare).task(Some(q.task)), None).await?;
        assert!(q.row(merge.id).await?.fired.is_none(), "choosing between attempts is not the review a merge waits for");
        let manual = reviewer(json!({ "manual": true })).await?;
        q.store.emit(NewEvent::new("review.finished").agent(q.agent).session(manual).task(Some(q.task)), None).await?;
        let fired = q.row(merge.id).await?;
        assert_eq!(fired.fired.as_ref().unwrap()["type"], "review.finished");
        assert_eq!(trigger_problem(fired.on_event.as_deref(), fired.fired.as_ref()), None);
        assert!(q.row(cleanup.id).await?.fired.is_none(), "a review ending does not touch a wait for the merge");
        Ok(())
    }

    #[tokio::test]
    async fn deleting_a_queued_action_cancels_its_run_and_its_prompt_cannot_change() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("PR_ACTIONS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let q = quest(&url).await?;
        let action = q.action(ActionKind::Close, None).await?;
        assert!(!q.store.update_agent_schedule(q.agent, action.id, Some("do something else"), None).await?);
        assert!(sqlx::query("UPDATE schedules SET cron_expression = '* * * * *' WHERE id = $1").bind(action.id).execute(&q.pool).await.is_err());
        let run = q.store.queue_pr_action(&action).await?.unwrap();
        assert!(q.store.delete_agent_schedule(q.agent, action.id).await?);
        let status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1").bind(run).fetch_one(&q.pool).await?;
        assert_eq!(status, "cancelled");
        let session: String = sqlx::query_scalar("SELECT s.status FROM agent_sessions s JOIN session_runs r ON r.session_id = s.id WHERE r.id = $1")
            .bind(run)
            .fetch_one(&q.pool)
            .await?;
        assert_eq!(session, "archived", "a cancelled action leaves no open session behind");
        assert!(q.store.pr_action_of_run(run).await?.is_none());
        Ok(())
    }

}
