use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use loka_shared::pr_actions::{decide, loka_problem, ActionJob, ActionKind, Decision, LivePr, Outcome};
use serde_json::Value;

const GH_TIMEOUT: Duration = Duration::from_secs(60);
const RERUN_CONCLUSIONS: &[&str] = &["failure", "cancelled", "timed_out", "startup_failure"];

pub(crate) trait GitHub {
    async fn pr(&self, repo: &str, number: i64) -> Result<LivePr>;
    async fn merge(&self, repo: &str, number: i64, sha: &str) -> Result<Option<String>>;
    async fn close(&self, repo: &str, number: i64) -> Result<()>;
    async fn delete_branch(&self, repo: &str, branch: &str) -> Result<bool>;
    async fn rerun_failed(&self, repo: &str, sha: &str, check: Option<&str>) -> Result<usize>;
}

pub async fn perform<G: GitHub>(github: &G, job: &ActionJob) -> Outcome {
    if let Some(problem) = loka_problem(job) {
        return Outcome::skipped(problem);
    }
    let spec = &job.spec;
    let live = match github.pr(&spec.repo, spec.number).await {
        Ok(live) => live,
        Err(error) => return Outcome::failed(format!("could not read {}: {error:#}", spec.pr())),
    };
    match decide(spec, &job.facts, &live) {
        Decision::Skip(reason) => Outcome::skipped(reason),
        Decision::AlreadyDone(reason) => Outcome::ran(reason),
        Decision::Act => {
            let acted = match spec.kind {
                ActionKind::Merge => github.merge(&spec.repo, spec.number, &spec.sha).await.map(|merge_sha| Outcome { merge_sha, ..Outcome::ran("") }),
                ActionKind::Close => github.close(&spec.repo, spec.number).await.map(|_| Outcome::ran("")),
                ActionKind::DeleteBranch => github.delete_branch(&spec.repo, &live.head_branch).await.map(|deleted| {
                    Outcome::ran(if deleted { format!("deleted {}", live.head_branch) } else { format!("{} was already deleted", live.head_branch) })
                }),
                ActionKind::RerunChecks => github.rerun_failed(&spec.repo, &spec.sha, spec.check.as_deref()).await.map(|count| match count {
                    0 => Outcome::skipped("no failed GitHub Actions runs to rerun on this head"),
                    1 => Outcome::ran("1 run restarted"),
                    n => Outcome::ran(format!("{n} runs restarted")),
                }),
            };
            acted.unwrap_or_else(|error| Outcome::failed(format!("GitHub refused: {error:#}")))
        }
    }
}

pub struct Gh;

impl Gh {
    async fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        let mut command = crate::cli::job_command("gh");
        command.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
        tokio::time::timeout(GH_TIMEOUT, command.output()).await.map_err(|_| anyhow!("gh did not answer within a minute"))?.context("could not run gh")
    }

    async fn json(&self, args: &[&str]) -> Result<Value> {
        let output = self.run(args).await?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    async fn branch_sha(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        let output = self.run(&["api", &format!("repos/{repo}/git/ref/heads/{branch}")]).await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("404") || error.contains("Not Found") {
                return Ok(None);
            }
            bail!("{}", error.trim());
        }
        let body: Value = serde_json::from_slice(&output.stdout)?;
        Ok(body.pointer("/object/sha").and_then(Value::as_str).map(str::to_string))
    }
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

impl GitHub for Gh {
    async fn pr(&self, repo: &str, number: i64) -> Result<LivePr> {
        let view = self
            .json(&["pr", "view", &number.to_string(), "--repo", repo, "--json", "state,headRefOid,headRefName,baseRefName,isCrossRepository"])
            .await?;
        let about = self.json(&["api", &format!("repos/{repo}")]).await?;
        let head_branch = text(&view, "headRefName");
        let cross_repo = view.get("isCrossRepository").and_then(Value::as_bool).unwrap_or(true);
        let branch_sha = if cross_repo || head_branch.is_empty() { None } else { self.branch_sha(repo, &head_branch).await? };
        Ok(LivePr {
            state: text(&view, "state"),
            head_sha: text(&view, "headRefOid"),
            head_branch,
            base_branch: text(&view, "baseRefName"),
            default_branch: text(&about, "default_branch"),
            cross_repo,
            branch_sha,
        })
    }

