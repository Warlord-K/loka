use anyhow::{Context, Result};
use loka_shared::runners::{pr_sync_from, PrSync};
use std::path::Path;
use tracing::info;

use crate::cli::workspace_command;

pub struct PrInfo {
    pub url: String,
    pub number: i32,
}

pub async fn create_pr(
    worktree_path: &Path,
    repo: &str,
    branch: &str,
    base_branch: &str,
    title: &str,
    body: &str,
) -> Result<Option<PrInfo>> {
    info!(repo, branch, "Creating PR");

    let output = workspace_command("gh", worktree_path)
        .await
        .args([
            "pr",
            "create",
            "--repo",
            repo,
            "--head",
            branch,
            "--base",
            base_branch,
            "--title",
            title,
            "--body",
            body,
        ])
        .output()
        .await
        .context("Failed to run gh pr create")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already exists") {
            return get_existing_pr(worktree_path, repo, branch).await.map(Some);
        }
        if nothing_to_merge(&stderr) {
            info!(repo, branch, base = base_branch, "Skipping the pull request, GitHub sees nothing to merge: {}", stderr.trim());
            return Ok(None);
        }
        anyhow::bail!("gh pr create failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let pr_url = stdout.lines().last().unwrap_or(&stdout).trim().to_string();

    let number = extract_pr_number(&pr_url).unwrap_or(0);

    info!(pr_url = %pr_url, number, "PR created");
    Ok(Some(PrInfo {
        url: pr_url,
        number,
    }))
}

pub fn nothing_to_merge(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("no history in common") || text.contains("no commits between")
}

async fn get_existing_pr(worktree_path: &Path, repo: &str, branch: &str) -> Result<PrInfo> {
    let output = workspace_command("gh", worktree_path)
        .await
        .args(["pr", "view", branch, "--repo", repo, "--json", "url,number"])
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)?;

    Ok(PrInfo {
        url: json["url"].as_str().unwrap_or("").to_string(),
        number: json["number"].as_i64().unwrap_or(0) as i32,
    })
}

pub async fn pr_state(worktree_path: &Path, repo: &str, number: i64) -> Result<Option<String>> {
    let output = workspace_command("gh", worktree_path)
        .await
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "state",
        ])
        .output()
        .await
        .context("Failed to run gh pr view")?;
    if !output.status.success() {
        return Ok(None);
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(json["state"].as_str().map(str::to_string))
}

pub fn is_final_pr_state(state: &str) -> bool {
    matches!(state, "MERGED" | "CLOSED")
}

pub async fn pr_sync(worktree_path: &Path, since: std::time::SystemTime) -> PrSync {
    pr_sync_checked(worktree_path, since).await.unwrap_or(PrSync::Changed)
}

