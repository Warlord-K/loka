use anyhow::{Context, Result};
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
) -> Result<PrInfo> {
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
            return get_existing_pr(worktree_path, repo, branch).await;
        }
        anyhow::bail!("gh pr create failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let pr_url = stdout.lines().last().unwrap_or(&stdout).trim().to_string();

    let number = extract_pr_number(&pr_url).unwrap_or(0);

    info!(pr_url = %pr_url, number, "PR created");
    Ok(PrInfo {
        url: pr_url,
        number,
    })
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

pub async fn open_pr_containing_head(
    worktree_path: &Path,
    repo: &str,
    base_branch: &str,
) -> Result<Option<(PrInfo, String)>> {
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
    let prs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    let prs: Vec<serde_json::Value> = prs.into_iter().filter(|pr| !pr["isCrossRepository"].as_bool().unwrap_or(false)).collect();
    if prs.is_empty() {
        return Ok(None);
    }
    let base_ref = format!("origin/{base_branch}");
    let unpublished = commits_only_in_head(worktree_path, &base_ref).await;
    for pr in prs {
        let head = pr["headRefOid"].as_str().unwrap_or("");
        let contains = match &unpublished {
            Some(commits) => commits.contains(head),
            None => is_ancestor(worktree_path, head, "HEAD").await? && !is_ancestor(worktree_path, head, &base_ref).await?,
        };
        if contains {
            let info = PrInfo {
                url: pr["url"].as_str().unwrap_or("").to_string(),
                number: pr["number"].as_i64().unwrap_or(0) as i32,
            };
            return Ok(Some((
                info,
                pr["headRefName"].as_str().unwrap_or("").to_string(),
            )));
        }
    }
    Ok(None)
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
    #[test]
    fn final_pr_states_stop_publish() {
        assert!(super::is_final_pr_state("MERGED"));
        assert!(super::is_final_pr_state("CLOSED"));
        assert!(!super::is_final_pr_state("OPEN"));
        assert!(!super::is_final_pr_state(""));
    }
}
