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

pub async fn commit_all(worktree_path: &Path, message: &str) -> Result<bool> {
    run_git(worktree_path, &["add", "-A"]).await?;

    let status = run_git_output(worktree_path, &["status", "--porcelain"]).await?;
    if status.trim().is_empty() {
        return Ok(false);
    }

    run_git(worktree_path, &["commit", "-m", &without_coauthors(message)]).await?;
    Ok(true)
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
    match run_git(worktree_path, &["push", "-u", "origin", &refspec]).await {
        Err(error) if rewritten_history(&error.to_string()) => {
            run_git(worktree_path, &["push", "-u", "--force-with-lease", "origin", &refspec]).await
        }
        other => other,
    }
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
    if branch != "HEAD" {
        return Ok(branch);
    }
    recorded
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "HEAD")
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("workspace is on a detached HEAD and has no task branch"))
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
    let signing = workspace_command("git", worktree_path)
        .await
        .args(["config", "--bool", "--get", "commit.gpgsign"])
        .output()
        .await
        .context("Failed to read git commit signing configuration")?;
    if signing.status.code() == Some(1) {
        return Ok(());
    }
    if !signing.status.success() {
        anyhow::bail!(
            "Could not read commit.gpgsign: {}",
            String::from_utf8_lossy(&signing.stderr).trim()
        );
    }
    if String::from_utf8_lossy(&signing.stdout).trim() != "true" {
        return Ok(());
    }

    let base_ref = format!("origin/{base_branch}");
    let commits =
        run_git_checked_output(worktree_path, &["rev-list", &format!("{base_ref}..HEAD")]).await?;
    for sha in commits.lines().filter(|sha| !sha.trim().is_empty()) {
        let commit = run_git_checked_output(worktree_path, &["cat-file", "-p", sha]).await?;
        let signed = commit
            .split_once("\n\n")
            .map(|(headers, _)| headers.lines().any(|line| line.starts_with("gpgsig ")))
            .unwrap_or(false);
        if !signed {
            anyhow::bail!(
                "Commit {sha} is unsigned while commit.gpgsign is enabled. Sign the commit using the configured key before retrying"
            );
        }
    }
    Ok(())
}

pub async fn has_commits_ahead(worktree_path: &Path, base_branch: &str) -> Result<bool> {
    let base_ref = format!("origin/{base_branch}");
    let output = run_git_output(
        worktree_path,
        &["rev-list", "--count", &format!("{base_ref}..HEAD")],
    )
    .await?;
    let count: i64 = output.trim().parse().unwrap_or(0);
    if count == 0 {
        return Ok(false);
    }
    let merged = run_git_output(worktree_path, &["merge-tree", "--write-tree", &base_ref, "HEAD"]).await?;
    let merged_tree = merged.lines().next().unwrap_or("").trim();
    if merged_tree.is_empty() {
        return Ok(true);
    }
    let base_tree = run_git_output(worktree_path, &["rev-parse", &format!("{base_ref}^{{tree}}")]).await?;
    Ok(merged_tree != base_tree.trim())
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
        assert!(commit_all(&repo.0, "attributes").await?);
        tokio::fs::write(repo.0.join("a.txt"), "b").await?;
        work_summary(&repo.0, "main").await;
        run_git_output(&repo.0, &["diff", "--no-ext-diff", "--no-textconv", "HEAD"]).await?;
        run_git(&repo.0, &["checkout", "--", "."]).await?;
        assert!(!marker.exists(), "a repo filter ran in runtime git");
        Ok(())
    }
}
