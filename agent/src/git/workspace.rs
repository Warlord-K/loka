use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{info, warn};

use crate::cli::{job_command, workspace_command};
use loka_shared::git_names::{github_repo, validate_git_ref};

pub async fn configure_git_auth(github_token: &str) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let git_dir = Path::new(&home);

    if !github_token.is_empty() {
        let cred_path = PathBuf::from("/tmp/.git-credentials");
        tokio::fs::write(
            &cred_path,
            format!("https://x-access-token:{github_token}@github.com\n"),
        )
        .await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600)).await?;
        }

        let _ = job_command("git")
            .args([
                "config",
                "--global",
                "credential.helper",
                &format!("store --file={}", cred_path.display()),
            ])
            .current_dir(git_dir)
            .output()
            .await;
    }

    Ok(())
}

pub async fn configure_git_signing(signing: Option<&crate::config::SigningConfig>) -> Result<()> {
    let signing = match signing {
        Some(signing) => signing,
        None => return Ok(()),
    };

    let key_path = &signing.ssh_key_path;

    if !key_path.exists() {
        anyhow::bail!(
            "SSH signing key configured at {} does not exist; fix signing.ssh_key_path or remove the signing configuration",
            key_path.display()
        );
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let local_key = PathBuf::from(home).join(".ssh/signing_key_active");
    tokio::fs::create_dir_all(local_key.parent().expect("signing key has a parent")).await?;
    if key_path != &local_key {
        tokio::fs::copy(key_path, &local_key)
            .await
            .with_context(|| format!("Failed to copy signing key from {}", key_path.display()))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&local_key, std::fs::Permissions::from_mode(0o600)).await?;
    }

    let key_str = local_key.to_string_lossy();

    for (key, value) in [
        ("gpg.format", "ssh"),
        ("user.signingkey", &*key_str),
        ("commit.gpgsign", "true"),
        ("tag.gpgsign", "true"),
    ] {
        let output = job_command("git")
            .args(["config", "--global", key, value])
            .output()
            .await
            .with_context(|| format!("Failed to set git config {key}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git config --global {key} {value} failed: {stderr}");
        }
    }

    if let Some(signers_path) = &signing.allowed_signers_path {
        if signers_path.exists() {
            let _ = job_command("git")
                .args([
                    "config",
                    "--global",
                    "gpg.ssh.allowedSignersFile",
                    &signers_path.to_string_lossy(),
                ])
                .output()
                .await;
        }
    }

    info!(key = %key_path.display(), "SSH commit signing configured");
    Ok(())
}

const FALLBACK_AUTHOR_NAME: &str = "Loka Agent";
const FALLBACK_AUTHOR_EMAIL: &str = "agent@loka.ygpt.dev";

pub async fn ensure_repo_identity(repo_path: &Path, github_token: &str) -> Result<()> {
    let have_name = !git_config_get(repo_path, "user.name").await.is_empty();
    let have_email = !git_config_get(repo_path, "user.email").await.is_empty();
    if have_name && have_email {
        return Ok(());
    }

    let gh = github_identity(github_token).await;

    if !have_name {
        let name = std::env::var("GIT_AUTHOR_NAME").ok().filter(|v| !v.is_empty());
        let name = name
            .or_else(|| gh.as_ref().map(|(name, _)| name.clone()))
            .unwrap_or_else(|| FALLBACK_AUTHOR_NAME.to_string());
        run_git(repo_path, &["config", "user.name", &name]).await?;
        info!(repo = %repo_path.display(), "Set repo-local git user.name");
    }
    if !have_email {
        let email = std::env::var("GIT_AUTHOR_EMAIL").ok().filter(|v| !v.is_empty());
        let email = email
            .or_else(|| gh.as_ref().map(|(_, email)| email.clone()))
            .unwrap_or_else(|| FALLBACK_AUTHOR_EMAIL.to_string());
        run_git(repo_path, &["config", "user.email", &email]).await?;
        info!(repo = %repo_path.display(), "Set repo-local git user.email");
    }
    Ok(())
}

async fn git_config_get(repo_path: &Path, key: &str) -> String {
    run_git_output(repo_path, &["config", key])
        .await
        .unwrap_or_default()
        .trim()
        .to_string()
}

async fn github_identity(github_token: &str) -> Option<(String, String)> {
    let mut cmd = Command::new("gh");
    cmd.args(["api", "user"]);
    if !github_token.is_empty() {
        cmd.env("GH_TOKEN", github_token);
    }
    let output = match tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await {
        Ok(Ok(output)) => output,
        _ => return None,
    };
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let login = json.get("login").and_then(|v| v.as_str())?;
    if login.is_empty() {
        return None;
    }
    let name = json
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(login)
        .to_string();
    let email = json
        .get("email")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{login}@users.noreply.github.com"));
    Some((name, email))
}

const COMMIT_MSG_HOOK: &str = r#"#!/bin/sh
msg=$1
filtered=$(mktemp "${TMPDIR:-/tmp}/loka-msg.XXXXXX") || exit 0
grep -vi '^[[:space:]]*co-authored-by:' "$msg" > "$filtered" || true
if [ -s "$filtered" ]; then
  cat "$filtered" > "$msg"
fi
rm -f "$filtered"
exit 0
"#;

pub async fn ensure_commit_hygiene(repo_path: &Path) -> Result<()> {
    let common_dir = run_git_checked_output(
        repo_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let hooks_dir = Path::new(common_dir.trim()).join("loka-hooks");
    let output = job_command("sh")
        .args([
            "-c",
            r#"mkdir -p "$1" && printf %s "$2" > "$1/.commit-msg.$$" && chmod 0755 "$1/.commit-msg.$$" && mv -f "$1/.commit-msg.$$" "$1/commit-msg""#,
            "sh",
        ])
        .arg(&hooks_dir)
        .arg(COMMIT_MSG_HOOK)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "failed to install the commit-msg hook in {}: {}",
            hooks_dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    run_git(
        repo_path,
        &["config", "core.hooksPath", &hooks_dir.to_string_lossy()],
    )
    .await?;
    Ok(())
}

pub async fn ensure_cloned(
    repos_dir: &Path,
    repo_url: &str,
    github_token: &str,
) -> Result<PathBuf> {
    ensure_cloned_at(repos_dir, repo_url, github_token, None).await
}

const FETCH_FRESH: std::time::Duration = std::time::Duration::from_secs(180);

fn recent_fetches() -> &'static std::sync::Mutex<std::collections::HashMap<(PathBuf, String), std::time::Instant>> {
    static FETCHES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(PathBuf, String), std::time::Instant>>> =
        std::sync::OnceLock::new();
    FETCHES.get_or_init(Default::default)
}

pub fn fetched_recently(repo_path: &Path, base: &str) -> bool {
    recent_fetches()
        .lock()
        .unwrap()
        .get(&(repo_path.to_path_buf(), base.to_string()))
        .is_some_and(|at| at.elapsed() < FETCH_FRESH)
}

pub fn mark_fetched(repo_path: &Path, base: &str) {
    recent_fetches()
        .lock()
        .unwrap()
        .insert((repo_path.to_path_buf(), base.to_string()), std::time::Instant::now());
}

pub async fn fetch_base(repo_path: &Path, base: &str) -> Result<()> {
    validate_git_ref(base)?;
    if fetched_recently(repo_path, base) {
        return Ok(());
    }
    run_git(repo_path, &["fetch", "origin", base]).await?;
    mark_fetched(repo_path, base);
    Ok(())
}