async fn pr_sync_checked(worktree_path: &Path, since: std::time::SystemTime) -> Result<PrSync> {
    let git = |args: Vec<String>| async move {
        let output = workspace_command("git", worktree_path)
            .await
            .args(args)
            .output()
            .await
            .context("Failed to run git")?;
        anyhow::ensure!(output.status.success(), "git failed");
        Ok::<String, anyhow::Error>(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let branch = git(vec!["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()]).await?;
    let mut view = workspace_command("gh", worktree_path).await;
    view.args(["pr", "view"]);
    if !branch.is_empty() && branch != "HEAD" {
        view.arg(&branch);
    }
    let state = match view.args(["--json", "state"]).output().await {
        Ok(output) if output.status.success() => serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|json| json["state"].as_str().map(str::to_string)),
        _ => None,
    };
    if state.as_deref() == Some("MERGED") {
        return Ok(PrSync::Merged);
    }
    let dirty = !git(vec!["status".into(), "--porcelain".into(), "--untracked-files=all".into()]).await?.is_empty();
    let since = chrono::DateTime::<chrono::Utc>::from(since).to_rfc3339();
    let new_commits = !git(vec!["rev-list".into(), "--max-count=1".into(), format!("--since={since}"), "HEAD".into()]).await?.is_empty();
    Ok(pr_sync_from(state.as_deref(), dirty, new_commits))
}

pub fn extends_recorded_pr(recorded: Option<i64>, number: i32) -> bool {
    recorded.is_none_or(|recorded| recorded == i64::from(number))
}

pub enum HeadPr {
    Own(PrInfo, String),
    Elsewhere(PrInfo),
    Extends(PrInfo, String),
    Missing,
}

pub async fn open_pr_containing_head(
    worktree_path: &Path,
    repo: &str,
    base_branch: &str,
    quest_branch: &str,
    parent_pr: Option<i64>,
) -> Result<HeadPr> {
    let output = workspace_command("gh", worktree_path)
        .await
        .args([
            "pr",
            "list",
            "--repo",
            repo,
            "--author",
            "@me",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "url,number,headRefName,headRefOid,isCrossRepository",
        ])
        .output()
        .await
        .context("Failed to run gh pr list")?;
    if !output.status.success() {
        anyhow::bail!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    head_pr_from_list(worktree_path, &output.stdout, base_branch, quest_branch, parent_pr).await
}

pub async fn stack_base(worktree_path: &Path, repo: &str, base_branch: &str, quest_branch: &str, parent_pr: Option<i64>) -> String {
    let Some(number) = parent_pr.filter(|number| *number > 0) else {
        return base_branch.to_string();
    };
    let head = parent_pr_head(worktree_path, repo, number)
        .await
        .filter(|head| !crate::git::workspace::is_quest_branch(head, quest_branch));
    match crate::git::workspace::fetched_stack_parent(worktree_path, base_branch, head.as_deref()).await {
        Some(parent) => {
            info!(parent = %parent, quest_branch = %quest_branch, "Quest is stacked on its parent quest's open pull request, opening its pull request into that branch");
            parent
        }
        None => base_branch.to_string(),
    }
}

async fn parent_pr_head(worktree_path: &Path, repo: &str, number: i64) -> Option<String> {
    let output = workspace_command("gh", worktree_path)
        .await
        .args([
            "pr", "view", &number.to_string(), "--repo", repo, "--json", "headRefName,state,isCrossRepository",
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    open_same_repo_head(&output.stdout)
}

fn open_same_repo_head(viewed: &[u8]) -> Option<String> {
    let pr: serde_json::Value = serde_json::from_slice(viewed).ok()?;
    if pr["state"].as_str() != Some("OPEN") || pr["isCrossRepository"].as_bool().unwrap_or(true) {
        return None;
    }
    pr["headRefName"].as_str().map(str::trim).filter(|head| !head.is_empty()).map(str::to_string)
}

async fn head_pr_from_list(
    worktree_path: &Path,
    listed: &[u8],
    base_branch: &str,
    quest_branch: &str,
    parent_pr: Option<i64>,
) -> Result<HeadPr> {
    let prs: Vec<serde_json::Value> = serde_json::from_slice(listed)?;
    let prs: Vec<serde_json::Value> = prs.into_iter().filter(|pr| !pr["isCrossRepository"].as_bool().unwrap_or(false)).collect();
    if prs.is_empty() {
        return Ok(HeadPr::Missing);
    }
    let base_ref = format!("origin/{base_branch}");
    let unpublished = commits_only_in_head(worktree_path, &base_ref).await;
    let head_sha = head_commit(worktree_path).await;
    let checked_out = crate::git::workspace::current_branch(worktree_path)
        .await
        .ok()
        .filter(|branch| branch != "HEAD" && branch != base_branch);
    let mut open = Vec::with_capacity(prs.len());
    for pr in prs {
        let head = pr["headRefOid"].as_str().unwrap_or("").to_string();
        let contains = match &unpublished {
            Some(commits) => commits.contains(&head),
            None => is_ancestor(worktree_path, &head, "HEAD").await? && !is_ancestor(worktree_path, &head, &base_ref).await?,
        };
        let distance = match (contains, head_sha.as_deref()) {
            (true, Some(tip)) if tip != head => commits_between(worktree_path, &head, tip).await,
            _ => 0,
        };
        open.push(OpenPr {
            info: PrInfo {
                url: pr["url"].as_str().unwrap_or("").to_string(),
                number: pr["number"].as_i64().unwrap_or(0) as i32,
            },
            head_ref: pr["headRefName"].as_str().unwrap_or("").to_string(),
            head,
            contains,
            distance,
        });
    }
    Ok(classify_open_prs(open, head_sha.as_deref(), quest_branch, checked_out.as_deref(), parent_pr))
}

struct OpenPr {
    info: PrInfo,
    head_ref: String,
    head: String,
    contains: bool,
    distance: usize,
}

fn branch_name(name: &str) -> &str {
    name.trim().trim_start_matches("refs/heads/")
}

fn is_exactly_ours(head_ref: &str, quest_branch: &str, checked_out: Option<&str>) -> bool {
    let name = branch_name(head_ref);
    if name.is_empty() {
        return false;
    }
    name == branch_name(quest_branch)
        || checked_out.map(branch_name).is_some_and(|current| {
            current == name && crate::git::workspace::is_quest_branch(current, quest_branch)
        })
}

fn classify_open_prs(
    prs: Vec<OpenPr>,
    head_sha: Option<&str>,
    quest_branch: &str,
    checked_out: Option<&str>,
    parent_pr: Option<i64>,
) -> HeadPr {
    let mut by_branch = None;
    let mut elsewhere = None;
    let mut extends: Option<(PrInfo, String, usize)> = None;
    for pr in prs {
        if crate::git::workspace::is_quest_branch(&pr.head_ref, quest_branch) {
            if pr.contains {
                return HeadPr::Own(pr.info, pr.head_ref);
            }
            if by_branch.is_none() && is_exactly_ours(&pr.head_ref, quest_branch, checked_out) {
                by_branch = Some(pr);
            }
            continue;
        }
        if !pr.contains {
            continue;
        }
        if head_sha == Some(pr.head.as_str()) {
            if elsewhere.is_none() {
                elsewhere = Some(pr.info);
            }
        } else if parent_pr.is_none()
            && loka_shared::git_names::validate_git_ref(branch_name(&pr.head_ref)).is_ok()
            && extends.as_ref().is_none_or(|(_, _, nearest)| pr.distance < *nearest)
        {
            extends = Some((pr.info, branch_name(&pr.head_ref).to_string(), pr.distance));
        }
    }
    match (by_branch, elsewhere, extends) {
        (Some(pr), _, _) => HeadPr::Own(pr.info, pr.head_ref),
        (None, Some(info), _) => HeadPr::Elsewhere(info),
        (None, None, Some((info, head_ref, _))) => HeadPr::Extends(info, head_ref),
        (None, None, None) => HeadPr::Missing,
    }
}

async fn head_commit(worktree_path: &Path) -> Option<String> {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn commits_only_in_head(worktree_path: &Path, base_ref: &str) -> Option<std::collections::HashSet<String>> {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["rev-list", "HEAD", &format!("^{base_ref}"), "--"])
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect())
}

async fn commits_between(worktree_path: &Path, from: &str, to: &str) -> usize {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["rev-list", "--count", &format!("{from}..{to}"), "--"])
        .output()
        .await;
    output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .unwrap_or(usize::MAX)
}

async fn is_ancestor(worktree_path: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .await?;
    Ok(output.status.success())
}

fn extract_pr_number(url: &str) -> Option<i32> {
    url.rsplit('/').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn open_pr(number: i32, head_ref: &str, head: &str, contains: bool) -> OpenPr {
        OpenPr {
            info: PrInfo { url: format!("https://github.com/o/r/pull/{number}"), number },
            head_ref: head_ref.to_string(),
            head: head.to_string(),
            contains,
            distance: usize::from(contains),
        }
    }

    #[test]
    fn a_contained_parent_pr_is_never_the_quest_own() {
        let quest = "add-sprite-stack-8210017e";
        let parent = || open_pr(113, "sprite-stacked-world-characters-buildings-7d988444", "parent", true);
        assert_eq!(label(classify_open_prs(vec![parent()], Some("child"), quest, Some(quest), Some(113))), "missing");
        assert_eq!(label(classify_open_prs(vec![parent()], Some("child"), quest, None, Some(113))), "missing");
        assert_eq!(label(classify_open_prs(vec![parent()], Some("child"), quest, None, None)), "extends 113 sprite-stacked-world-characters-buildings-7d988444");
        assert_eq!(label(classify_open_prs(vec![parent()], Some("parent"), quest, Some(quest), None)), "elsewhere 113");
        let own = open_pr(120, quest, "child", true);
        assert_eq!(label(classify_open_prs(vec![parent(), own], Some("child"), quest, Some(quest), None)), "own 120 add-sprite-stack-8210017e");
    }

    #[test]
    fn only_an_open_same_repo_parent_pr_is_a_stack_base() {
        assert_eq!(
            open_same_repo_head(br#"{"headRefName":"parent-1","state":"OPEN","isCrossRepository":false}"#),
            Some("parent-1".to_string())
        );
        assert_eq!(open_same_repo_head(br#"{"headRefName":"parent-1","state":"OPEN","isCrossRepository":true}"#), None);
        assert_eq!(open_same_repo_head(br#"{"headRefName":"parent-1","state":"MERGED","isCrossRepository":false}"#), None);
        assert_eq!(open_same_repo_head(br#"{"headRefName":"parent-1","state":"OPEN"}"#), None);
        assert_eq!(open_same_repo_head(b"not json"), None);
    }

    fn label(found: HeadPr) -> String {
        match found {
            HeadPr::Own(pr, head) => format!("own {} {head}", pr.number),
            HeadPr::Elsewhere(pr) => format!("elsewhere {}", pr.number),
            HeadPr::Extends(pr, head) => format!("extends {} {head}", pr.number),
            HeadPr::Missing => "missing".to_string(),
        }
    }

    #[test]
    fn github_refusals_for_branches_with_nothing_to_merge_are_skips() {
        assert!(nothing_to_merge("pull request create failed: GraphQL: The rewrite-karna-out-of-world-5f0189d4 branch has no history in common with main (createPullRequest)"));
        assert!(nothing_to_merge("pull request create failed: GraphQL: No commits between main and fix-x-dcec4a9d (createPullRequest)"));
        assert!(!nothing_to_merge("pull request create failed: GraphQL: Resource not accessible by integration (createPullRequest)"));
        assert!(!nothing_to_merge("gh: GraphQL: missing scope: repo (createPullRequest)"));
        assert!(!nothing_to_merge(""));
    }

    #[test]
    fn work_on_top_of_an_open_pr_updates_that_pr_unless_it_is_the_stack_parent() {
        let quest = "merge-pr-98-d22f2222";
        let nearer = || OpenPr { distance: 1, ..open_pr(98, "browser-gate", "aaa", true) };
        let deeper = || OpenPr { distance: 4, ..open_pr(97, "browser-base", "bbb", true) };
        assert_eq!(label(classify_open_prs(vec![deeper(), nearer()], Some("ccc"), quest, Some(quest), None)), "extends 98 browser-gate");
        assert_eq!(label(classify_open_prs(vec![nearer(), deeper()], Some("ccc"), quest, Some(quest), None)), "extends 98 browser-gate");
        assert_eq!(label(classify_open_prs(vec![nearer(), deeper()], Some("ccc"), quest, Some(quest), Some(98))), "missing");
        assert_eq!(label(classify_open_prs(vec![nearer(), deeper()], Some("ccc"), quest, Some(quest), Some(55))), "missing");
        assert!(extends_recorded_pr(None, 98));
        assert!(extends_recorded_pr(Some(98), 98));
        assert!(!extends_recorded_pr(Some(94), 98));
        assert_eq!(label(classify_open_prs(vec![nearer()], Some("aaa"), quest, Some(quest), None)), "elsewhere 98");
        assert_eq!(label(classify_open_prs(vec![open_pr(98, "browser-gate", "aaa", false)], Some("ccc"), quest, Some(quest), None)), "missing");
        assert_eq!(label(classify_open_prs(vec![open_pr(98, "", "aaa", true)], Some("ccc"), quest, Some(quest), None)), "missing");
        assert_eq!(label(classify_open_prs(vec![open_pr(98, "bad..ref", "aaa", true)], Some("ccc"), quest, Some(quest), None)), "missing");
        assert_eq!(label(classify_open_prs(vec![nearer(), open_pr(94, quest, "ddd", true)], Some("ccc"), quest, Some(quest), None)), "own 94 merge-pr-98-d22f2222");
    }

    #[test]
    fn open_prs_are_classified_by_quest_branch_first() {
        let quest = "fix-x-dcec4a9d";
        let own = || open_pr(94, quest, "aaa", true);
        let foreign_at_head = || open_pr(86, "pr84", "bbb", true);
        assert_eq!(label(classify_open_prs(vec![foreign_at_head(), own()], Some("bbb"), quest, None, None)), "own 94 fix-x-dcec4a9d");
        assert_eq!(label(classify_open_prs(vec![foreign_at_head()], Some("bbb"), quest, None, None)), "elsewhere 86");
        assert_eq!(label(classify_open_prs(vec![open_pr(86, "pr84", "bbb", true)], Some("ccc"), quest, None, None)), "extends 86 pr84");
        assert_eq!(label(classify_open_prs(vec![open_pr(86, "pr84", "bbb", true)], Some("ccc"), quest, None, Some(86))), "missing");
        assert_eq!(label(classify_open_prs(vec![open_pr(86, "pr84", "bbb", false)], Some("bbb"), quest, None, None)), "missing");
        assert_eq!(label(classify_open_prs(vec![], Some("bbb"), quest, None, None)), "missing");
    }

    #[test]
    fn a_pr_on_the_quest_branch_stays_own_after_runtime_commits() {
        let quest = "fix-x-dcec4a9d";
        let stale_own = open_pr(94, quest, "old", false);
        assert_eq!(label(classify_open_prs(vec![stale_own], Some("new"), quest, None, None)), "own 94 fix-x-dcec4a9d");
        let renamed = || open_pr(95, "fix-x-dcec4a9d-2", "old", false);
        let foreign = || open_pr(86, "pr84", "new", true);
        assert_eq!(
            label(classify_open_prs(vec![foreign(), renamed()], Some("new"), quest, Some("fix-x-dcec4a9d-2"), None)),
            "own 95 fix-x-dcec4a9d-2"
        );
        assert_eq!(label(classify_open_prs(vec![foreign(), renamed()], Some("new"), quest, None, None)), "elsewhere 86");
        assert_eq!(label(classify_open_prs(vec![renamed()], Some("new"), quest, Some(quest), None)), "missing");
    }

    #[test]
    fn variants_of_the_quest_branch_count_only_when_they_hold_head() {
        let quest = "fix-login-1a2b3c4d";
        let prs = || vec![open_pr(40, "fix-login", "aaa", false), open_pr(41, "fix-login-2", "bbb", false)];
        assert_eq!(label(classify_open_prs(prs(), Some("ccc"), quest, None, None)), "missing");
        assert_eq!(label(classify_open_prs(prs(), Some("ccc"), quest, Some(quest), None)), "missing");
        assert_eq!(label(classify_open_prs(prs(), Some("ccc"), quest, Some("main"), None)), "missing");
        let shared = vec![open_pr(42, "eng-12/fix-login", "ddd", false)];
        assert_eq!(label(classify_open_prs(shared, Some("ccc"), quest, Some("eng-12/fix-login"), None)), "missing");
        assert_eq!(label(classify_open_prs(vec![open_pr(41, "fix-login-2", "ccc", true)], Some("ccc"), quest, None, None)), "own 41 fix-login-2");
        assert_eq!(label(classify_open_prs(prs(), Some("ccc"), quest, Some("fix-login-2"), None)), "own 41 fix-login-2");
    }

    struct TempRepos(std::path::PathBuf);

    impl Drop for TempRepos {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn git(dir: &Path, args: &[&str]) -> String {
        let output = tokio::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .args(args)
            .output()
            .await
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    async fn commit_file(dir: &Path, name: &str) -> String {
        tokio::fs::write(dir.join(name), name).await.unwrap();
        git(dir, &["add", name]).await;
        git(dir, &["commit", "-m", name]).await;
        git(dir, &["rev-parse", "HEAD"]).await
    }

    fn listed(prs: &[(i32, &str, &str)]) -> Vec<u8> {
        let rows: Vec<serde_json::Value> = prs
            .iter()
            .map(|(number, head_ref, head)| {
                json!({
                    "url": format!("https://github.com/o/r/pull/{number}"),
                    "number": number,
                    "headRefName": head_ref,
                    "headRefOid": head,
                    "isCrossRepository": false,
                })
            })
            .collect();
        serde_json::to_vec(&rows).unwrap()
    }

    #[tokio::test]
    async fn contains_is_computed_from_a_real_remote() {
        let root = TempRepos(std::env::temp_dir().join(format!("loka-head-pr-{}", uuid::Uuid::new_v4())));
        let remote = root.0.join("remote.git");
        let work = root.0.join("work");
        tokio::fs::create_dir_all(&remote).await.unwrap();
        tokio::fs::create_dir_all(&work).await.unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]).await;
        git(&work, &["init", "--initial-branch=main"]).await;
        git(&work, &["config", "user.name", "Loka Test"]).await;
        git(&work, &["config", "user.email", "test@loka.ygpt.dev"]).await;
        git(&work, &["remote", "add", "origin", remote.to_str().unwrap()]).await;
        commit_file(&work, "base").await;
        git(&work, &["push", "origin", "main"]).await;

        let quest = "fix-login-1a2b3c4d";
        git(&work, &["switch", "-c", "fix-login"]).await;
        let old_style = commit_file(&work, "old-style").await;
        git(&work, &["push", "origin", "fix-login"]).await;

        git(&work, &["switch", "-c", "other", "main"]).await;
        let other = commit_file(&work, "other").await;
        git(&work, &["push", "origin", "other"]).await;

        git(&work, &["switch", "-c", quest, "main"]).await;
        let published = commit_file(&work, "quest").await;
        git(&work, &["push", "origin", quest]).await;
        let local = commit_file(&work, "runtime").await;
        git(&work, &["fetch", "origin"]).await;

        let all = listed(&[(40, "fix-login", &old_style), (41, "other", &other), (94, quest, &published)]);
        assert_eq!(label(head_pr_from_list(&work, &all, "main", quest, None).await.unwrap()), format!("own 94 {quest}"));

        let variants = listed(&[(40, "fix-login", &old_style), (41, "fix-login-2", &other)]);
        assert_eq!(label(head_pr_from_list(&work, &variants, "main", quest, None).await.unwrap()), "missing");

        let stale = listed(&[(94, quest, &other)]);
        assert_eq!(label(head_pr_from_list(&work, &stale, "main", quest, None).await.unwrap()), format!("own 94 {quest}"));

        let at_head = listed(&[(41, "other", &other), (86, "pr84", &local)]);
        assert_eq!(label(head_pr_from_list(&work, &at_head, "main", quest, None).await.unwrap()), "elsewhere 86");

        git(&work, &["switch", "fix-login"]).await;
        let checked_out = listed(&[(40, "fix-login", &other)]);
        assert_eq!(label(head_pr_from_list(&work, &checked_out, "main", quest, None).await.unwrap()), "own 40 fix-login");
        git(&work, &["switch", "-c", "pr-91", &published]).await;
        let fix_one = commit_file(&work, "fix-one").await;
        let fix_two = commit_file(&work, "fix-two").await;
        let on_top = listed(&[(91, "pr-91", &fix_one), (92, "pr-92", &published)]);
        assert_eq!(label(head_pr_from_list(&work, &on_top, "main", quest, None).await.unwrap()), "extends 91 pr-91");
        let exact = listed(&[(91, "pr-91", &fix_two)]);
        assert_eq!(label(head_pr_from_list(&work, &exact, "main", quest, None).await.unwrap()), "elsewhere 91");
        assert_eq!(label(head_pr_from_list(&work, &on_top, "main", quest, Some(91)).await.unwrap()), "missing");
        git(&work, &["switch", "fix-login"]).await;
        let cousin = listed(&[(41, "fix-login-2", &other)]);
        assert_eq!(label(head_pr_from_list(&work, &cousin, "main", quest, None).await.unwrap()), "missing");
    }

    #[test]
    fn final_pr_states_stop_publish() {
        assert!(super::is_final_pr_state("MERGED"));
        assert!(super::is_final_pr_state("CLOSED"));
        assert!(!super::is_final_pr_state("OPEN"));
        assert!(!super::is_final_pr_state(""));
    }
}
