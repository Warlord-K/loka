use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tracing::info;

use super::workspace::{is_quest_branch, run_git as git_run, run_git_checked_output as git_output, without_coauthors};
use crate::cli::workspace_command;

const MAX_COMMITS: usize = 100;
const MAX_FILES_PER_COMMIT: usize = 500;
const MAX_COMMIT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

const CREATE_COMMIT: &str = "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }";

#[derive(Debug, PartialEq, Eq)]
pub enum Replay {
    Published { commits: usize },
    Unsupported(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub additions: Vec<(String, String)>,
    pub deletions: Vec<String>,
}

pub(crate) struct Staged {
    pub additions: Vec<(String, Vec<u8>)>,
    pub deletions: Vec<String>,
}

struct Planned {
    message: String,
    changes: Changes,
}

pub(crate) trait Hub {
    async fn push_access(&self, dir: &Path, repo: &str) -> bool;
    async fn create_ref(&self, dir: &Path, repo: &str, branch: &str, sha: &str) -> Result<()>;
    async fn commit(&self, dir: &Path, repo: &str, branch: &str, expected: &str, message: &str, staged: &Staged) -> Result<String>;
}

pub(crate) struct GitHubHub;

pub fn enabled() -> bool {
    std::env::var("WORLD_API_COMMITS").map_or(true, |value| !matches!(value.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false"))
}

pub async fn available(dir: &Path, repo: &str) -> bool {
    enabled() && GitHubHub.push_access(dir, repo).await
}

pub async fn replay(dir: &Path, repo: &str, onto: &str, branch: &str, quest_branch: &str) -> Result<Replay> {
    replay_with(&GitHubHub, dir, repo, onto, branch, quest_branch, None).await
}

pub(crate) async fn replay_with<H: Hub>(
    hub: &H,
    dir: &Path,
    repo: &str,
    onto: &str,
    branch: &str,
    quest_branch: &str,
    protocols: Option<&str>,
) -> Result<Replay> {
    let remote = remote_tip(dir, branch, protocols).await?;
    let start = match &remote {
        Some(tip) => match start_for_remote(dir, tip, onto).await? {
            Some(start) => start,
            None => return Ok(Replay::Unsupported("the remote branch holds commits this workspace does not have".to_string())),
        },
        None => match git_output(dir, &["merge-base", "HEAD", &format!("origin/{onto}")]).await {
            Ok(base) => base.trim().to_string(),
            Err(_) => return Ok(Replay::Unsupported(format!("this work shares no history with {onto}"))),
        },
    };
    let range = format!("{start}..HEAD");
    if !git_output(dir, &["rev-list", "--merges", "--max-count=1", &range]).await?.trim().is_empty() {
        return Ok(Replay::Unsupported("the branch holds a merge commit, which the API cannot create".to_string()));
    }
    let commits: Vec<String> = git_output(dir, &["rev-list", "--reverse", "--topo-order", &range])
        .await?
        .lines()
        .map(str::to_string)
        .collect();
    if commits.is_empty() && remote.is_none() {
        return Ok(Replay::Unsupported("there are no commits to publish".to_string()));
    }
    if commits.len() > MAX_COMMITS {
        return Ok(Replay::Unsupported(format!("{} commits are more than the {MAX_COMMITS} the API publish replays", commits.len())));
    }
    let mut planned = Vec::new();
    let mut total = 0u64;
    for commit in &commits {
        let raw = git_output(dir, &["diff-tree", "-r", "-z", "--raw", "--no-renames", "--no-ext-diff", &format!("{commit}^"), commit]).await?;
        let changes = match parse_raw_diff(&raw) {
            Ok(changes) => changes,
            Err(reason) => return Ok(Replay::Unsupported(reason)),
        };
        if changes.additions.is_empty() && changes.deletions.is_empty() {
            continue;
        }
        let size = blob_sizes(dir, changes.additions.iter().map(|(_, sha)| sha.as_str())).await?;
        if let Err(reason) = check_limits(changes.additions.len() + changes.deletions.len(), size, total) {
            return Ok(Replay::Unsupported(reason));
        }
        total += size;
        let message = git_output(dir, &["show", "-s", "--format=%B", commit]).await?;
        planned.push(Planned { message: without_coauthors(message.trim()), changes });
    }
    if planned.is_empty() && remote.is_none() {
        return Ok(Replay::Unsupported("the commits change no files".to_string()));
    }
    let mut expected = match &remote {
        Some(tip) => tip.clone(),
        None => {
            hub.create_ref(dir, repo, branch, &start).await.context("could not create the branch through the API")?;
            start
        }
    };
    for step in &planned {
        let staged = load(dir, &step.changes).await?;
        expected = hub.commit(dir, repo, branch, &expected, &step.message, &staged).await?;
    }
    verify_and_sync(dir, branch, quest_branch, &expected, protocols).await?;
    info!(repo, branch, commits = planned.len(), "Published through the GitHub API, every commit is signed by GitHub");
    Ok(Replay::Published { commits: planned.len() })
}

fn seen_ref(branch: &str) -> String {
    format!("refs/loka-api/{branch}")
}

async fn remote_tip(dir: &Path, branch: &str, protocols: Option<&str>) -> Result<Option<String>> {
    match fetch_branch(dir, branch, protocols).await {
        Ok(()) => {
            let tip = git_output(dir, &["rev-parse", &seen_ref(branch)]).await?;
            Ok(Some(tip.trim().to_string()))
        }
        Err(error) if format!("{error:#}").contains("couldn't find remote ref") => Ok(None),
        Err(error) => Err(error),
    }
}

async fn fetch_branch(dir: &Path, branch: &str, protocols: Option<&str>) -> Result<()> {
    let refspec = format!("+refs/heads/{branch}:{}", seen_ref(branch));
    let mut command = workspace_command("git", dir).await;
    if let Some(protocols) = protocols {
        command.env("GIT_ALLOW_PROTOCOL", protocols);
    }
    let output = command
        .args(["fetch", "--no-tags", "--refmap=", "origin", &refspec])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("Failed to run git fetch")?;
    if !output.status.success() {
        bail!("git fetch failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

async fn start_for_remote(dir: &Path, tip: &str, onto: &str) -> Result<Option<String>> {
    if git_run(dir, &["merge-base", "--is-ancestor", tip, "HEAD"]).await.is_ok() {
        return Ok(Some(tip.to_string()));
    }
    let wanted = git_output(dir, &["rev-parse", &format!("{tip}^{{tree}}")]).await?.trim().to_string();
    let Ok(fork) = git_output(dir, &["merge-base", "HEAD", &format!("origin/{onto}")]).await else {
        return Ok(None);
    };
    let fork = fork.trim().to_string();
    let listed = git_output(dir, &["log", "--format=%H %T", &format!("{fork}..HEAD")]).await?;
    let matching = listed.lines().filter_map(|line| line.split_once(' ')).find(|(_, tree)| *tree == wanted).map(|(sha, _)| sha.to_string());
    if matching.is_some() {
        return Ok(matching);
    }
    let fork_tree = git_output(dir, &["rev-parse", &format!("{fork}^{{tree}}")]).await?;
    Ok((fork_tree.trim() == wanted).then_some(fork))
}

async fn verify_and_sync(dir: &Path, branch: &str, quest_branch: &str, tip: &str, protocols: Option<&str>) -> Result<()> {
    fetch_branch(dir, branch, protocols).await?;
    let remote = git_output(dir, &["rev-parse", &seen_ref(branch)]).await?;
    if remote.trim() != tip {
        bail!("the branch moved while publishing through the API");
    }
    let remote_tree = git_output(dir, &["rev-parse", &format!("{tip}^{{tree}}")]).await?;
    let local_tree = git_output(dir, &["rev-parse", "HEAD^{tree}"]).await?;
    if remote_tree.trim() != local_tree.trim() {
        bail!("the commits made through the API do not match this workspace, so the branch was not trusted");
    }
    let current = git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let current = current.trim();
    if current == "HEAD" || is_quest_branch(current, quest_branch) {
        git_run(dir, &["reset", "-q", "--mixed", tip]).await?;
        git_run(dir, &["update-ref", &format!("refs/remotes/origin/{branch}"), tip]).await?;
    }
    Ok(())
}

pub(crate) fn parse_raw_diff(raw: &str) -> std::result::Result<Changes, String> {
    let mut changes = Changes::default();
    let mut parts = raw.split('\0').filter(|part| !part.is_empty());
    while let Some(meta) = parts.next() {
        let path = parts.next().ok_or_else(|| "git printed a change without a path".to_string())?;
        let fields: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, _, new_sha, status] = fields[..] else {
            return Err(format!("git printed a change this publish cannot read: {meta}"));
        };
        let status = status.chars().next().unwrap_or(' ');
        match status {
            'D' if old_mode == "160000" => return Err(format!("{path} is a submodule, which the API cannot change")),
            'D' => changes.deletions.push(path.to_string()),
            'A' if new_mode == "100644" => changes.additions.push((path.to_string(), new_sha.to_string())),
            'M' if old_mode == new_mode && matches!(new_mode, "100644" | "100755") => {
                changes.additions.push((path.to_string(), new_sha.to_string()))
            }
            'A' | 'M' | 'T' => {
                return Err(format!(
                    "{path} is a symlink, submodule, new executable file or mode change ({old_mode} to {new_mode}), which the API cannot create"
                ))
            }
            _ => return Err(format!("{path} has a change ({status}) this publish cannot replay")),
        }
    }
    Ok(changes)
}

pub(crate) fn check_limits(files: usize, bytes: u64, already: u64) -> std::result::Result<(), String> {
    if files > MAX_FILES_PER_COMMIT {
        return Err(format!("a commit changes {files} files, over the {MAX_FILES_PER_COMMIT} the API publish sends in one commit"));
    }
    if bytes > MAX_COMMIT_BYTES {
        return Err(format!("a commit carries {} MB of files, over the {} MB the API publish sends in one commit", bytes >> 20, MAX_COMMIT_BYTES >> 20));
    }
    if already + bytes > MAX_TOTAL_BYTES {
        return Err(format!("the commits carry over {} MB of files in total, more than the API publish sends", MAX_TOTAL_BYTES >> 20));
    }
    Ok(())
}

pub(crate) fn commit_request(repo: &str, branch: &str, expected: &str, message: &str, staged: &Staged) -> Value {
    let message = message.trim();
    let (headline, body) = message.split_once('\n').map_or((message, ""), |(head, rest)| (head, rest.trim()));
    let headline = if headline.trim().is_empty() { "Update" } else { headline.trim() };
    let mut text = json!({ "headline": headline });
    if !body.is_empty() {
        text["body"] = json!(body);
    }
    let additions: Vec<Value> = staged.additions.iter().map(|(path, bytes)| json!({ "path": path, "contents": STANDARD.encode(bytes) })).collect();
    let deletions: Vec<Value> = staged.deletions.iter().map(|path| json!({ "path": path })).collect();
    json!({
        "query": CREATE_COMMIT,
        "variables": { "input": {
            "branch": { "repositoryNameWithOwner": repo, "branchName": branch },
            "message": text,
            "expectedHeadOid": expected,
            "fileChanges": { "additions": additions, "deletions": deletions },
        } },
    })
}

pub(crate) fn commit_oid(response: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(response).context("GitHub answered the commit request with something that is not JSON")?;
    if let Some(error) = value["errors"].as_array().and_then(|errors| errors.first()) {
        bail!("GitHub refused the commit: {}", error["message"].as_str().unwrap_or("no reason given"));
    }
    value["data"]["createCommitOnBranch"]["commit"]["oid"]
        .as_str()
        .map(str::to_string)
        .context("GitHub did not return the new commit")
}

async fn blob_sizes<'a>(dir: &Path, shas: impl Iterator<Item = &'a str>) -> Result<u64> {
    let input: String = shas.map(|sha| format!("{sha}\n")).collect();
    if input.is_empty() {
        return Ok(0);
    }
    let mut child = workspace_command("git", dir)
        .await
        .args(["cat-file", "--batch-check=%(objectsize)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to run git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file has no stdin")?;
    let writer = tokio::spawn(async move { stdin.write_all(input.as_bytes()).await });
    let output = child.wait_with_output().await.context("Failed to run git cat-file")?;
    let _ = writer.await;
    if !output.status.success() {
        bail!("git cat-file failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| line.trim().parse::<u64>().ok()).sum())
}

async fn load(dir: &Path, changes: &Changes) -> Result<Staged> {
    let mut additions = Vec::new();
    for (path, sha) in &changes.additions {
        let output = workspace_command("git", dir)
            .await
            .args(["cat-file", "blob", sha])
            .output()
            .await
            .context("Failed to run git cat-file")?;
        if !output.status.success() {
            bail!("git cat-file blob {sha} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        additions.push((path.clone(), output.stdout));
    }
    Ok(Staged { additions, deletions: changes.deletions.clone() })
}

async fn gh(dir: &Path, args: &[&str], stdin: Option<Vec<u8>>) -> Result<std::process::Output> {
    let mut command = workspace_command("gh", dir).await;
    command.args(args).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    } else {
        command.stdin(std::process::Stdio::null());
    }
    let mut child = command.spawn().context("Failed to run gh")?;
    let writer = stdin.and_then(|body| {
        child.stdin.take().map(|mut pipe| tokio::spawn(async move { pipe.write_all(&body).await }))
    });
    let output = child.wait_with_output().await.context("Failed to run gh")?;
    if let Some(writer) = writer {
        let _ = writer.await;
    }
    Ok(output)
}

impl Hub for GitHubHub {
    async fn push_access(&self, dir: &Path, repo: &str) -> bool {
        let path = format!("repos/{repo}");
        match gh(dir, &["api", &path, "--jq", ".permissions.push"], None).await {
            Ok(output) => output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true",
            Err(_) => false,
        }
    }

    async fn create_ref(&self, dir: &Path, repo: &str, branch: &str, sha: &str) -> Result<()> {
        let path = format!("repos/{repo}/git/refs");
        let reference = format!("ref=refs/heads/{branch}");
        let sha = format!("sha={sha}");
        let output = gh(dir, &["api", "-X", "POST", &path, "-f", &reference, "-f", &sha], None).await?;
        if !output.status.success() {
            bail!("gh api {path} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    async fn commit(&self, dir: &Path, repo: &str, branch: &str, expected: &str, message: &str, staged: &Staged) -> Result<String> {
        let body = serde_json::to_vec(&commit_request(repo, branch, expected, message, staged))?;
        let output = gh(dir, &["api", "graphql", "--input", "-"], Some(body)).await?;
        if serde_json::from_slice::<Value>(&output.stdout).is_err() {
            bail!("gh api graphql failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        commit_oid(&output.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use uuid::Uuid;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fresh_dir(label: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("loka-apic-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    async fn sh(dir: &Path, args: &[&str]) -> String {
        let output = tokio::process::Command::new("git")
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "user.name=Test", "-c", "user.email=test@loka.ygpt.dev", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct Fake {
        scratch: PathBuf,
        fail_at: Mutex<Option<usize>>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl Hub for Fake {
        async fn push_access(&self, _: &Path, _: &str) -> bool {
            true
        }

        async fn create_ref(&self, _: &Path, _: &str, branch: &str, sha: &str) -> Result<()> {
            sh(&self.scratch, &["fetch", "origin"]).await;
            sh(&self.scratch, &["push", "origin", &format!("{sha}:refs/heads/{branch}")]).await;
            Ok(())
        }

        async fn commit(&self, _: &Path, _: &str, branch: &str, expected: &str, message: &str, staged: &Staged) -> Result<String> {
            let seen = self.calls.lock().unwrap().len();
            if *self.fail_at.lock().unwrap() == Some(seen) {
                bail!("GitHub refused the commit: simulated outage");
            }
            sh(&self.scratch, &["fetch", "origin"]).await;
            let head = sh(&self.scratch, &["rev-parse", &format!("origin/{branch}")]).await;
            if head != expected {
                bail!("GitHub refused the commit: Expected branch to point to {expected} but it points to {head}");
            }
            sh(&self.scratch, &["checkout", "-q", "--detach", &head]).await;
            for path in &staged.deletions {
                std::fs::remove_file(self.scratch.join(path)).unwrap();
            }
            for (path, bytes) in &staged.additions {
                let target = self.scratch.join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, bytes).unwrap();
            }
            sh(&self.scratch, &["add", "-A"]).await;
            sh(&self.scratch, &["commit", "-q", "-m", message]).await;
            sh(&self.scratch, &["push", "origin", &format!("HEAD:refs/heads/{branch}")]).await;
            self.calls.lock().unwrap().push((expected.to_string(), message.to_string()));
            Ok(sh(&self.scratch, &["rev-parse", "HEAD"]).await)
        }
    }

    struct World {
        _dirs: Vec<Dir>,
        remote: PathBuf,
        local: PathBuf,
        hub: Fake,
    }

    async fn world() -> World {
        let remote = fresh_dir("remote");
        let local = fresh_dir("local");
        let scratch = fresh_dir("scratch");
        sh(&remote.0, &["init", "-q", "--bare", "--initial-branch=main"]).await;
        let url = format!("file://{}", remote.0.display());
        sh(&local.0, &["init", "-q", "--initial-branch=main"]).await;
        std::fs::write(local.0.join("base.txt"), "base\n").unwrap();
        std::fs::write(local.0.join("gone.txt"), "gone\n").unwrap();
        sh(&local.0, &["add", "-A"]).await;
        sh(&local.0, &["commit", "-q", "-m", "base"]).await;
        sh(&local.0, &["remote", "add", "origin", &url]).await;
        sh(&local.0, &["push", "-q", "origin", "main:main"]).await;
        sh(&local.0, &["fetch", "-q", "origin"]).await;
        sh(&local.0, &["switch", "-q", "-c", "quest-1234abcd"]).await;
        sh(&scratch.0, &["clone", "-q", &url, "."]).await;
        let hub = Fake { scratch: scratch.0.clone(), fail_at: Mutex::new(None), calls: Mutex::new(Vec::new()) };
        World { remote: remote.0.clone(), local: local.0.clone(), hub, _dirs: vec![remote, local, scratch] }
    }

    async fn run(world: &World) -> Result<Replay> {
        replay_with(&world.hub, &world.local, "acme/app", "main", "quest-1234abcd", "quest-1234abcd", Some("file")).await
    }

    async fn commit_file(world: &World, name: &str, bytes: &[u8], message: &str) {
        let target = world.local.join(name);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
        sh(&world.local, &["add", "-A"]).await;
        sh(&world.local, &["commit", "-q", "-m", message]).await;
    }

    async fn remote_tree(world: &World, branch: &str) -> String {
        sh(&world.remote, &["rev-parse", &format!("{branch}^{{tree}}")]).await
    }

    #[tokio::test]
    async fn local_commits_become_one_api_commit_each_with_text_binary_and_deletes() {
        let world = world().await;
        commit_file(&world, "a.txt", b"alpha\n", "feat: add a\n\nwith a body").await;
        let binary: Vec<u8> = (0..=255u8).collect();
        commit_file(&world, "dir/bin.dat", &binary, "feat: binary").await;
        std::fs::remove_file(world.local.join("gone.txt")).unwrap();
        commit_file(&world, "a.txt", b"alpha two\n", "fix: edit and delete").await;
        let before = sh(&world.local, &["rev-parse", "HEAD^{tree}"]).await;
        assert_eq!(run(&world).await.unwrap(), Replay::Published { commits: 3 });
        assert_eq!(remote_tree(&world, "quest-1234abcd").await, before);
        let messages: Vec<String> = world.hub.calls.lock().unwrap().iter().map(|(_, message)| message.clone()).collect();
        assert_eq!(messages, ["feat: add a\n\nwith a body", "feat: binary", "fix: edit and delete"]);
        let tip = sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await;
        assert_eq!(sh(&world.local, &["rev-parse", "HEAD"]).await, tip);
        let base = sh(&world.remote, &["rev-parse", "main"]).await;
        assert_eq!(sh(&world.remote, &["rev-parse", "quest-1234abcd~3"]).await, base);
    }

    #[tokio::test]
    async fn a_follow_up_adds_only_the_new_commits_on_top_of_the_remote_branch() {
        let world = world().await;
        commit_file(&world, "a.txt", b"one\n", "feat: one").await;
        run(&world).await.unwrap();
        let first_tip = sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await;
        assert_eq!(run(&world).await.unwrap(), Replay::Published { commits: 0 });
        commit_file(&world, "b.txt", b"two\n", "fix: review").await;
        assert_eq!(run(&world).await.unwrap(), Replay::Published { commits: 1 });
        let calls = world.hub.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, first_tip);
        assert_eq!(remote_tree(&world, "quest-1234abcd").await, sh(&world.local, &["rev-parse", "HEAD^{tree}"]).await);
    }

    #[tokio::test]
    async fn a_half_published_branch_resumes_from_the_matching_tree() {
        let world = world().await;
        commit_file(&world, "a.txt", b"one\n", "feat: one").await;
        commit_file(&world, "b.txt", b"two\n", "feat: two").await;
        commit_file(&world, "c.txt", b"three\n", "feat: three").await;
        *world.hub.fail_at.lock().unwrap() = Some(2);
        assert!(run(&world).await.is_err());
        assert_eq!(world.hub.calls.lock().unwrap().len(), 2);
        *world.hub.fail_at.lock().unwrap() = None;
        assert_eq!(run(&world).await.unwrap(), Replay::Published { commits: 1 });
        assert_eq!(remote_tree(&world, "quest-1234abcd").await, sh(&world.local, &["rev-parse", "HEAD^{tree}"]).await);
        assert_eq!(world.hub.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn merge_commits_fall_back_to_a_signed_push() {
        let world = world().await;
        commit_file(&world, "a.txt", b"one\n", "feat: one").await;
        sh(&world.local, &["switch", "-q", "main"]).await;
        commit_file(&world, "main-only.txt", b"m\n", "chore: main moves").await;
        sh(&world.local, &["switch", "-q", "quest-1234abcd"]).await;
        sh(&world.local, &["merge", "-q", "--no-ff", "-m", "merge main", "main"]).await;
        sh(&world.local, &["update-ref", "refs/remotes/origin/main", "main"]).await;
        match run(&world).await.unwrap() {
            Replay::Unsupported(reason) => assert!(reason.contains("merge commit"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(world.hub.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn executable_files_and_symlinks_fall_back_before_anything_is_created() {
        let world = world().await;
        std::fs::write(world.local.join("run.sh"), "#!/bin/sh\n").unwrap();
        sh(&world.local, &["add", "run.sh"]).await;
        sh(&world.local, &["update-index", "--chmod=+x", "run.sh"]).await;
        sh(&world.local, &["commit", "-q", "-m", "feat: script"]).await;
        match run(&world).await.unwrap() {
            Replay::Unsupported(reason) => assert!(reason.contains("run.sh"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(sh(&world.remote, &["branch", "--list", "quest-1234abcd"]).await.is_empty());
    }

    #[tokio::test]
    async fn a_remote_branch_with_foreign_commits_is_left_to_the_push_path() {
        let world = world().await;
        commit_file(&world, "a.txt", b"one\n", "feat: one").await;
        run(&world).await.unwrap();
        let scratch = &world.hub.scratch;
        sh(scratch, &["fetch", "origin"]).await;
        sh(scratch, &["checkout", "-q", "--detach", "origin/quest-1234abcd"]).await;
        std::fs::write(scratch.join("someone.txt"), "else\n").unwrap();
        sh(scratch, &["add", "-A"]).await;
        sh(scratch, &["commit", "-q", "-m", "someone else"]).await;
        sh(scratch, &["push", "origin", "HEAD:refs/heads/quest-1234abcd"]).await;
        let foreign = sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await;
        let published = sh(&world.local, &["rev-parse", "refs/remotes/origin/quest-1234abcd"]).await;
        assert_ne!(foreign, published);
        commit_file(&world, "b.txt", b"two\n", "feat: two").await;
        match run(&world).await.unwrap() {
            Replay::Unsupported(reason) => assert!(reason.contains("remote branch"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(sh(&world.local, &["rev-parse", "refs/remotes/origin/quest-1234abcd"]).await, published);
        assert!(!lease_push(&world).await);
        assert_eq!(sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await, foreign);
    }

    #[tokio::test]
    async fn a_foreign_branch_never_fetched_before_survives_the_fallback_push() {
        let world = world().await;
        let scratch = &world.hub.scratch;
        sh(scratch, &["fetch", "origin"]).await;
        sh(scratch, &["checkout", "-q", "--detach", "origin/main"]).await;
        std::fs::write(scratch.join("someone.txt"), "else\n").unwrap();
        sh(scratch, &["add", "-A"]).await;
        sh(scratch, &["commit", "-q", "-m", "someone else"]).await;
        sh(scratch, &["push", "origin", "HEAD:refs/heads/quest-1234abcd"]).await;
        let foreign = sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await;
        commit_file(&world, "b.txt", b"two\n", "feat: two").await;
        assert!(matches!(run(&world).await.unwrap(), Replay::Unsupported(_)));
        assert!(!lease_push(&world).await);
        assert_eq!(sh(&world.remote, &["rev-parse", "quest-1234abcd"]).await, foreign);
    }

    async fn lease_push(world: &World) -> bool {
        tokio::process::Command::new("git")
            .current_dir(&world.local)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["push", "--force-with-lease", "origin", "HEAD:refs/heads/quest-1234abcd"])
            .output()
            .await
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn raw_diffs_are_split_into_additions_and_deletions() {
        let raw = ":100644 100644 aaa bbb M\0src/a.rs\0:000000 100644 000 ccc A\0new file.txt\0:100644 000000 ddd 000 D\0old.txt\0:100755 100755 eee fff M\0run.sh\0";
        let changes = parse_raw_diff(raw).unwrap();
        assert_eq!(
            changes.additions,
            [("src/a.rs".to_string(), "bbb".to_string()), ("new file.txt".to_string(), "ccc".to_string()), ("run.sh".to_string(), "fff".to_string())]
        );
        assert_eq!(changes.deletions, ["old.txt"]);
        assert_eq!(parse_raw_diff("").unwrap(), Changes::default());
    }

    #[test]
    fn what_the_api_cannot_write_is_refused() {
        for raw in [
            ":000000 100755 000 abc A\0run.sh\0",
            ":000000 120000 000 abc A\0link\0",
            ":000000 160000 000 abc A\0vendor\0",
            ":100644 100755 abc abc M\0mode.sh\0",
            ":100644 120000 abc def T\0swapped\0",
            ":160000 000000 abc 000 D\0vendor\0",
        ] {
            assert!(parse_raw_diff(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn size_limits_name_what_was_exceeded() {
        assert!(check_limits(10, 1 << 20, 0).is_ok());
        assert!(check_limits(MAX_FILES_PER_COMMIT + 1, 0, 0).unwrap_err().contains("files"));
        assert!(check_limits(1, MAX_COMMIT_BYTES + 1, 0).unwrap_err().contains("one commit"));
        assert!(check_limits(1, MAX_COMMIT_BYTES, MAX_TOTAL_BYTES).unwrap_err().contains("in total"));
    }

    #[test]
    fn the_request_carries_headline_body_base64_contents_and_the_expected_head() {
        let staged = Staged { additions: vec![("a/b.bin".to_string(), vec![0, 1, 255])], deletions: vec!["old.txt".to_string()] };
        let request = commit_request("acme/app", "quest-1", "abc123", "fix: thing\n\nWhy it matters.\nMore.", &staged);
        let input = &request["variables"]["input"];
        assert_eq!(input["branch"], json!({ "repositoryNameWithOwner": "acme/app", "branchName": "quest-1" }));
        assert_eq!(input["expectedHeadOid"], "abc123");
        assert_eq!(input["message"], json!({ "headline": "fix: thing", "body": "Why it matters.\nMore." }));
        assert_eq!(input["fileChanges"]["additions"], json!([{ "path": "a/b.bin", "contents": "AAH/" }]));
        assert_eq!(input["fileChanges"]["deletions"], json!([{ "path": "old.txt" }]));
        let bare = commit_request("acme/app", "quest-1", "abc", "chore: only a headline", &staged);
        assert!(bare["variables"]["input"]["message"].get("body").is_none());
    }

    #[test]
    fn graphql_answers_are_read_for_the_oid_or_the_refusal() {
        assert_eq!(commit_oid(br#"{"data":{"createCommitOnBranch":{"commit":{"oid":"deadbeef"}}}}"#).unwrap(), "deadbeef");
        let refused = commit_oid(br#"{"data":null,"errors":[{"message":"Expected branch to point to abc"}]}"#).unwrap_err();
        assert!(refused.to_string().contains("Expected branch to point to abc"));
        assert!(commit_oid(b"<html>").is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn live_api_publish_makes_verified_commits() {
        let repo = std::env::var("LIVE_API_REPO").unwrap_or_else(|_| "Warlord-K/loka".to_string());
        let branch = format!("loka-api-live-{}", &Uuid::new_v4().to_string()[..8]);
        let local = fresh_dir("live");
        sh(&local.0, &["init", "-q", "--initial-branch=main"]).await;
        sh(&local.0, &["remote", "add", "origin", &format!("https://github.com/{repo}.git")]).await;
        sh(&local.0, &["fetch", "-q", "--depth=1", "origin", "main"]).await;
        sh(&local.0, &["update-ref", "refs/remotes/origin/main", "FETCH_HEAD"]).await;
        sh(&local.0, &["checkout", "-q", "-b", &branch, "origin/main"]).await;
        assert!(available(&local.0, &repo).await);
        std::fs::write(local.0.join("api-live.txt"), "one\n").unwrap();
        std::fs::write(local.0.join("api-live.bin"), [0u8, 159, 146, 150, 255]).unwrap();
        sh(&local.0, &["add", "-A"]).await;
        sh(&local.0, &["commit", "-q", "-m", "chore: live api publish check\n\nbody"]).await;
        std::fs::write(local.0.join("api-live.txt"), "two\n").unwrap();
        sh(&local.0, &["commit", "-q", "-am", "chore: second live commit"]).await;
        let result = replay(&local.0, &repo, "main", &branch, &branch).await;
        let tip = sh(&local.0, &["rev-parse", "HEAD"]).await;
        let path = format!("repos/{repo}/commits/{tip}");
        let seen = tokio::process::Command::new("gh")
            .args(["api", &path, "--jq", "[.commit.verification.verified, .commit.author.name, (.parents|length)] | @tsv"])
            .output()
            .await
            .unwrap();
        let parent = sh(&local.0, &["rev-parse", "HEAD~1"]).await;
        let parent_path = format!("repos/{repo}/commits/{parent}");
        let parent_seen = tokio::process::Command::new("gh")
            .args(["api", &parent_path, "--jq", ".commit.verification.verified"])
            .output()
            .await
            .unwrap();
        let _ = tokio::process::Command::new("gh")
            .args(["api", "-X", "DELETE", &format!("repos/{repo}/git/refs/heads/{branch}")])
            .output()
            .await;
        assert_eq!(result.unwrap(), Replay::Published { commits: 2 });
        let seen = String::from_utf8_lossy(&seen.stdout).to_string();
        assert!(seen.starts_with("true\t"), "{seen}");
        assert_eq!(String::from_utf8_lossy(&parent_seen.stdout).trim(), "true");
    }
}