pub async fn ensure_cloned_at(
    repos_dir: &Path,
    repo_url: &str,
    github_token: &str,
    base: Option<&str>,
) -> Result<PathBuf> {
    if github_token.is_empty() {
        bail!("GITHUB_TOKEN is not set, so GitHub repos are disabled");
    }
    let repo = github_repo(repo_url)?;
    let repo_name = repo.rsplit('/').next().unwrap_or(&repo).to_string();
    let repo_name = repo_name.as_str();

    let clone_path = repos_dir.join(repo_name);

    if clone_path.exists() {
        match base {
            Some(base) => fetch_base(&clone_path, base).await?,
            None => {
                info!(repo = repo_name, "Repo already cloned, fetching");
                run_git(&clone_path, &["fetch", "origin"]).await?;
            }
        }
    } else {
        let auth_url = format!("https://github.com/{repo}.git");

        info!(repo = repo_name, "Cloning repo");
        tokio::fs::create_dir_all(repos_dir).await?;
        run_git_in(repos_dir, &["clone", "--", &auth_url, repo_name]).await?;
    }

    ensure_agents_md_symlink(&clone_path).await;

    if let Err(e) = ensure_repo_identity(&clone_path, github_token).await {
        warn!(repo = repo_name, error = %e, "Failed to set repo git identity");
    }
    if let Err(e) = ensure_commit_hygiene(&clone_path).await {
        warn!(repo = repo_name, error = %e, "Failed to install commit hook");
    }

    Ok(clone_path)
}

pub async fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
    base_branch: &str,
) -> Result<()> {
    if worktree_path.exists() {
        let current_branch =
            run_git_output(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
        if let Ok(branch) = current_branch {
            if branch.trim() == branch_name {
                info!(
                    worktree = %worktree_path.display(),
                    branch = branch_name,
                    "Reusing existing worktree"
                );
                ensure_agents_md_symlink(worktree_path).await;
                return Ok(());
            }
        }
        let _ = run_git(
            repo_path,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ],
        )
        .await;
        let _ = tokio::fs::remove_dir_all(worktree_path).await;
    }

    let _ = run_git(repo_path, &["branch", "-D", branch_name]).await;

    run_git(
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            branch_name,
            &worktree_path.to_string_lossy(),
            &format!("origin/{base_branch}"),
        ],
    )
    .await
    .context("Failed to create git worktree")?;

    ensure_agents_md_symlink(worktree_path).await;

    info!(
        worktree = %worktree_path.display(),
        branch = branch_name,
        "Created worktree"
    );

    Ok(())
}

const UNSIGNED_LEFTOVERS: &str = "Publish found uncommitted changes and commit.gpgsign is on, but no signing key could sign them, so it will not commit them unsigned. Commit them yourself with a signed commit, or set user.signingkey (or add ~/.ssh/loka_signing), then publish again.";

const FALLBACK_SIGNING_KEYS: [&str; 3] = [".ssh/loka_signing", ".ssh/karna_signing", ".ssh/signing_key_active"];

pub async fn commit_signed(worktree_path: &Path, message: &str) -> Result<bool> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    commit_signed_with_home(worktree_path, message, home.as_deref()).await
}

async fn commit_signed_with_home(worktree_path: &Path, message: &str, home: Option<&Path>) -> Result<bool> {
    let status = run_git_checked_output(worktree_path, &["status", "--porcelain"]).await?;
    if status.trim().is_empty() {
        return Ok(false);
    }
    let message = without_coauthors(message);
    let gpgsign = gpgsign_enabled(worktree_path).await?;
    let configured = !run_git_output(worktree_path, &["config", "--get", "user.signingkey"]).await?.trim().is_empty();
    let mut attempts: Vec<Vec<String>> = Vec::new();
    if configured || gpgsign {
        attempts.push(Vec::new());
    }
    let only_default_key = !configured && attempts.len() == 1;
    if let Some(home) = home {
        for key in FALLBACK_SIGNING_KEYS.iter().map(|key| home.join(key)).filter(|key| key.is_file()) {
            attempts.push(vec![
                "-c".to_string(),
                "gpg.format=ssh".to_string(),
                "-c".to_string(),
                format!("user.signingkey={}", key.display()),
            ]);
        }
    }
    let only_default_key = only_default_key && attempts.len() == 1;
    if attempts.is_empty() {
        run_git(worktree_path, &["add", "-A"]).await?;
        if let Err(error) = run_git(worktree_path, &["commit", "-m", &message]).await {
            unstage(worktree_path).await?;
            return Err(error);
        }
        return Ok(true);
    }
    let mut failures = Vec::new();
    for flags in attempts {
        let mut args = flags;
        args.extend(["commit", "-S", "-m"].map(String::from));
        args.push(message.clone());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_git(worktree_path, &["add", "-A"]).await?;
        match run_git(worktree_path, &args).await {
            Err(error) => {
                unstage(worktree_path).await?;
                failures.push(format!("{error:#}"));
            }
            Ok(()) if commit_is_signed(worktree_path, "HEAD").await? => return Ok(true),
            Ok(()) => {
                undo_head_commit(worktree_path).await?;
                failures.push("the commit came out without a signature".to_string());
            }
        }
    }
    if only_default_key {
        bail!("{UNSIGNED_LEFTOVERS} Details: {}", failures.join("; "));
    }
    bail!(
        "Publish could not sign the commit for the uncommitted changes, so it did not commit them: {}",
        failures.join("; ")
    )
}