    async fn merge(&self, repo: &str, number: i64, sha: &str) -> Result<Option<String>> {
        let output = self.run(&["pr", "merge", &number.to_string(), "--repo", repo, "--squash", "--match-head-commit", sha]).await?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        let view = self.json(&["pr", "view", &number.to_string(), "--repo", repo, "--json", "mergeCommit"]).await.ok();
        Ok(view.as_ref().and_then(|view| view.pointer("/mergeCommit/oid")).and_then(Value::as_str).map(str::to_string))
    }

    async fn close(&self, repo: &str, number: i64) -> Result<()> {
        let output = self.run(&["pr", "close", &number.to_string(), "--repo", repo]).await?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    async fn delete_branch(&self, repo: &str, branch: &str) -> Result<bool> {
        let output = self.run(&["api", "-X", "DELETE", &format!("repos/{repo}/git/refs/heads/{branch}")]).await?;
        if output.status.success() {
            return Ok(true);
        }
        let error = String::from_utf8_lossy(&output.stderr);
        if ["404", "422", "Not Found", "Reference does not exist"].iter().any(|gone| error.contains(gone)) {
            return Ok(false);
        }
        bail!("{}", error.trim())
    }

    async fn rerun_failed(&self, repo: &str, sha: &str, check: Option<&str>) -> Result<usize> {
        let runs = self.json(&["api", &format!("repos/{repo}/actions/runs?head_sha={sha}&per_page=100")]).await?;
        let wanted = check.map(str::to_lowercase);
        let failed: Vec<i64> = runs
            .get("workflow_runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|run| RERUN_CONCLUSIONS.contains(&text(run, "conclusion").as_str()))
            .filter(|run| wanted.as_deref().is_none_or(|name| text(run, "name").to_lowercase().contains(name)))
            .filter_map(|run| run.get("id").and_then(Value::as_i64))
            .collect();
        for id in &failed {
            let output = self.run(&["api", "-X", "POST", &format!("repos/{repo}/actions/runs/{id}/rerun-failed-jobs")]).await?;
            if !output.status.success() {
                bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
            }
        }
        Ok(failed.len())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use loka_shared::pr_actions::{ActionSpec, Checks, Facts, OutcomeStatus, Review};
    use std::sync::Mutex;
    use uuid::Uuid;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const MOVED: &str = "3333333333333333333333333333333333333333";

    pub(crate) struct FakeGitHub {
        pub(crate) live: Mutex<LivePr>,
        calls: Mutex<Vec<String>>,
        refuse: bool,
        merge_commit_unreadable: bool,
    }

    impl FakeGitHub {
        pub(crate) fn new(state: &str) -> Self {
            Self {
                live: Mutex::new(LivePr {
                    state: state.into(),
                    head_sha: HEAD.into(),
                    head_branch: "quest-branch".into(),
                    base_branch: "main".into(),
                    default_branch: "main".into(),
                    cross_repo: false,
                    branch_sha: Some(HEAD.into()),
                }),
                calls: Mutex::new(Vec::new()),
                refuse: false,
                merge_commit_unreadable: false,
            }
        }

        pub(crate) fn acted(&self) -> Vec<String> {
            self.calls.lock().unwrap().iter().filter(|call| !call.starts_with("pr ")).cloned().collect()
        }

        fn note(&self, call: String) -> Result<()> {
            self.calls.lock().unwrap().push(call);
            if self.refuse {
                bail!("Head branch was modified");
            }
            Ok(())
        }
    }

    impl GitHub for FakeGitHub {
        async fn pr(&self, repo: &str, number: i64) -> Result<LivePr> {
            self.calls.lock().unwrap().push(format!("pr {repo}#{number}"));
            Ok(self.live.lock().unwrap().clone())
        }

        async fn merge(&self, repo: &str, number: i64, sha: &str) -> Result<Option<String>> {
            self.note(format!("merge {repo}#{number} {sha}"))?;
            let mut live = self.live.lock().unwrap();
            live.state = "MERGED".into();
            Ok((!self.merge_commit_unreadable).then(|| "4444444444444444444444444444444444444444".into()))
        }

        async fn close(&self, repo: &str, number: i64) -> Result<()> {
            self.note(format!("close {repo}#{number}"))?;
            self.live.lock().unwrap().state = "CLOSED".into();
            Ok(())
        }

        async fn delete_branch(&self, repo: &str, branch: &str) -> Result<bool> {
            self.note(format!("delete {repo} {branch}"))?;
            Ok(self.live.lock().unwrap().branch_sha.take().is_some())
        }

        async fn rerun_failed(&self, repo: &str, sha: &str, _check: Option<&str>) -> Result<usize> {
            self.note(format!("rerun {repo} {sha}"))?;
            Ok(2)
        }
    }

    fn job(kind: ActionKind) -> ActionJob {
        ActionJob {
            schedule_id: Uuid::new_v4(),
            spec: ActionSpec { kind, repo: "acme/castle".into(), number: 264, sha: HEAD.into(), check: None },
            facts: Facts {
                granted: true,
                quest_repo: Some("acme/castle".into()),
                quest_pr: Some(264),
                quest_branch: Some("quest-branch".into()),
                quest_cancelled: false,
                published_head: Some(HEAD.into()),
                review: Review::Approved,
                checks: Checks::Passed,
            },
            trigger_problem: None,
        }
    }

    #[tokio::test]
    async fn every_action_acts_once_on_its_happy_path() {
        for (kind, state, call) in [
            (ActionKind::Merge, "OPEN", format!("merge acme/castle#264 {HEAD}")),
            (ActionKind::Close, "OPEN", "close acme/castle#264".to_string()),
            (ActionKind::RerunChecks, "OPEN", format!("rerun acme/castle {HEAD}")),
            (ActionKind::DeleteBranch, "MERGED", "delete acme/castle quest-branch".to_string()),
        ] {
            let github = FakeGitHub::new(state);
            let outcome = perform(&github, &job(kind)).await;
            assert_eq!(outcome.status, OutcomeStatus::Ran, "{kind:?}: {outcome:?}");
            assert_eq!(github.acted(), vec![call]);
        }
    }

    #[tokio::test]
    async fn a_merge_records_the_merge_commit() {
        let outcome = perform(&FakeGitHub::new("OPEN"), &job(ActionKind::Merge)).await;
        assert_eq!(outcome.merge_sha.as_deref(), Some("4444444444444444444444444444444444444444"));
    }

    #[tokio::test]
    async fn a_merge_whose_commit_cannot_be_read_still_counts_as_merged() {
        let mut github = FakeGitHub::new("OPEN");
        github.merge_commit_unreadable = true;
        let action = job(ActionKind::Merge);
        let outcome = perform(&github, &action).await;
        assert_eq!((outcome.status, outcome.merge_sha.as_deref()), (OutcomeStatus::Ran, None));
        assert!(loka_shared::pr_actions::merged_by(&action.spec, &outcome));
        assert!(!loka_shared::pr_actions::merged_by(&job(ActionKind::Close).spec, &outcome));
    }

    #[tokio::test]
    async fn a_second_fire_after_the_action_ran_does_nothing_harmful() {
        let github = FakeGitHub::new("OPEN");
        assert_eq!(perform(&github, &job(ActionKind::Merge)).await.status, OutcomeStatus::Ran);
        let again = perform(&github, &job(ActionKind::Merge)).await;
        assert_eq!(again.status, OutcomeStatus::Skipped);
        assert!(again.reason.contains("already merged"), "{}", again.reason);
        assert_eq!(github.acted().len(), 1);
        let github = FakeGitHub::new("MERGED");
        assert_eq!(perform(&github, &job(ActionKind::DeleteBranch)).await.status, OutcomeStatus::Ran);
        let again = perform(&github, &job(ActionKind::DeleteBranch)).await;
        assert_eq!((again.status, again.reason.as_str()), (OutcomeStatus::Ran, "quest-branch was already deleted"));
        assert_eq!(github.acted().len(), 1);
    }

    #[tokio::test]
    async fn a_moved_head_skips_without_touching_github() {
        let github = FakeGitHub::new("OPEN");
        github.live.lock().unwrap().head_sha = MOVED.into();
        let outcome = perform(&github, &job(ActionKind::Merge)).await;
        assert_eq!(outcome.status, OutcomeStatus::Skipped);
        assert!(github.acted().is_empty());
    }

    #[tokio::test]
    async fn loka_side_problems_skip_before_github_is_asked() {
        let github = FakeGitHub::new("OPEN");
        let mut waited = job(ActionKind::Merge);
        waited.trigger_problem = Some("the quest was cancelled".into());
        assert_eq!(perform(&github, &waited).await, Outcome::skipped("the quest was cancelled"));
        let mut ungranted = job(ActionKind::Merge);
        ungranted.facts.granted = false;
        assert_eq!(perform(&github, &ungranted).await.status, OutcomeStatus::Skipped);
        assert!(github.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_github_refusal_is_a_failure_with_its_reason() {
        let mut github = FakeGitHub::new("OPEN");
        github.refuse = true;
        let outcome = perform(&github, &job(ActionKind::Merge)).await;
        assert_eq!(outcome.status, OutcomeStatus::Failed);
        assert!(outcome.reason.contains("Head branch was modified"));
    }
}