async fn gpgsign_enabled(worktree_path: &Path) -> Result<bool> {
    let signing = workspace_command("git", worktree_path)
        .await
        .args(["config", "--bool", "--get", "commit.gpgsign"])
        .output()
        .await
        .context("Failed to read git commit signing configuration")?;
    if signing.status.code() == Some(1) {
        return Ok(false);
    }
    if !signing.status.success() {
        bail!("Could not read commit.gpgsign: {}", String::from_utf8_lossy(&signing.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&signing.stdout).trim() == "true")
}

async fn has_rev(worktree_path: &Path, rev: &str) -> bool {
    run_git(worktree_path, &["rev-parse", "--verify", "--quiet", rev]).await.is_ok()
}

async fn unstage(worktree_path: &Path) -> Result<()> {
    if has_rev(worktree_path, "HEAD").await {
        run_git(worktree_path, &["reset", "-q"]).await
    } else {
        run_git(worktree_path, &["read-tree", "--empty"]).await
    }
}

async fn undo_head_commit(worktree_path: &Path) -> Result<()> {
    if has_rev(worktree_path, "HEAD~1").await {
        run_git(worktree_path, &["reset", "-q", "--soft", "HEAD~1"]).await?;
    } else {
        run_git(worktree_path, &["update-ref", "-d", "HEAD"]).await?;
    }
    unstage(worktree_path).await
}

async fn commit_is_signed(worktree_path: &Path, rev: &str) -> Result<bool> {
    let commit = run_git_checked_output(worktree_path, &["cat-file", "commit", rev]).await?;
    Ok(has_signature(&commit))
}

fn has_signature(commit: &str) -> bool {
    commit
        .split_once("\n\n")
        .map(|(headers, _)| headers.lines().any(|line| line.starts_with("gpgsig ") || line.starts_with("gpgsig-sha256 ")))
        .unwrap_or(false)
}

pub async fn push_quest_branch(worktree_path: &Path, branch: &str, quest_branch: &str) -> Result<()> {
    if !is_quest_branch(branch, quest_branch) {
        bail!("Publish refused to push to {branch}: a quest only pushes its own branch ({quest_branch}). Its pull request opens from that branch instead.");
    }
    push(worktree_path, branch).await
}

pub async fn fetched_stack_parent(worktree_path: &Path, base_branch: &str, parent: Option<&str>) -> Option<String> {
    fetched_stack_parent_over(worktree_path, base_branch, parent, None).await
}

async fn fetched_stack_parent_over(
    worktree_path: &Path,
    base_branch: &str,
    parent: Option<&str>,
    protocols: Option<&str>,
) -> Option<String> {
    let parent = parent.map(str::trim).filter(|parent| !parent.is_empty() && *parent != base_branch)?;
    validate_git_ref(parent).ok()?;
    if let Err(problem) = fetch_origin(worktree_path, parent, protocols).await {
        warn!("Could not fetch the parent branch {parent}, opening the pull request into {base_branch}: {problem}");
        return None;
    }
    let parent_ref = format!("origin/{parent}");
    run_git(worktree_path, &["merge-base", "--is-ancestor", &parent_ref, "HEAD"]).await.ok()?;
    Some(parent.to_string())
}

fn without_coauthors(message: &str) -> String {
    message
        .lines()
        .filter(|line| !line.trim_start().to_ascii_lowercase().starts_with("co-authored-by:"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn push(worktree_path: &Path, branch: &str) -> Result<()> {
    let refspec = push_refspec(branch);
    let error = match run_git(worktree_path, &["push", "-u", "origin", &refspec]).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let stderr = error.to_string();
    if push_rejected(&stderr)
        && rejected_push_choice(worktree_path, branch).await == RejectedPush::AlreadyPublished
    {
        info!(branch, "Push rejected, but the remote branch already holds this exact tree, so it counts as published");
        return Ok(());
    }
    if rewritten_history(&stderr) {
        return run_git(worktree_path, &["push", "-u", "--force-with-lease", "origin", &refspec]).await;
    }
    Err(error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedPush {
    AlreadyPublished,
    Fail,
}

pub fn rejected_push(local_tree: &str, remote_tree: Option<&str>) -> RejectedPush {
    let local_tree = local_tree.trim();
    match remote_tree.map(str::trim) {
        Some(remote) if !local_tree.is_empty() && remote == local_tree => RejectedPush::AlreadyPublished,
        _ => RejectedPush::Fail,
    }
}

async fn rejected_push_choice(worktree_path: &Path, branch: &str) -> RejectedPush {
    let remote = format!("refs/heads/{}", branch.trim_start_matches("refs/heads/"));
    let fetched = run_git(worktree_path, &["fetch", "--no-tags", "--refmap=", "origin", &remote]).await;
    if let Err(error) = fetched {
        warn!(branch, "Could not fetch the remote branch after a rejected push: {error}");
        return RejectedPush::Fail;
    }
    let local = run_git_checked_output(worktree_path, &["rev-parse", "HEAD^{tree}"]).await;
    let remote = run_git_checked_output(worktree_path, &["rev-parse", "FETCH_HEAD^{tree}"]).await;
    match local {
        Ok(local) => rejected_push(&local, remote.ok().as_deref()),
        Err(_) => RejectedPush::Fail,
    }
}

fn push_rejected(stderr: &str) -> bool {
    ["[rejected]", "non-fast-forward", "fetch first", "stale info"]
        .iter()
        .any(|marker| stderr.contains(marker))
}

fn rewritten_history(stderr: &str) -> bool {
    stderr.contains("non-fast-forward")
}

fn push_refspec(branch: &str) -> String {
    format!("HEAD:refs/heads/{}", branch.trim_start_matches("refs/heads/"))
}

pub async fn current_branch(worktree_path: &Path) -> Result<String> {
    let branch =
        run_git_checked_output(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(branch.trim().to_string())
}

pub async fn publish_branch(worktree_path: &Path, recorded: Option<&str>) -> Result<String> {
    let branch = current_branch(worktree_path).await?;
    let recorded = recorded
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "HEAD");
    match recorded {
        Some(own) if branch == "HEAD" || !is_quest_branch(&branch, own) => Ok(own.to_string()),
        _ if branch != "HEAD" => Ok(branch),
        _ => Err(anyhow::anyhow!("workspace is on a detached HEAD and has no task branch")),
    }
}

pub fn legacy_branch(branch: &str) -> Option<&str> {
    let (stem, suffix) = branch.rsplit_once('-')?;
    (!branch.contains('/') && suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(stem)
}

pub fn is_branch_of(name: &str, stem: &str) -> bool {
    !stem.is_empty()
        && (name == stem
            || name.strip_prefix(stem).and_then(|rest| rest.strip_prefix('-')).is_some_and(|n| {
                (1..=3).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit())
            }))
}

pub fn is_quest_branch(name: &str, branch: &str) -> bool {
    let name = name.trim().trim_start_matches("refs/heads/");
    let branch = branch.trim().trim_start_matches("refs/heads/");
    std::iter::once(branch)
        .chain(legacy_branch(branch))
        .any(|stem| is_branch_of(name, stem))
}

pub async fn unused_branch_name(repo_path: &Path, branch: &str) -> String {
    let mut name = branch.to_string();
    let mut attempt = 1;
    while run_git(repo_path, &["show-ref", "--quiet", &name])
        .await
        .is_ok()
    {
        attempt += 1;
        name = format!("{branch}-{attempt}");
    }
    name
}

pub async fn ensure_signed_commits(worktree_path: &Path, base_branch: &str) -> Result<()> {
    if !gpgsign_enabled(worktree_path).await? {
        return Ok(());
    }

    let base_ref = format!("origin/{base_branch}");
    let commits =
        run_git_checked_output(worktree_path, &["rev-list", &format!("{base_ref}..HEAD")]).await?;
    for sha in commits.lines().filter(|sha| !sha.trim().is_empty()) {
        if !commit_is_signed(worktree_path, sha).await? {
            anyhow::bail!(
                "Commit {sha} is unsigned while commit.gpgsign is enabled. Sign the commit using the configured key before retrying"
            );
        }
    }
    Ok(())
}

async fn refresh_base_over(worktree_path: &Path, base: &str, protocols: Option<&str>) -> Result<()> {
    validate_git_ref(base).with_context(|| format!("Not publishing against {base}, it is not a usable base branch"))?;
    let mut retried = false;
    loop {
        let problem = match fetch_origin(worktree_path, base, protocols).await {
            Ok(()) => return Ok(()),
            Err(problem) => problem,
        };
        if !retried && is_lock_error(&problem) {
            retried = true;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        }
        bail!(
            "Could not fetch origin/{base} before publishing, so Loka did not decide against an old copy of {base}. Details: {problem}"
        );
    }
}

pub async fn ahead_of_fresh_base(worktree_path: &Path, base: &str) -> Result<bool> {
    ahead_of_fresh_base_over(worktree_path, base, None).await
}

async fn ahead_of_fresh_base_over(worktree_path: &Path, base: &str, protocols: Option<&str>) -> Result<bool> {
    refresh_base_over(worktree_path, base, protocols).await?;
    has_commits_ahead(worktree_path, base).await
}

async fn fetch_origin(worktree_path: &Path, base: &str, protocols: Option<&str>) -> std::result::Result<(), String> {
    let mut command = workspace_command("git", worktree_path).await;
    if let Some(protocols) = protocols {
        command.env("GIT_ALLOW_PROTOCOL", protocols);
    }
    let output = command
        .args(["fetch", "origin", base])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("git fetch could not start: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn is_lock_error(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("cannot lock ref") || text.contains(".lock': file exists") || text.contains("unable to create") && text.contains(".lock")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishChoice {
    Create,
    NoSharedHistory,
    UnrelatedWithWork,
    NothingOwn,
    AlreadyInBase,
}

impl PublishChoice {
    pub fn reason(self, base_branch: &str) -> String {
        match self {
            Self::Create => format!("The branch has work of its own on top of {base_branch}"),
            Self::NoSharedHistory => format!("The quest branch shares no history with {base_branch}, so there is no pull request to open"),
            Self::UnrelatedWithWork => loka_shared::run_causes::unrelated_history_error(base_branch),
            Self::NothingOwn => format!("The quest branch has no commits of its own on top of {base_branch}, so there is no pull request to open"),
            Self::AlreadyInBase => format!("Everything on the quest branch is already in {base_branch}, so there is no pull request to open"),
        }
    }
}

pub fn publish_choice(shares_history: bool, own_commits: u64) -> PublishChoice {
    if !shares_history && own_commits == 0 {
        PublishChoice::NoSharedHistory
    } else if !shares_history {
        PublishChoice::UnrelatedWithWork
    } else if own_commits == 0 {
        PublishChoice::NothingOwn
    } else {
        PublishChoice::Create
    }
}

pub fn commits_after_base_tree(head_trees: &[&str], base_trees: &std::collections::HashSet<&str>) -> u64 {
    head_trees.iter().position(|tree| base_trees.contains(tree)).unwrap_or(head_trees.len()) as u64
}

pub async fn branch_publish_choice(worktree_path: &Path, base_branch: &str) -> Result<PublishChoice> {
    let base_ref = format!("origin/{base_branch}");
    let base_known = workspace_command("git", worktree_path)
        .await
        .args(["rev-parse", "--verify", "--quiet", &format!("{base_ref}^{{commit}}")])
        .output()
        .await
        .context("Failed to run git rev-parse")?;
    if !base_known.status.success() {
        return Ok(PublishChoice::NothingOwn);
    }
    let merge_base = workspace_command("git", worktree_path)
        .await
        .args(["merge-base", &base_ref, "HEAD"])
        .output()
        .await
        .context("Failed to run git merge-base")?;
    let shares_history = merge_base.status.success() && !merge_base.stdout.trim_ascii().is_empty();
    if !shares_history {
        let head_trees = run_git_output(worktree_path, &["log", "--format=%T", "HEAD"]).await?;
        let base_trees = run_git_output(worktree_path, &["log", "--format=%T", &base_ref]).await?;
        let head: Vec<&str> = head_trees.lines().map(str::trim).filter(|tree| !tree.is_empty()).collect();
        let base: std::collections::HashSet<&str> = base_trees.lines().map(str::trim).collect();
        return Ok(publish_choice(false, commits_after_base_tree(&head, &base)));
    }
    let output = run_git_output(
        worktree_path,
        &["rev-list", "--count", &format!("{base_ref}..HEAD")],
    )
    .await?;
    let own_commits: u64 = output.trim().parse().unwrap_or(0);
    let choice = publish_choice(shares_history, own_commits);
    if choice != PublishChoice::Create {
        return Ok(choice);
    }
    let base_tree = tree_of(worktree_path, &base_ref).await?;
    if base_tree.is_empty() {
        return Ok(choice);
    }
    if tree_of(worktree_path, "HEAD").await? == base_tree {
        return Ok(PublishChoice::AlreadyInBase);
    }
    match merged_tree(worktree_path, &base_ref).await? {
        Some(tree) if tree == base_tree => return Ok(PublishChoice::AlreadyInBase),
        Some(_) => {}
        None => {
            let merge_base = String::from_utf8_lossy(&merge_base.stdout).trim().to_string();
            if changes_already_in_base(worktree_path, &base_ref, &merge_base).await {
                return Ok(PublishChoice::AlreadyInBase);
            }
        }
    }
    Ok(choice)
}

const CONTAINED_FILES_MAX: usize = 500;

async fn changes_already_in_base(worktree_path: &Path, base_ref: &str, merge_base: &str) -> bool {
    let Ok(changed) =
        run_git_checked_output(worktree_path, &["diff", "--name-only", "--no-renames", "-z", merge_base, "HEAD"]).await
    else {
        return false;
    };
    let files: Vec<&str> = changed.split('\0').filter(|file| !file.is_empty()).collect();
    if files.is_empty() || files.len() > CONTAINED_FILES_MAX {
        return false;
    }
    let mut args = vec!["--literal-pathspecs", "ls-tree", "-r", "-z", "HEAD", "--"];
    args.extend(files.iter().copied());
    let Ok(listing) = run_git_checked_output(worktree_path, &args).await else {
        return false;
    };
    let mut wanted: std::collections::HashMap<&str, String> =
        files.iter().map(|file| (*file, DELETED_VERSION.to_string())).collect();
    for entry in listing.split('\0') {
        let Some((meta, path)) = entry.split_once('\t') else { continue };
        let parts: Vec<&str> = meta.split(' ').collect();
        if let (Some(mode), Some(object), Some(slot)) = (parts.first(), parts.get(2), wanted.get_mut(path)) {
            *slot = format!("{mode} {object}");
        }
    }
    let range = format!("{merge_base}..{base_ref}");
    let mut args = vec![
        "--literal-pathspecs",
        "log",
        "-m",
        "--raw",
        "--no-abbrev",
        "--no-renames",
        "--format=",
        "-z",
        &range,
        "--",
    ];
    args.extend(files.iter().copied());
    let Ok(history) = run_git_checked_output(worktree_path, &args).await else {
        return false;
    };
    let seen = versions_in_raw_log(&history);
    wanted.iter().all(|(path, version)| seen.contains(&(path.to_string(), version.clone())))
}

const DELETED_VERSION: &str = "deleted";

fn versions_in_raw_log(raw: &str) -> std::collections::HashSet<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut tokens = raw.split('\0');
    while let Some(token) = tokens.next() {
        let Some(meta) = token.trim_start_matches('\n').strip_prefix(':') else { continue };
        let Some(path) = tokens.next() else { break };
        let fields: Vec<&str> = meta.split(' ').collect();
        let (Some(mode), Some(object), Some(status)) = (fields.get(1), fields.get(3), fields.get(4)) else {
            continue;
        };
        let version = if status.starts_with('D') {
            DELETED_VERSION.to_string()
        } else {
            format!("{mode} {object}")
        };
        seen.insert((path.to_string(), version));
    }
    seen
}

async fn tree_of(worktree_path: &Path, rev: &str) -> Result<String> {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{tree}}")])
        .output()
        .await
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn merged_tree(worktree_path: &Path, base_ref: &str) -> Result<Option<String>> {
    let output = workspace_command("git", worktree_path)
        .await
        .args(["merge-tree", "--write-tree", base_ref, "HEAD"])
        .output()
        .await
        .context("Failed to run git merge-tree")?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree = stdout.lines().next().unwrap_or("").trim();
    Ok((!tree.is_empty()).then(|| tree.to_string()))
}

pub async fn has_commits_ahead(worktree_path: &Path, base_branch: &str) -> Result<bool> {
    let choice = branch_publish_choice(worktree_path, base_branch).await?;
    if choice == PublishChoice::UnrelatedWithWork {
        bail!(choice.reason(base_branch));
    }
    if choice != PublishChoice::Create {
        info!(base = base_branch, ?choice, "Skipping the pull request: {}", choice.reason(base_branch));
        return Ok(false);
    }
    Ok(true)
}

pub async fn work_summary(worktree_path: &Path, base_branch: &str) -> Option<String> {
    let base_ref = format!("origin/{base_branch}");
    let commits = run_git_output(worktree_path, &["log", "--oneline", "-n", "10", &format!("{base_ref}..HEAD")])
        .await
        .ok()?;
    let stat = run_git_output(worktree_path, &["diff", "--no-ext-diff", "--no-textconv", "--stat", "HEAD"]).await.ok()?;
    let mut parts = Vec::new();
    if !commits.trim().is_empty() {
        parts.push(format!("Commits ahead of {base_branch}:\n{}", commits.trim()));
    }
    let stat: Vec<&str> = stat.lines().filter(|line| !line.trim().is_empty()).collect();
    if !stat.is_empty() {
        let shown = stat.iter().rev().take(12).rev().copied().collect::<Vec<_>>().join("\n");
        parts.push(format!("Uncommitted changes:\n{shown}"));
    }
    if parts.is_empty() {
        return Some("No commits or changes yet.".to_string());
    }
    Some(parts.join("\n\n"))
}

pub async fn ensure_agents_md_symlink(repo_path: &Path) {
    let claude_md = repo_path.join("CLAUDE.md");
    let agents_md = repo_path.join("AGENTS.md");

    if claude_md.is_file() && !agents_md.exists() {
        if let Err(e) = tokio::fs::symlink("CLAUDE.md", &agents_md).await {
            tracing::debug!(
                path = %repo_path.display(),
                error = %e,
                "Failed to create AGENTS.md symlink"
            );
        }
    }
}

async fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let output = workspace_command("git", dir)
        .await
        .args(args)
        .output()
        .await
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr);
    }
    Ok(())
}

async fn run_git_in(dir: &Path, args: &[&str]) -> Result<()> {
    run_git(dir, args).await
}

async fn run_git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = workspace_command("git", dir)
        .await
        .args(args)
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn run_git_checked_output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = workspace_command("git", dir)
        .await
        .args(args)
        .output()
        .await
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SigningConfig;
    use uuid::Uuid;

    #[test]
    fn runtime_git_ignores_repo_hooks_and_fsmonitor() {
        let command = job_command("git");
        let args: Vec<String> = command.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["-c", "core.hooksPath=/dev/null"]));
        assert!(args.windows(2).any(|w| w == ["-c", "core.fsmonitor=false"]));
    }

    #[test]
    fn runtime_commits_drop_coauthor_trailers() {
        assert_eq!(without_coauthors("fix: x\n\nCo-Authored-By: A <a@b>\nkeep"), "fix: x\n\nkeep");
    }

    #[test]
    fn push_retries_with_a_lease_only_after_a_rebase() {
        assert!(rewritten_history(" ! [rejected]        HEAD -> work (non-fast-forward)"));
        assert!(!rewritten_history(" ! [rejected]        HEAD -> work (fetch first)"));
        assert!(!rewritten_history("remote: Repository not found."));
    }

    struct TestRepo(PathBuf);

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn test_repo() -> Result<TestRepo> {
        let repo = TestRepo(std::env::temp_dir().join(format!("loka-signing-{}", Uuid::new_v4())));
        tokio::fs::create_dir_all(&repo.0).await?;
        run_git(&repo.0, &["init", "--initial-branch=main"]).await?;
        run_git(&repo.0, &["config", "user.name", "Loka Test"]).await?;
        run_git(&repo.0, &["config", "user.email", "test@loka.ygpt.dev"]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "false"]).await?;
        tokio::fs::write(repo.0.join("base"), "base").await?;
        run_git(&repo.0, &["add", "base"]).await?;
        run_git(&repo.0, &["commit", "-m", "base"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "HEAD"]).await?;
        run_git(&repo.0, &["switch", "-c", "task"]).await?;
        Ok(repo)
    }

    async fn commit(repo: &Path, name: &str) -> Result<()> {
        tokio::fs::write(repo.join(name), name).await?;
        run_git(repo, &["add", name]).await?;
        run_git(repo, &["commit", "-m", name]).await
    }

    #[test]
    fn only_the_quest_branch_counts_as_its_own() {
        let quest = "fix-quests-get-an-unrelated-dcec4a9d";
        assert!(is_quest_branch(quest, quest));
        assert!(is_quest_branch("fix-quests-get-an-unrelated-dcec4a9d-2", quest));
        assert!(is_quest_branch("refs/heads/fix-quests-get-an-unrelated-dcec4a9d", quest));
        assert!(!is_quest_branch("merge-pr-90-workshop-dark-93052e28", quest));
        assert!(!is_quest_branch("pr84", quest));
        assert!(!is_quest_branch("fix-quests-get-an-unrelated-dcec4a9d-extra", quest));
        assert!(!is_quest_branch("pr84", ""));
    }

    #[test]
    fn only_ref_lock_failures_are_retried() {
        assert!(is_lock_error("error: cannot lock ref 'refs/remotes/origin/main': is at abc but expected def"));
        assert!(is_lock_error("fatal: Unable to create '/r/.git/refs/remotes/origin/main.lock': File exists."));
        assert!(!is_lock_error("fatal: couldn't find remote ref main"));
        assert!(!is_lock_error("fatal: 'origin' does not appear to be a git repository"));
    }

    const TEST_PROTOCOLS: &str = "file:https:ssh";

    async fn test_git(dir: &Path, args: &[&str]) -> Result<()> {
        let output = workspace_command("git", dir)
            .await
            .env("GIT_ALLOW_PROTOCOL", TEST_PROTOCOLS)
            .args(args)
            .output()
            .await?;
        if !output.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    }

    async fn repo_with_remote() -> Result<(TestRepo, TestRepo)> {
        let remote = TestRepo(std::env::temp_dir().join(format!("loka-remote-{}", Uuid::new_v4())));
        tokio::fs::create_dir_all(&remote.0).await?;
        let output = Command::new("git")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_DIR")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&remote.0)
            .output()
            .await?;
        if !output.status.success() {
            bail!("git init --bare failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let repo = test_repo().await?;
        let url = format!("file://{}", remote.0.display());
        run_git(&repo.0, &["remote", "add", "origin", &url]).await?;
        test_git(&repo.0, &["push", "origin", "main:main"]).await?;
        Ok((remote, repo))
    }

    #[tokio::test]
    async fn refresh_base_sees_a_squash_merge_on_the_remote() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "feature").await?;
        run_git(&repo.0, &["switch", "main"]).await?;
        tokio::fs::write(repo.0.join("feature"), "feature").await?;
        run_git(&repo.0, &["add", "feature"]).await?;
        run_git(&repo.0, &["commit", "-m", "feature (#90)"]).await?;
        test_git(&repo.0, &["push", "origin", "main:main"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "main~1"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        assert!(has_commits_ahead(&repo.0, "main").await?);

        refresh_base_over(&repo.0, "main", Some(TEST_PROTOCOLS)).await?;
        assert!(!has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    #[tokio::test]
    async fn publishing_decides_against_a_fresh_base_not_the_last_fetch() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "feature").await?;
        commit(&repo.0, "fixup").await?;
        run_git(&repo.0, &["switch", "main"]).await?;
        for name in ["feature", "fixup"] {
            tokio::fs::write(repo.0.join(name), name).await?;
            run_git(&repo.0, &["add", name]).await?;
        }
        run_git(&repo.0, &["commit", "-m", "work (#112)"]).await?;
        test_git(&repo.0, &["push", "origin", "main:main"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "main~1"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        assert!(has_commits_ahead(&repo.0, "main").await?, "the last fetched main predates the squash");

        assert!(!ahead_of_fresh_base_over(&repo.0, "main", Some(TEST_PROTOCOLS)).await?);
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_base_fetch_refuses_to_publish() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "feature").await?;
        let renamed = ahead_of_fresh_base_over(&repo.0, "renamed-base", Some(TEST_PROTOCOLS)).await;
        assert!(renamed.unwrap_err().to_string().contains("Could not fetch origin/renamed-base before publishing"));
        assert!(refresh_base_over(&repo.0, "bad..ref", Some(TEST_PROTOCOLS)).await.is_err());
        run_git(&repo.0, &["remote", "set-url", "origin", "file:///nonexistent/loka-remote"]).await?;
        assert!(ahead_of_fresh_base_over(&repo.0, "main", Some(TEST_PROTOCOLS)).await.is_err());
        assert!(ahead_of_fresh_base(&repo.0, "main").await.is_err());
        assert!(has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    #[tokio::test]
    async fn publish_uses_the_quest_branch_not_a_checked_out_pr_branch() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["switch", "-c", "pr84"]).await?;
        assert_eq!(publish_branch(&repo.0, Some("fix-x-dcec4a9d")).await?, "fix-x-dcec4a9d");
        run_git(&repo.0, &["switch", "-c", "fix-x-dcec4a9d-2"]).await?;
        assert_eq!(publish_branch(&repo.0, Some("fix-x-dcec4a9d")).await?, "fix-x-dcec4a9d-2");
        Ok(())
    }

    #[test]
    fn a_rejected_push_counts_as_published_only_with_the_same_tree() {
        assert_eq!(rejected_push("abc123\n", Some("abc123")), RejectedPush::AlreadyPublished);
        assert_eq!(rejected_push("abc123", Some("def456")), RejectedPush::Fail);
        assert_eq!(rejected_push("abc123", None), RejectedPush::Fail);
        assert_eq!(rejected_push("", Some("")), RejectedPush::Fail);
    }

    #[test]
    fn push_rejections_are_recognised() {
        assert!(push_rejected(" ! [rejected]        HEAD -> fix-x (fetch first)\nerror: failed to push some refs"));
        assert!(push_rejected(" ! [rejected]        HEAD -> fix-x (non-fast-forward)"));
        assert!(push_rejected(" ! [rejected]        HEAD -> fix-x (stale info)"));
        assert!(!push_rejected("fatal: could not read Username for 'https://github.com'"));
        assert!(!push_rejected("remote: Permission to o/r.git denied"));
    }

    #[tokio::test]
    async fn a_remote_head_with_the_same_tree_is_already_published() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        let local = run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?;
        run_git(&repo.0, &["commit", "--allow-empty", "-m", "same tree, other commit"]).await?;
        let same = run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?;
        assert_eq!(rejected_push(&local, Some(&same)), RejectedPush::AlreadyPublished);
        commit(&repo.0, "more").await?;
        let other = run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?;
        assert_eq!(rejected_push(&local, Some(&other)), RejectedPush::Fail);
        Ok(())
    }

    #[test]
    fn publish_choice_skips_unrelated_and_empty_branches() {
        assert_eq!(publish_choice(false, 0), PublishChoice::NoSharedHistory);
        assert_eq!(publish_choice(false, 2), PublishChoice::UnrelatedWithWork);
        assert_eq!(publish_choice(true, 0), PublishChoice::NothingOwn);
        assert_eq!(publish_choice(true, 3), PublishChoice::Create);
    }

    #[tokio::test]
    async fn a_branch_cut_from_a_rewritten_base_has_nothing_to_publish() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "old-main").await?;
        run_git(&repo.0, &["switch", "--orphan", "rewritten"]).await?;
        tokio::fs::write(repo.0.join("base"), "base").await?;
        tokio::fs::write(repo.0.join("old-main"), "old-main").await?;
        run_git(&repo.0, &["add", "base", "old-main"]).await?;
        run_git(&repo.0, &["commit", "-m", "base rewritten"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "rewritten"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;

        let own = run_git_output(&repo.0, &["rev-list", "--count", "origin/main..HEAD"]).await?;
        assert_eq!(own.trim(), "2", "every old commit looks like the branch's own");
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::NoSharedHistory);
        assert!(!has_commits_ahead(&repo.0, "main").await?);

        commit(&repo.0, "feature").await?;
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::UnrelatedWithWork);
        let error = has_commits_ahead(&repo.0, "main").await.unwrap_err().to_string();
        assert!(error.contains("no history in common with main"), "{error}");
        let diagnosis = loka_shared::run_causes::diagnose(&loka_shared::run_causes::RunEnd { kind: "publish", error: Some(&error), output: "", turns: 0, timed_out: false });
        assert!(!diagnosis.transient(), "{diagnosis:?}");
        Ok(())
    }

    #[test]
    fn own_commits_of_an_unrelated_branch_stop_at_a_tree_the_base_has() {
        let base: std::collections::HashSet<&str> = ["t-base", "t-old"].into_iter().collect();
        assert_eq!(commits_after_base_tree(&["t-old", "t-1"], &base), 0);
        assert_eq!(commits_after_base_tree(&["t-feature", "t-old", "t-1"], &base), 1);
        assert_eq!(commits_after_base_tree(&["t-a", "t-b"], &base), 2);
    }

    #[tokio::test]
    async fn publish_choice_follows_the_branch_against_its_base() -> Result<()> {
        let repo = test_repo().await?;
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::NothingOwn);
        commit(&repo.0, "feature").await?;
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::Create);
        assert_eq!(branch_publish_choice(&repo.0, "missing-base").await?, PublishChoice::NothingOwn);
        Ok(())
    }

    #[tokio::test]
    async fn a_branch_without_new_commits_has_nothing_to_publish() -> Result<()> {
        let repo = test_repo().await?;
        assert!(!has_commits_ahead(&repo.0, "main").await?);
        commit(&repo.0, "feature").await?;
        assert!(has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    #[tokio::test]
    async fn squash_merged_work_has_nothing_to_publish_once_the_base_is_fresh() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        assert!(has_commits_ahead(&repo.0, "main").await?);

        run_git(&repo.0, &["switch", "main"]).await?;
        tokio::fs::write(repo.0.join("feature"), "feature").await?;
        run_git(&repo.0, &["add", "feature"]).await?;
        run_git(&repo.0, &["commit", "-m", "feature (#90)"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        assert!(has_commits_ahead(&repo.0, "main").await?, "a stale base still looks like new work");

        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "main"]).await?;
        let ahead = run_git_output(&repo.0, &["rev-list", "--count", "origin/main..HEAD"]).await?;
        assert_eq!(ahead.trim(), "1");
        assert!(!has_commits_ahead(&repo.0, "main").await?);

        commit(&repo.0, "more").await?;
        assert!(has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    async fn squash_onto_main(repo: &Path, files: &[&str], later: Option<&str>) -> Result<()> {
        run_git(repo, &["switch", "main"]).await?;
        for name in files {
            tokio::fs::write(repo.join(name), name).await?;
            run_git(repo, &["add", name]).await?;
        }
        run_git(repo, &["commit", "-m", "squashed (#112)"]).await?;
        if let Some(name) = later {
            commit(repo, name).await?;
        }
        run_git(repo, &["update-ref", "refs/remotes/origin/main", "main"]).await?;
        run_git(repo, &["switch", "task"]).await
    }

    #[tokio::test]
    async fn a_merged_in_branch_whose_tree_equals_main_is_already_in_main() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        commit(&repo.0, "fixup").await?;
        squash_onto_main(&repo.0, &["feature", "fixup"], Some("later")).await?;
        run_git(&repo.0, &["merge", "--no-edit", "-m", "merge: origin/main into the PR", "origin/main"]).await?;

        assert_eq!(tree_of(&repo.0, "HEAD").await?, tree_of(&repo.0, "origin/main").await?);
        let own = run_git_output(&repo.0, &["rev-list", "--count", "origin/main..HEAD"]).await?;
        assert_eq!(own.trim(), "3", "the branch still has commits of its own");
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::AlreadyInBase);
        assert!(!has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    #[tokio::test]
    async fn squashed_work_stays_skipped_after_main_moves_on() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        squash_onto_main(&repo.0, &["feature"], Some("later")).await?;

        assert_ne!(tree_of(&repo.0, "HEAD").await?, tree_of(&repo.0, "origin/main").await?);
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::AlreadyInBase);
        assert!(!has_commits_ahead(&repo.0, "main").await?);
        Ok(())
    }

    #[tokio::test]
    async fn a_real_diff_on_top_of_squashed_work_still_publishes() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        squash_onto_main(&repo.0, &["feature"], Some("later")).await?;
        commit(&repo.0, "new-work").await?;

        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::Create);
        assert!(has_commits_ahead(&repo.0, "main").await?);

        run_git(&repo.0, &["merge", "--no-edit", "origin/main"]).await?;
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::Create);
        Ok(())
    }

    #[tokio::test]
    async fn squashed_work_main_changed_again_is_already_in_main() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "feature").await?;
        commit(&repo.0, "gone").await?;
        run_git(&repo.0, &["rm", "-q", "base"]).await?;
        run_git(&repo.0, &["commit", "-m", "drop base"]).await?;
        run_git(&repo.0, &["switch", "main"]).await?;
        tokio::fs::write(repo.0.join("feature"), "feature").await?;
        tokio::fs::write(repo.0.join("gone"), "gone").await?;
        run_git(&repo.0, &["add", "feature", "gone"]).await?;
        run_git(&repo.0, &["rm", "-q", "base"]).await?;
        run_git(&repo.0, &["commit", "-m", "squashed (#112)"]).await?;
        tokio::fs::write(repo.0.join("feature"), "feature, changed again").await?;
        run_git(&repo.0, &["rm", "-q", "gone"]).await?;
        run_git(&repo.0, &["commit", "-am", "follow-up on main"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "main"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;

        assert!(merged_tree(&repo.0, "origin/main").await?.is_none(), "merging main conflicts");
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::AlreadyInBase);
        assert!(!has_commits_ahead(&repo.0, "main").await?);

        tokio::fs::write(repo.0.join("feature"), "feature, my own take").await?;
        run_git(&repo.0, &["commit", "-am", "real work"]).await?;
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::Create);
        Ok(())
    }

    #[test]
    fn raw_log_versions_cover_changes_and_deletions() {
        let zero = "0".repeat(40);
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let raw = format!(
            ":000000 100644 {zero} {a} A\0new file\0\n:100644 000000 {b} {zero} D\0old\0:100644 100755 {a} {b} M\0tool\0"
        );
        let seen = versions_in_raw_log(&raw);
        assert!(seen.contains(&("new file".to_string(), format!("100644 {a}"))));
        assert!(seen.contains(&("old".to_string(), DELETED_VERSION.to_string())));
        assert!(seen.contains(&("tool".to_string(), format!("100755 {b}"))));
        assert_eq!(seen.len(), 3);
    }

    #[tokio::test]
    async fn a_conflicting_branch_still_publishes() -> Result<()> {
        let repo = test_repo().await?;
        tokio::fs::write(repo.0.join("base"), "ours").await?;
        run_git(&repo.0, &["commit", "-am", "ours"]).await?;
        run_git(&repo.0, &["switch", "main"]).await?;
        tokio::fs::write(repo.0.join("base"), "theirs").await?;
        run_git(&repo.0, &["commit", "-am", "theirs"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "main"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;

        assert_eq!(merged_tree(&repo.0, "origin/main").await?, None);
        assert_eq!(branch_publish_choice(&repo.0, "main").await?, PublishChoice::Create);
        Ok(())
    }

    #[tokio::test]
    async fn required_signing_rejects_unsigned_commits() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "unsigned").await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;

        let error = ensure_signed_commits(&repo.0, "main").await.unwrap_err();
        assert!(error.to_string().contains("is unsigned"));
        Ok(())
    }

    #[tokio::test]
    async fn required_signing_accepts_ssh_signed_commits() -> Result<()> {
        let repo = test_repo().await?;
        let key = repo.0.join("signing-key");
        let key_arg = key.to_string_lossy().to_string();
        let output = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", &key_arg])
            .output()
            .await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &key_arg]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        commit(&repo.0, "signed").await?;

        ensure_signed_commits(&repo.0, "main").await
    }

    async fn ssh_key(path: &Path) -> Result<()> {
        let key_arg = path.to_string_lossy().to_string();
        let output = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", &key_arg])
            .output()
            .await?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Ok(())
    }

    #[tokio::test]
    async fn publish_signs_leftovers_with_the_configured_key() -> Result<()> {
        let repo = test_repo().await?;
        let key = repo.0.join("signing-key");
        ssh_key(&key).await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &key.to_string_lossy()]).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        let empty_home = repo.0.join("home");
        assert!(commit_signed_with_home(&repo.0, "feat: leftovers", Some(&empty_home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        ensure_signed_commits(&repo.0, "main").await?;
        assert!(!commit_signed_with_home(&repo.0, "feat: nothing", Some(&empty_home)).await?);
        Ok(())
    }

    #[tokio::test]
    async fn publish_signs_leftovers_with_a_fallback_key_even_with_gpgsign_off() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        let home = repo.0.join("home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(".ssh/loka_signing")).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        assert!(commit_signed_with_home(&repo.0, "feat: leftovers", Some(&home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        let log = run_git_checked_output(&repo.0, &["log", "--format=%s", "-1"]).await?;
        assert_eq!(log.trim(), "feat: leftovers");
        Ok(())
    }

    async fn staged(repo: &Path) -> Result<String> {
        Ok(run_git_checked_output(repo, &["diff", "--cached", "--name-only"]).await?.trim().to_string())
    }

    #[tokio::test]
    async fn publish_commits_unsigned_only_without_any_key_and_with_gpgsign_off() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        assert!(commit_signed_with_home(&repo.0, "feat: leftovers", Some(&repo.0.join("home"))).await?);
        assert!(!commit_is_signed(&repo.0, "HEAD").await?);
        let log = run_git_checked_output(&repo.0, &["log", "--format=%s", "-1"]).await?;
        assert_eq!(log.trim(), "feat: leftovers");
        Ok(())
    }

    #[tokio::test]
    async fn publish_refuses_leftovers_without_a_key_when_gpgsign_is_on() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        run_git(&repo.0, &["config", "gpg.format", "openpgp"]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        tokio::fs::write(repo.0.join("sprite-stack.ts"), "left").await?;
        let error = commit_signed_with_home(&repo.0, "feat: leftovers", Some(&repo.0.join("home")))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("will not commit them unsigned"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        assert!(staged(&repo.0).await?.is_empty());
        assert!(repo.0.join("sprite-stack.ts").is_file());
        let error = commit_signed_with_home(&repo.0, "feat: leftovers", None).await.unwrap_err();
        assert!(error.to_string().contains("will not commit them unsigned"), "{error:#}");
        Ok(())
    }

    #[tokio::test]
    async fn publish_refuses_when_the_signing_key_does_not_sign() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        let error = commit_signed_with_home(&repo.0, "feat: leftovers", Some(&repo.0.join("home")))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("could not sign"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        assert!(staged(&repo.0).await?.is_empty());
        assert!(repo.0.join("left").is_file());
        Ok(())
    }

    async fn unborn_repo() -> Result<TestRepo> {
        let repo = TestRepo(std::env::temp_dir().join(format!("loka-unborn-{}", Uuid::new_v4())));
        tokio::fs::create_dir_all(&repo.0).await?;
        run_git(&repo.0, &["init", "--initial-branch=main"]).await?;
        run_git(&repo.0, &["config", "user.name", "Loka Test"]).await?;
        run_git(&repo.0, &["config", "user.email", "test@loka.ygpt.dev"]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "false"]).await?;
        Ok(repo)
    }

    #[tokio::test]
    async fn undoing_a_root_commit_leaves_the_files_unstaged() -> Result<()> {
        let repo = unborn_repo().await?;
        tokio::fs::write(repo.0.join("first"), "first").await?;
        run_git(&repo.0, &["add", "first"]).await?;
        run_git(&repo.0, &["commit", "-m", "first"]).await?;
        undo_head_commit(&repo.0).await?;
        assert!(!has_rev(&repo.0, "HEAD").await);
        let indexed = run_git_checked_output(&repo.0, &["ls-files"]).await?;
        assert!(indexed.trim().is_empty());
        assert!(repo.0.join("first").is_file());
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_signed_root_commit_leaves_nothing_staged() -> Result<()> {
        let repo = unborn_repo().await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        assert!(commit_signed_with_home(&repo.0, "feat: leftovers", None).await.is_err());
        assert!(!has_rev(&repo.0, "HEAD").await);
        let indexed = run_git_checked_output(&repo.0, &["ls-files"]).await?;
        assert!(indexed.trim().is_empty());
        assert!(repo.0.join("left").is_file());
        Ok(())
    }

    #[test]
    fn only_a_gpgsig_header_counts_as_a_signature() {
        assert!(has_signature("tree a\nauthor x\ngpgsig -----BEGIN SSH SIGNATURE-----\n abc\n\nmsg"));
        assert!(!has_signature("tree a\nauthor x\n\ngpgsig in the message"));
        assert!(!has_signature("tree a"));
    }

    async fn remote_branches(remote: &Path) -> Result<String> {
        let output = Command::new("git")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_DIR")
            .arg("--git-dir")
            .arg(remote)
            .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
            .output()
            .await?;
        if !output.status.success() {
            bail!("git for-each-ref failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    #[tokio::test]
    async fn publish_pushes_only_the_quest_branch() -> Result<()> {
        let (remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "child").await?;
        let quest = "add-sprite-stack-8210017e";
        for foreign in ["sprite-stacked-world-characters-buildings-7d988444", "main"] {
            let error = push_quest_branch(&repo.0, foreign, quest).await.unwrap_err();
            assert!(error.to_string().contains("only pushes its own branch"), "{error:#}");
        }
        assert_eq!(remote_branches(&remote.0).await?.trim(), "main");
        if let Err(error) = push_quest_branch(&repo.0, &format!("{quest}-2"), quest).await {
            assert!(!error.to_string().contains("only pushes its own branch"), "{error:#}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn stacked_quests_open_into_the_recorded_parent_branch() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "parent").await?;
        test_git(&repo.0, &["push", "origin", "HEAD:refs/heads/parent-7d988444"]).await?;
        commit(&repo.0, "child").await?;
        run_git(&repo.0, &["switch", "-c", "elsewhere", "main"]).await?;
        commit(&repo.0, "other").await?;
        test_git(&repo.0, &["push", "origin", "HEAD:refs/heads/unrelated"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        run_git(&repo.0, &["update-ref", "-d", "refs/remotes/origin/parent-7d988444"]).await?;

        let stacked = |parent: Option<&'static str>| {
            let dir = repo.0.clone();
            async move { fetched_stack_parent_over(&dir, "main", parent, Some(TEST_PROTOCOLS)).await }
        };
        assert_eq!(stacked(Some("parent-7d988444")).await, Some("parent-7d988444".to_string()));
        assert!(has_rev(&repo.0, "refs/remotes/origin/parent-7d988444").await);
        assert_eq!(stacked(None).await, None);
        assert_eq!(stacked(Some("main")).await, None);
        assert_eq!(stacked(Some("unrelated")).await, None);
        assert_eq!(stacked(Some("gone-1234abcd")).await, None);
        assert_eq!(stacked(Some("bad..ref")).await, None);
        Ok(())
    }

    #[tokio::test]
    async fn disabled_signing_allows_unsigned_commits() -> Result<()> {
        let repo = test_repo().await?;
        commit(&repo.0, "unsigned").await?;

        ensure_signed_commits(&repo.0, "main").await
    }

    #[tokio::test]
    async fn configured_missing_key_fails() {
        let signing = SigningConfig {
            ssh_key_path: std::env::temp_dir().join(format!("missing-{}", Uuid::new_v4())),
            allowed_signers_path: None,
        };

        let error = configure_git_signing(Some(&signing)).await.unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
        LOCK.lock().unwrap()
    }

    struct EnvRestore(Vec<(&'static str, Option<String>)>);

    impl EnvRestore {
        fn stash(keys: &[&'static str]) -> Self {
            Self(
                keys.iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            )
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn temp_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
    }

    fn clear_git_env() {
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
    }

    async fn global_git_config(home: &Path, key: &str) -> Option<String> {
        let output = Command::new("git")
            .args(["config", "--global", key])
            .env("HOME", home)
            .output()
            .await
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn empty_repo(prefix: &str) -> Result<TestRepo> {
        let repo = TestRepo(temp_path(prefix));
        tokio::fs::create_dir_all(&repo.0).await?;
        run_git(&repo.0, &["init", "--initial-branch=main"]).await?;
        Ok(repo)
    }

    #[tokio::test]
    async fn startup_never_rewrites_a_global_git_identity() -> Result<()> {
        let _guard = env_lock();
        let _restore = EnvRestore::stash(&["HOME", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"]);
        clear_git_env();
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        tokio::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Existing User\n\temail = me@example.com\n",
        )
        .await?;

        configure_git_auth("test-token").await?;

        assert_eq!(
            global_git_config(&home, "user.name").await.as_deref(),
            Some("Existing User")
        );
        assert_eq!(
            global_git_config(&home, "user.email").await.as_deref(),
            Some("me@example.com")
        );
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn repo_identity_is_left_untouched_when_present() -> Result<()> {
        let _guard = env_lock();
        let _restore = EnvRestore::stash(&["HOME", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"]);
        clear_git_env();
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        std::env::set_var("HOME", &home);

        let repo = empty_repo("loka-ident").await?;
        run_git(&repo.0, &["config", "user.name", "Repo Owner"]).await?;
        run_git(&repo.0, &["config", "user.email", "owner@repo.example"]).await?;

        ensure_repo_identity(&repo.0, "test-token").await?;

        assert_eq!(
            git_config_get(&repo.0, "user.name").await,
            "Repo Owner"
        );
        assert_eq!(
            git_config_get(&repo.0, "user.email").await,
            "owner@repo.example"
        );
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn owner_identity_comes_from_gh_repo_level_only() -> Result<()> {
        let _guard = env_lock();
        let _restore =
            EnvRestore::stash(&["HOME", "PATH", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"]);
        clear_git_env();
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        std::env::set_var("HOME", &home);

        let bin = temp_path("loka-bin");
        tokio::fs::create_dir_all(&bin).await?;
        tokio::fs::write(
            bin.join("gh"),
            "#!/bin/sh\nif [ \"$1\" = \"api\" ] && [ \"$2\" = \"user\" ]; then\n  printf '%s' '{\"login\":\"mona\",\"name\":\"Mona Lisa\",\"email\":null}'\n  exit 0\nfi\nexit 1\n",
        )
        .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .await?;
        }
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));

        let repo = empty_repo("loka-ident").await?;
        ensure_repo_identity(&repo.0, "test-token").await?;

        assert_eq!(git_config_get(&repo.0, "user.name").await, "Mona Lisa");
        assert_eq!(
            git_config_get(&repo.0, "user.email").await,
            "mona@users.noreply.github.com"
        );
        assert_eq!(global_git_config(&home, "user.name").await, None);
        assert_eq!(global_git_config(&home, "user.email").await, None);

        commit(&repo.0, "authored").await?;
        let author = run_git_checked_output(&repo.0, &["log", "-1", "--format=%an <%ae>"]).await?;
        assert_eq!(author.trim(), "Mona Lisa <mona@users.noreply.github.com>");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&bin);
        Ok(())
    }

    #[tokio::test]
    async fn missing_owner_identity_falls_back_without_global_writes() -> Result<()> {
        let _guard = env_lock();
        let _restore =
            EnvRestore::stash(&["HOME", "PATH", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"]);
        clear_git_env();
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        std::env::set_var("HOME", &home);

        let bin = temp_path("loka-bin");
        tokio::fs::create_dir_all(&bin).await?;
        tokio::fs::write(bin.join("gh"), "#!/bin/sh\nexit 1\n").await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .await?;
        }
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));

        let repo = empty_repo("loka-ident").await?;
        ensure_repo_identity(&repo.0, "test-token").await?;

        assert_eq!(
            git_config_get(&repo.0, "user.name").await,
            FALLBACK_AUTHOR_NAME
        );
        assert_eq!(
            git_config_get(&repo.0, "user.email").await,
            FALLBACK_AUTHOR_EMAIL
        );
        assert_eq!(global_git_config(&home, "user.name").await, None);
        assert_eq!(global_git_config(&home, "user.email").await, None);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&bin);
        Ok(())
    }

    #[tokio::test]
    async fn commit_hook_strips_co_author_trailers() -> Result<()> {
        let repo = test_repo().await?;
        ensure_commit_hygiene(&repo.0).await?;

        tokio::fs::write(repo.0.join("trailer"), "trailer").await?;
        run_git(&repo.0, &["add", "trailer"]).await?;
        let status = Command::new("git")
            .current_dir(&repo.0)
            .args(["commit", "-m", "feat: subject", "-m", "Co-authored-by: Cursor <cursor@example.com>\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"])
            .status()
            .await?;
        assert!(status.success());

        let body = run_git_checked_output(&repo.0, &["log", "-1", "--format=%B"]).await?;
        assert!(body.contains("feat: subject"), "{body}");
        assert!(!body.to_ascii_lowercase().contains("co-authored-by"), "{body}");

        let hooks_path = Command::new("git").current_dir(&repo.0).args(["config", "--get", "core.hooksPath"]).output().await?;
        let hooks_path = String::from_utf8_lossy(&hooks_path.stdout).trim().to_string();
        assert!(hooks_path.ends_with("loka-hooks"), "{hooks_path}");
        Ok(())
    }

    #[tokio::test]
    async fn repo_config_cannot_run_commands_in_runtime_git() -> Result<()> {
        let repo = test_repo().await?;
        let marker = repo.0.join("ran");
        let command = format!("touch {}; cat", marker.display());
        run_git(&repo.0, &["config", "filter.evil.clean", &command]).await?;
        run_git(&repo.0, &["config", "filter.evil.smudge", &command]).await?;
        run_git(&repo.0, &["config", "diff.evil.textconv", &command]).await?;
        run_git(&repo.0, &["config", "core.sshCommand", &command]).await?;
        tokio::fs::write(repo.0.join(".gitattributes"), "*.txt filter=evil diff=evil\n").await?;
        tokio::fs::write(repo.0.join("a.txt"), "a").await?;
        run_git(&repo.0, &["add", "-A"]).await?;
        run_git(&repo.0, &["commit", "-m", "attributes"]).await?;
        tokio::fs::write(repo.0.join("a.txt"), "b").await?;
        work_summary(&repo.0, "main").await;
        run_git_output(&repo.0, &["diff", "--no-ext-diff", "--no-textconv", "HEAD"]).await?;
        run_git(&repo.0, &["checkout", "--", "."]).await?;
        assert!(!marker.exists(), "a repo filter ran in runtime git");
        Ok(())
    }
}
