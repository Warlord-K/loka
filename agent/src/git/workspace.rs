use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{info, warn};

use crate::cli::{job_command, workspace_command};
use loka_shared::git_names::{github_repo, validate_git_ref};

pub struct GitHost {
    home: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
}

impl GitHost {
    pub const INHERITED: GitHost = GitHost { home: None, path: None };

    #[cfg(test)]
    pub(crate) fn isolated(home: &Path, path: Option<std::ffi::OsString>) -> Self {
        Self { home: Some(home.to_path_buf()), path }
    }

    fn home(&self) -> PathBuf {
        self.home.clone().unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())))
    }

    fn home_var(&self) -> Option<PathBuf> {
        self.home.clone().or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }

    fn path_var(&self) -> Option<std::ffi::OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }

    fn credentials(&self) -> PathBuf {
        match &self.home {
            Some(home) => home.join(".git-credentials"),
            None => PathBuf::from("/tmp/.git-credentials"),
        }
    }

    fn author(&self, name: &str) -> Option<String> {
        self.home.is_none().then(|| std::env::var(name).ok()).flatten().filter(|value| !value.is_empty())
    }

    fn apply<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        if let Some(home) = &self.home {
            command
                .env("HOME", home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env("GIT_CONFIG_NOSYSTEM", "1");
            for name in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"] {
                command.env_remove(name);
            }
        }
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        command
    }
}

pub async fn configure_git_auth(github_token: &str, host: &GitHost) -> Result<()> {
    let home = host.home();

    if !github_token.is_empty() {
        let cred_path = host.credentials();
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

        let mut git = job_command("git");
        let _ = host
            .apply(&mut git)
            .args([
                "config",
                "--global",
                "credential.helper",
                &format!("store --file={}", cred_path.display()),
            ])
            .current_dir(&home)
            .output()
            .await;
    }

    Ok(())
}

pub async fn configure_git_signing(signing: Option<&crate::config::SigningConfig>, host: &GitHost) -> Result<()> {
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

    let local_key = host.home().join(".ssh/signing_key_active");
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
        let mut git = job_command("git");
        let output = host
            .apply(&mut git)
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
            let mut git = job_command("git");
            let _ = host
                .apply(&mut git)
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

pub async fn ensure_repo_identity(repo_path: &Path, github_token: &str, host: &GitHost) -> Result<()> {
    let have_name = !git_config_get(repo_path, "user.name", host).await.is_empty();
    let have_email = !git_config_get(repo_path, "user.email", host).await.is_empty();
    if have_name && have_email {
        return Ok(());
    }

    let gh = github_identity(github_token, host).await;

    if !have_name {
        let name = host.author("GIT_AUTHOR_NAME");
        let name = name
            .or_else(|| gh.as_ref().map(|(name, _)| name.clone()))
            .unwrap_or_else(|| FALLBACK_AUTHOR_NAME.to_string());
        run_git_as(host, repo_path, &["config", "user.name", &name]).await?;
        info!(repo = %repo_path.display(), "Set repo-local git user.name");
    }
    if !have_email {
        let email = host.author("GIT_AUTHOR_EMAIL");
        let email = email
            .or_else(|| gh.as_ref().map(|(_, email)| email.clone()))
            .unwrap_or_else(|| FALLBACK_AUTHOR_EMAIL.to_string());
        run_git_as(host, repo_path, &["config", "user.email", &email]).await?;
        info!(repo = %repo_path.display(), "Set repo-local git user.email");
    }
    Ok(())
}

async fn git_config_get(repo_path: &Path, key: &str, host: &GitHost) -> String {
    run_git_output_as(host, repo_path, &["config", key])
        .await
        .unwrap_or_default()
        .trim()
        .to_string()
}

async fn github_identity(github_token: &str, host: &GitHost) -> Option<(String, String)> {
    let mut cmd = Command::new("gh");
    host.apply(&mut cmd).args(["api", "user"]);
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

    if let Err(e) = ensure_repo_identity(&clone_path, github_token, &GitHost::INHERITED).await {
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

const NEVER_COMMITTED: [&str; 2] = [":(exclude,glob)**/.mcp-auth/**", ":(exclude,glob)**/\\*\\*\\*/**"];

async fn stage_all(worktree_path: &Path) -> Result<()> {
    let mut args = vec!["add", "-A", "--", "."];
    args.extend(NEVER_COMMITTED);
    run_git(worktree_path, &args).await
}

async fn pending_changes(worktree_path: &Path, untracked: &str) -> Result<String> {
    let mut args = vec!["status", "--porcelain", untracked, "--", "."];
    args.extend(NEVER_COMMITTED);
    run_git_checked_output(worktree_path, &args).await
}

pub const SIGNING_SETUP: &str = "Show the owner setup:signing so they can create the Loka signing key (~/.ssh/loka_signing) and add its public key to GitHub as a Signing key.";

const UNSIGNED_LEFTOVERS: &str = "Publish found uncommitted changes and commit.gpgsign is on, but no configured key could sign them, so it will not commit them unsigned. Loka never signs with another key in its place. Fix the configured key or set user.signingkey, then publish again.";

pub const LOKA_SIGNING_KEY: &str = ".ssh/loka_signing";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signing {
    Configured,
    Loka(PathBuf),
    Unset,
}

fn config_values(list: &str) -> std::collections::HashMap<String, String> {
    let mut values = std::collections::HashMap::new();
    for entry in list.split('\0').filter(|entry| !entry.is_empty()) {
        let (name, value) = entry.split_once('\n').unwrap_or((entry, "true"));
        values.insert(name.to_ascii_lowercase(), value.to_string());
    }
    values
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitSigning {
    pub key: String,
    pub gpgsign: bool,
    pub format: String,
    pub program: String,
}

impl GitSigning {
    pub fn from_config_list(list: &str) -> Self {
        let values = config_values(list);
        let get = |name: &str| values.get(name).map(|value| value.trim().to_string()).unwrap_or_default();
        let format = match get("gpg.format").to_ascii_lowercase().as_str() {
            "" => "openpgp".to_string(),
            other => other.to_string(),
        };
        let program = Some(get(&format!("gpg.{format}.program")))
            .filter(|program| !program.is_empty())
            .unwrap_or_else(|| if format == "openpgp" { get("gpg.program") } else { String::new() });
        GitSigning {
            key: get("user.signingkey"),
            gpgsign: matches!(get("commit.gpgsign").to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"),
            format,
            program,
        }
    }

    fn signer(&self) -> String {
        if !self.program.is_empty() {
            return self.program.clone();
        }
        match self.format.as_str() {
            "ssh" => "ssh-keygen",
            "x509" => "gpgsm",
            _ => "gpg",
        }
        .to_string()
    }

    pub fn configured(&self, path: Option<&std::ffi::OsStr>) -> bool {
        !self.key.is_empty() || (self.gpgsign && self.format != "ssh" && on_path(&self.signer(), path))
    }

    pub fn problem(&self, home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Option<String> {
        let signer = self.signer();
        if !on_path(&signer, path) {
            return Some(format!("{signer} is not installed, so git cannot sign with the configured key"));
        }
        if self.format != "ssh" || self.key.is_empty() || self.key.starts_with("key::") || self.key.starts_with("ssh-") {
            return None;
        }
        let file = match (self.key.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(&self.key),
        };
        (!file.is_file()).then(|| format!("the signing key {} does not exist", self.key))
    }
}

fn on_path(program: &str, path: Option<&std::ffi::OsStr>) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    path.is_some_and(|paths| std::env::split_paths(paths).any(|dir| dir.join(program).is_file()))
}

pub fn signing_choice(git: &GitSigning, home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Signing {
    if git.configured(path) {
        return Signing::Configured;
    }
    match home.map(|home| home.join(LOKA_SIGNING_KEY)).filter(|key| key.is_file()) {
        Some(key) => Signing::Loka(key),
        None => Signing::Unset,
    }
}

pub fn loka_key_config(key: &Path) -> [(&'static str, String); 3] {
    [
        ("gpg.format", "ssh".to_string()),
        ("user.signingkey", key.display().to_string()),
        ("commit.gpgsign", "true".to_string()),
    ]
}

const PINNED_SIGNING_KEYS: [(&str, &str); 4] = [
    ("gpg.format", "gpg.format"),
    ("user.signingkey", "user.signingKey"),
    ("commit.gpgsign", "commit.gpgSign"),
    ("gpg.ssh.allowedsignersfile", "gpg.ssh.allowedSignersFile"),
];

pub fn signing_pins(list: &str, home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Vec<(String, String)> {
    match signing_choice(&GitSigning::from_config_list(list), home, path) {
        Signing::Unset => Vec::new(),
        Signing::Loka(key) => loka_key_config(&key).into_iter().map(|(name, value)| (name.to_string(), value)).collect(),
        Signing::Configured => {
            let values = config_values(list);
            PINNED_SIGNING_KEYS
                .iter()
                .filter_map(|(lookup, name)| Some((name.to_string(), values.get(*lookup).filter(|value| !value.trim().is_empty())?.clone())))
                .collect()
        }
    }
}

fn split_by_scope(scoped: &str) -> (String, String) {
    let (mut inherited, mut repo_level) = (String::new(), String::new());
    let mut parts = scoped.split('\0');
    while let (Some(scope), Some(entry)) = (parts.next(), parts.next()) {
        let target = if matches!(scope, "local" | "worktree") { &mut repo_level } else { &mut inherited };
        target.push_str(entry);
        target.push('\0');
    }
    (inherited, repo_level)
}

pub async fn pin_signing(worktree_path: &Path) {
    if crate::cli::job_user().is_some() {
        return;
    }
    if let Err(error) = pin_signing_in(worktree_path, &GitHost::INHERITED).await {
        warn!(worktree = %worktree_path.display(), %error, "could not write the signing settings into the quest worktree");
    }
}

async fn pin_signing_in(worktree_path: &Path, host: &GitHost) -> Result<()> {
    let scoped = run_git_checked_output_as(host, worktree_path, &["config", "--list", "-z", "--show-scope"]).await?;
    let (inherited, repo_level) = split_by_scope(&scoped);
    let pins = signing_pins(&inherited, host.home_var().as_deref(), host.path_var().as_deref());
    if pins.is_empty() {
        return Ok(());
    }
    if config_values(&repo_level).get("extensions.worktreeconfig").map(|value| value.as_str()) != Some("true") {
        run_git_as(host, worktree_path, &["config", "--local", "extensions.worktreeConfig", "true"]).await?;
    }
    for (name, value) in &pins {
        run_git_as(host, worktree_path, &["config", "--worktree", name, value]).await?;
    }
    Ok(())
}

fn command_env(command: &std::process::Command, name: &str) -> Option<std::ffi::OsString> {
    command.get_envs().find(|(key, _)| *key == name).and_then(|(_, value)| value.map(ToOwned::to_owned))
}

fn git_signing_for(command: Option<&std::process::Command>, dir: &Path) -> GitSigning {
    let mut git = std::process::Command::new("git");
    if let Some(command) = command {
        git.env_clear().envs(command.get_envs().filter_map(|(name, value)| Some((name, value?))));
    }
    git.args(["config", "--list", "-z"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let list = git.output().ok().filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    GitSigning::from_config_list(&list.unwrap_or_default())
}

pub fn append_git_config(command: &mut tokio::process::Command, entries: &[(&str, &str)]) {
    let start = command_env(command.as_std(), "GIT_CONFIG_COUNT").and_then(|value| value.to_str()?.parse::<usize>().ok()).unwrap_or(0);
    for (offset, (key, value)) in entries.iter().enumerate() {
        command.env(format!("GIT_CONFIG_KEY_{}", start + offset), key).env(format!("GIT_CONFIG_VALUE_{}", start + offset), value);
    }
    command.env("GIT_CONFIG_COUNT", (start + entries.len()).to_string());
}

pub fn sign_with_loka_key(command: &mut tokio::process::Command, dir: &Path) -> Signing {
    let home = command_env(command.as_std(), "HOME").map(PathBuf::from);
    let path = command_env(command.as_std(), "PATH").or_else(|| std::env::var_os("PATH"));
    let git = git_signing_for(Some(command.as_std()), dir);
    let choice = signing_choice(&git, home.as_deref(), path.as_deref());
    if let Signing::Loka(key) = &choice {
        let config = loka_key_config(key);
        let entries: Vec<(&str, &str)> = config.iter().map(|(key, value)| (*key, value.as_str())).collect();
        append_git_config(command, &entries);
    }
    choice
}

pub fn ensure_loka_key_in(home: &Path, configured: bool) -> Result<bool> {
    let key = home.join(LOKA_SIGNING_KEY);
    if configured {
        return Ok(false);
    }
    let public = PathBuf::from(format!("{}.pub", key.display()));
    match std::fs::symlink_metadata(&key) {
        Ok(_) if key.is_file() => return Ok(false),
        Ok(_) => bail!("{} exists but is not a key file (a dangling symlink or a folder), so Loka left it alone and created no key", key.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("Could not check {}", key.display())),
    }
    match std::fs::symlink_metadata(&public) {
        Ok(_) => bail!("{} exists without its private key, so Loka left it alone and created no key", public.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("Could not check {}", public.display())),
    }
    let ssh = key.parent().expect("the Loka key has a parent");
    std::fs::create_dir_all(ssh).with_context(|| format!("Could not create {}", ssh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(ssh, std::fs::Permissions::from_mode(0o700))?;
    }
    let output = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "loka-signing", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .output()
        .context("Could not run ssh-keygen to create the Loka signing key")?;
    if !output.status.success() {
        bail!("ssh-keygen could not create the Loka signing key: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(true)
}

pub fn ensure_loka_key() {
    if !cfg!(target_os = "macos") || crate::cli::job_user().is_some() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let path = std::env::var_os("PATH");
    let configured = git_signing_for(None, &home).configured(path.as_deref());
    match ensure_loka_key_in(&home, configured) {
        Ok(true) => info!(key = %home.join(LOKA_SIGNING_KEY).display(), "Created the Loka signing key for quest commits"),
        Ok(false) => {}
        Err(error) => warn!(%error, "could not create the Loka signing key, quest commits stay unsigned"),
    }
}

pub fn signing_view() -> serde_json::Value {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, view)) = cache.as_ref().filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(60)) {
        return view.clone();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    let dir = home.clone().unwrap_or_else(std::env::temp_dir);
    let git = git_signing_for(None, &dir);
    let mut view = signing_view_of(&git, home.as_deref(), path.as_deref());
    view["job_uids"] = serde_json::json!(crate::cli::job_user().is_some());
    *cache = Some((std::time::Instant::now(), view.clone()));
    view
}

fn signing_view_of(git: &GitSigning, home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> serde_json::Value {
    match signing_choice(git, home, path) {
        Signing::Configured => serde_json::json!({ "source": "configured", "problem": git.problem(home, path) }),
        Signing::Unset => serde_json::json!({ "source": "unset", "requires": git.gpgsign }),
        Signing::Loka(key) => {
            let public = PathBuf::from(format!("{}.pub", key.display()));
            let fingerprint = std::process::Command::new("ssh-keygen")
                .arg("-lf")
                .arg(if public.is_file() { &public } else { &key })
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).map(str::to_string));
            serde_json::json!({ "source": "loka", "key": format!("~/{LOKA_SIGNING_KEY}"), "public": public.is_file(), "fingerprint": fingerprint })
        }
    }
}

const SIGNING_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct PublishCommit {
    api: bool,
    leftover: Option<String>,
}

impl PublishCommit {
    pub fn carried(&self) -> PublishCommit {
        PublishCommit { api: self.api, leftover: None }
    }
}

pub struct PublishTarget<'a> {
    pub repo: &'a str,
    pub base: &'a str,
    pub onto: &'a str,
    pub quest_branch: &'a str,
}

pub fn changes_of_another_writer(writer: &str) -> String {
    format!(
        "Publish stopped: the quest folder holds uncommitted changes that this session did not make. The last work in the folder came from {writer}, so publishing now would sweep its changes into this pull request. Let that session commit or discard its changes, then publish again."
    )
}

pub async fn commit_for_publish(worktree_path: &Path, repo: &str, message: &str, other_writer: Option<&str>) -> Result<PublishCommit> {
    if let Some(writer) = other_writer {
        if !pending_changes(worktree_path, "--untracked-files=all").await?.trim().is_empty() {
            bail!(changes_of_another_writer(writer));
        }
    }
    if !super::api_commits::available(worktree_path, repo).await {
        commit_signed(worktree_path, message).await?;
        return Ok(PublishCommit { api: false, leftover: None });
    }
    let leftover = match commit_unsigned(worktree_path, message).await? {
        true => Some(run_git_checked_output(worktree_path, &["rev-parse", "HEAD"]).await?.trim().to_string()),
        false => None,
    };
    Ok(PublishCommit { api: true, leftover })
}

enum CommitText {
    Message(String),
    ReusedFrom(String),
}

impl CommitText {
    fn args(&self) -> [&str; 2] {
        match self {
            Self::Message(message) => ["-m", message],
            Self::ReusedFrom(commit) => ["-C", commit],
        }
    }
}

async fn commit_unsigned(worktree_path: &Path, message: &str) -> Result<bool> {
    commit_unsigned_text(worktree_path, &CommitText::Message(without_coauthors(message))).await
}

async fn commit_unsigned_text(worktree_path: &Path, text: &CommitText) -> Result<bool> {
    let status = pending_changes(worktree_path, "--untracked-files=all").await?;
    if status.trim().is_empty() {
        return Ok(false);
    }
    let [flag, value] = text.args();
    stage_all(worktree_path).await?;
    if let Err(error) = run_git(worktree_path, &["-c", "commit.gpgsign=false", "commit", flag, value]).await {
        unstage(worktree_path).await?;
        return Err(error);
    }
    Ok(true)
}

pub async fn publish_commits(worktree_path: &Path, committed: &PublishCommit, target: &PublishTarget<'_>, branch: &str) -> Result<()> {
    if !is_quest_branch(branch, target.quest_branch) {
        bail!("Publish refused to push to {branch}: a quest only pushes its own branch ({}). Its pull request opens from that branch instead.", target.quest_branch);
    }
    if committed.api {
        match super::api_commits::replay(worktree_path, target.repo, target.onto, branch, target.quest_branch).await {
            Ok(super::api_commits::Replay::Published { .. }) => return Ok(()),
            Ok(super::api_commits::Replay::Unsupported(reason)) => {
                info!(branch, "Publishing with a signed push instead of the GitHub API: {reason}");
            }
            Err(error) => warn!(branch, "Publishing through the GitHub API failed, using a signed push instead: {error:#}"),
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        sign_leftover_in_place(worktree_path, committed, home.as_deref(), None).await?;
    }
    ensure_signed_commits(worktree_path, target.base).await?;
    push_quest_branch(worktree_path, branch, target.quest_branch).await
}

async fn sign_leftover_in_place(worktree_path: &Path, committed: &PublishCommit, home: Option<&Path>, gnupg_home: Option<&Path>) -> Result<()> {
    let Some(leftover) = committed.leftover.as_deref() else {
        return Ok(());
    };
    let head = run_git_checked_output(worktree_path, &["rev-parse", "HEAD"]).await?;
    if head.trim() != leftover {
        bail!("Publish stopped: the commit it made for the uncommitted changes ({leftover}) is no longer the newest commit, so it was not signed. Publish again.");
    }
    sign_new_commit(worktree_path, home, gnupg_home).await
}

pub async fn commit_signed(worktree_path: &Path, message: &str) -> Result<bool> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    commit_signed_with_home(worktree_path, message, home.as_deref(), None).await
}

async fn commit_signed_with_home(
    worktree_path: &Path,
    message: &str,
    home: Option<&Path>,
    gnupg_home: Option<&Path>,
) -> Result<bool> {
    commit_signed_text(worktree_path, CommitText::Message(without_coauthors(message)), home, gnupg_home).await
}

async fn commit_signed_text(
    worktree_path: &Path,
    text: CommitText,
    home: Option<&Path>,
    gnupg_home: Option<&Path>,
) -> Result<bool> {
    if !commit_unsigned_text(worktree_path, &text).await? {
        return Ok(false);
    }
    sign_new_commit(worktree_path, home, gnupg_home).await?;
    Ok(true)
}

async fn sign_new_commit(worktree_path: &Path, home: Option<&Path>, gnupg_home: Option<&Path>) -> Result<()> {
    let git = GitSigning::from_config_list(&run_git_output(worktree_path, &["config", "--list", "-z"]).await?);
    let path = std::env::var_os("PATH");
    let flags: Vec<String> = match signing_choice(&git, home, path.as_deref()) {
        Signing::Unset if git.gpgsign => {
            undo_head_commit(worktree_path).await?;
            bail!("{UNSIGNED_LEFTOVERS} {SIGNING_SETUP}");
        }
        Signing::Unset => return Ok(()),
        Signing::Configured => Vec::new(),
        Signing::Loka(path) => loka_key_config(&path).into_iter().flat_map(|(name, value)| ["-c".to_string(), format!("{name}={value}")]).collect(),
    };
    let mut args = flags.clone();
    args.extend(["commit", "--amend", "--no-edit", "--allow-empty", "-S"].map(String::from));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let failure = match run_signing_git(worktree_path, &args, gnupg_home, SIGNING_LIMIT).await {
        Err(error) => format!("{error:#}"),
        Ok(()) if commit_is_signed(worktree_path, "HEAD").await? => return Ok(()),
        Ok(()) => "the commit came out without a signature".to_string(),
    };
    undo_head_commit(worktree_path).await?;
    let key = &git.key;
    if key.is_empty() && flags.is_empty() {
        bail!("{UNSIGNED_LEFTOVERS} Details: {failure}");
    }
    let which = if flags.is_empty() { format!("the configured signing key ({key})") } else { format!("the Loka signing key (~/{LOKA_SIGNING_KEY})") };
    let setup = if flags.is_empty() { String::new() } else { format!("{SIGNING_SETUP} ") };
    bail!(
        "Publish could not sign the commit for the uncommitted changes with {which}, so it did not commit them. Loka never signs with another key in its place. {setup}Details: {failure}"
    )
}

async fn run_signing_git(
    dir: &Path,
    args: &[&str],
    gnupg_home: Option<&Path>,
    limit: std::time::Duration,
) -> Result<()> {
    let mut command = workspace_command("git", dir).await;
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("GPG_TTY")
        .env_remove("SSH_ASKPASS");
    #[cfg(unix)]
    command.process_group(0);
    if let Some(gnupg_home) = gnupg_home {
        command.env("GNUPGHOME", gnupg_home);
    }
    let child = command.spawn().with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    let pid = child.id();
    let output = match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(output) => output.with_context(|| format!("Failed to run git {}", args.join(" ")))?,
        Err(_) => {
            stop_process_group(pid);
            bail!(
                "git {} did not finish within {} s, so signing was given up (the key probably waits for a passphrase)",
                args.join(" "),
                limit.as_secs()
            );
        }
    };
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

fn stop_process_group(leader: Option<u32>) {
    #[cfg(unix)]
    if let Some(group) = leader.and_then(|pid| libc::pid_t::try_from(pid).ok()).filter(|pid| *pid > 0) {
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
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
    let head = run_git_checked_output(worktree_path, &["rev-list", "--parents", "--no-walk", "HEAD"]).await?;
    let parents: Vec<String> = head.split_whitespace().skip(2).map(str::to_string).collect();
    if has_rev(worktree_path, "HEAD~1").await {
        run_git(worktree_path, &["reset", "-q", "--soft", "HEAD~1"]).await?;
    } else {
        run_git(worktree_path, &["update-ref", "-d", "HEAD"]).await?;
    }
    unstage(worktree_path).await?;
    if parents.is_empty() {
        Ok(())
    } else {
        resume_merge(worktree_path, &parents).await
    }
}

async fn resume_merge(worktree_path: &Path, parents: &[String]) -> Result<()> {
    let path = run_git_checked_output(worktree_path, &["rev-parse", "--path-format=absolute", "--git-path", "MERGE_HEAD"]).await?;
    tokio::fs::write(path.trim(), format!("{}\n", parents.join("\n"))).await.context("Could not restore the merge in progress after undoing its commit")
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

pub(super) fn without_coauthors(message: &str) -> String {
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

async fn unused_carry_branch_name(repo_path: &Path, branch: &str, protocols: Option<&str>) -> Result<String> {
    let pattern = format!("refs/heads/{branch}*");
    let mut command = workspace_command("git", repo_path).await;
    if let Some(protocols) = protocols {
        command.env("GIT_ALLOW_PROTOCOL", protocols);
    }
    let output = command
        .args(["ls-remote", "--heads", "origin", &pattern])
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        bail!("Could not inspect origin branches before carrying new work: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let remote: std::collections::HashSet<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(str::to_string)
        .collect();
    let mut name = branch.to_string();
    let mut attempt = 1;
    while remote.contains(&format!("refs/heads/{name}")) || run_git(repo_path, &["show-ref", "--quiet", &name]).await.is_ok() {
        attempt += 1;
        name = format!("{branch}-{attempt}");
    }
    Ok(name)
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

#[derive(Debug, PartialEq, Eq)]
pub enum Carry {
    Nothing,
    Branch(String),
    Conflict { commit: String, subject: String, files: Vec<String> },
}

pub async fn fetch_pr_head(worktree_path: &Path, number: i64, protocols: Option<&str>) -> Option<String> {
    let private = format!("refs/loka-pr/{number}");
    let spec = format!("+refs/pull/{number}/head:{private}");
    let mut command = workspace_command("git", worktree_path).await;
    if let Some(protocols) = protocols {
        command.env("GIT_ALLOW_PROTOCOL", protocols);
    }
    match command.args(["fetch", "--no-tags", "--refmap=", "origin", &spec]).stdin(std::process::Stdio::null()).output().await {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            warn!(number, "Could not fetch the head of pull request #{number}: {}", String::from_utf8_lossy(&output.stderr).trim());
            return None;
        }
        Err(error) => {
            warn!(number, "Could not fetch the head of pull request #{number}: {error}");
            return None;
        }
    }
    run_git_checked_output(worktree_path, &["rev-parse", "--verify", "--quiet", &format!("{private}^{{commit}}")])
        .await
        .ok()
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
}

async fn revisions_not_in(worktree_path: &Path, excluded: &[&str], filter: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec!["rev-list", "--reverse"];
    if let Some(filter) = filter {
        args.push(filter);
    }
    args.extend(["HEAD", "--not"]);
    args.extend(excluded);
    let list = run_git_checked_output(worktree_path, &args).await?;
    Ok(list.lines().map(str::trim).filter(|sha| !sha.is_empty()).map(str::to_string).collect())
}

pub async fn carry_new_work(
    worktree_path: &Path,
    base: &str,
    quest_branch: &str,
    pr_number: i64,
    pr_head: Option<&str>,
    landed: bool,
    protocols: Option<&str>,
) -> Result<Carry> {
    if !ahead_of_fresh_base_over(worktree_path, base, protocols).await? {
        return Ok(Carry::Nothing);
    }
    let Some(pr_head) = pr_head else {
        bail!("Could not fetch the head of pull request #{pr_number}, so Loka did not carry its work onto a new branch");
    };
    let base_ref = format!("origin/{base}");
    let mut seen = vec![base_ref.as_str()];
    seen.push(pr_head);
    if revisions_not_in(worktree_path, &seen, None).await?.is_empty() {
        return Ok(Carry::Nothing);
    }
    let marker = format!("{pr_number}:{pr_head}");
    let started_on = current_branch(worktree_path).await?;
    if started_on != "HEAD" && is_quest_branch(&started_on, quest_branch) {
        let key = format!("branch.{started_on}.loka-carried-from-pr");
        if run_git_checked_output(worktree_path, &["config", "--local", "--get", &key])
            .await
            .is_ok_and(|value| value.trim() == marker)
        {
            return Ok(Carry::Branch(started_on));
        }
    }
    let excluded = if landed { &seen[..] } else { &seen[..1] };
    let carried = revisions_not_in(worktree_path, excluded, Some("--no-merges")).await?;
    for commit in revisions_not_in(worktree_path, excluded, Some("--merges")).await? {
        let parents = run_git_checked_output(worktree_path, &["rev-list", "--parents", "-n", "1", &commit]).await?;
        if parents.split_whitespace().count() > 3 {
            bail!(
                "Merge commit {} has more than two parents and cannot be carried safely. Nothing was published",
                commit.chars().take(12).collect::<String>()
            );
        }
        let diff = run_git_checked_output(
            worktree_path,
            &["show", "--remerge-diff", "--format=", "--no-ext-diff", "--no-textconv", &commit],
        )
        .await?;
        if !diff.trim().is_empty() {
            bail!(
                "Merge commit {} contains manual changes that cannot be carried safely. Nothing was published",
                commit.chars().take(12).collect::<String>()
            );
        }
    }
    let started_at = run_git_checked_output(worktree_path, &["rev-parse", "HEAD"]).await?.trim().to_string();
    let branch = unused_carry_branch_name(worktree_path, quest_branch, protocols).await?;
    run_git(worktree_path, &["switch", "--quiet", "--no-track", "-c", &branch, &base_ref]).await?;
    let back = |conflict: Option<Carry>| {
        let started_on = started_on.clone();
        let started_at = started_at.clone();
        let branch = branch.clone();
        async move {
            let _ = run_git(worktree_path, &["cherry-pick", "--quit"]).await;
            run_git(worktree_path, &["reset", "--quiet", "--hard"]).await?;
            if started_on == "HEAD" {
                run_git(worktree_path, &["switch", "--quiet", "--detach", &started_at]).await?;
            } else {
                run_git(worktree_path, &["switch", "--quiet", &started_on]).await?;
            }
            run_git(worktree_path, &["branch", "--quiet", "-D", &branch]).await?;
            Ok::<Carry, anyhow::Error>(conflict.unwrap_or(Carry::Nothing))
        }
    };
    for commit in &carried {
        if run_git(worktree_path, &["cherry-pick", "--no-commit", commit]).await.is_err() {
            let files = run_git_output(worktree_path, &["diff", "--name-only", "--diff-filter=U"]).await.unwrap_or_default();
            let subject = run_git_output(worktree_path, &["log", "-1", "--format=%s", commit]).await.unwrap_or_default();
            let conflict = Carry::Conflict {
                commit: commit.chars().take(12).collect(),
                subject: subject.trim().to_string(),
                files: files.lines().map(str::trim).filter(|file| !file.is_empty()).map(str::to_string).collect(),
            };
            return back(Some(conflict)).await;
        }
        let _ = run_git(worktree_path, &["cherry-pick", "--quit"]).await;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Err(error) = commit_signed_text(worktree_path, CommitText::ReusedFrom(commit.clone()), home.as_deref(), None).await {
            back(None).await?;
            return Err(error);
        }
    }
    if run_git_checked_output(worktree_path, &["rev-parse", "HEAD"]).await? == run_git_checked_output(worktree_path, &["rev-parse", &base_ref]).await? {
        return back(None).await;
    }
    let key = format!("branch.{branch}.loka-carried-from-pr");
    if let Err(error) = run_git(worktree_path, &["config", "--local", &key, &marker]).await {
        back(None).await?;
        return Err(error);
    }
    Ok(Carry::Branch(branch))
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

async fn host_git(host: &GitHost, dir: &Path) -> Command {
    let mut command = workspace_command("git", dir).await;
    host.apply(&mut command);
    command
}

pub(super) async fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    run_git_as(&GitHost::INHERITED, dir, args).await
}

async fn run_git_as(host: &GitHost, dir: &Path, args: &[&str]) -> Result<()> {
    let output = host_git(host, dir)
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
    run_git_output_as(&GitHost::INHERITED, dir, args).await
}

async fn run_git_output_as(host: &GitHost, dir: &Path, args: &[&str]) -> Result<String> {
    let output = host_git(host, dir)
        .await
        .args(args)
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) async fn run_git_checked_output(dir: &Path, args: &[&str]) -> Result<String> {
    run_git_checked_output_as(&GitHost::INHERITED, dir, args).await
}

async fn run_git_checked_output_as(host: &GitHost, dir: &Path, args: &[&str]) -> Result<String> {
    let output = host_git(host, dir)
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

    async fn merged_pr(repo: &Path, number: u32, squash: bool) -> Result<()> {
        commit(repo, "feature").await?;
        test_git(repo, &["push", "origin", &format!("HEAD:refs/pull/{number}/head")]).await?;
        if squash {
            run_git(repo, &["switch", "main"]).await?;
            tokio::fs::write(repo.join("feature"), "feature").await?;
            run_git(repo, &["add", "feature"]).await?;
            run_git(repo, &["commit", "-m", &format!("feature (#{number})")]).await?;
            test_git(repo, &["push", "origin", "main:main"]).await?;
            run_git(repo, &["switch", "task"]).await?;
        }
        Ok(())
    }

    async fn after_pr(repo: &Path, state: &str) -> Result<super::super::github::RecordedPr> {
        super::super::github::carry_past_recorded_pr(repo, "main", "task", 7, Some(state), Some(TEST_PROTOCOLS)).await
    }

    async fn subjects_on_top_of_main(repo: &Path) -> Result<Vec<String>> {
        let log = run_git_checked_output(repo, &["log", "--reverse", "--format=%s", "origin/main..HEAD"]).await?;
        Ok(log.lines().map(str::to_string).collect())
    }

    #[tokio::test]
    async fn new_work_after_a_merged_pr_moves_onto_fresh_main_alone() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, true).await?;
        commit(&repo.0, "tweak").await?;
        let super::super::github::RecordedPr::Final { state, carried } = after_pr(&repo.0, "MERGED").await? else {
            bail!("a merged pull request is final");
        };
        assert_eq!((state.as_str(), carried.as_deref()), ("merged", Some("task-2")));
        assert_eq!(current_branch(&repo.0).await?, "task-2");
        assert_eq!(subjects_on_top_of_main(&repo.0).await?, ["tweak"], "only the commit the merged PR lacked is carried");
        let parent = run_git_checked_output(&repo.0, &["rev-parse", "HEAD~1"]).await?;
        assert_eq!(parent, run_git_checked_output(&repo.0, &["rev-parse", "origin/main"]).await?);
        assert_eq!(tokio::fs::read_to_string(repo.0.join("tweak")).await?, "tweak");
        Ok(())
    }

    #[tokio::test]
    async fn carried_commits_are_signed_with_the_configured_key() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, true).await?;
        commit(&repo.0, "tweak").await?;
        let key = repo.0.join("signing-key");
        ssh_key(&key).await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &key.to_string_lossy()]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        assert!(matches!(after_pr(&repo.0, "MERGED").await?, super::super::github::RecordedPr::Final { carried: Some(_), .. }));
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        ensure_signed_commits(&repo.0, "main").await?;
        Ok(())
    }

    #[tokio::test]
    async fn new_work_after_a_closed_pr_carries_the_unmerged_work_too() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, false).await?;
        commit(&repo.0, "tweak").await?;
        let super::super::github::RecordedPr::Final { state, carried } = after_pr(&repo.0, "CLOSED").await? else {
            bail!("a closed pull request is final");
        };
        assert_eq!((state.as_str(), carried.as_deref()), ("closed", Some("task-2")));
        assert_eq!(subjects_on_top_of_main(&repo.0).await?, ["feature", "tweak"]);
        Ok(())
    }

    #[tokio::test]
    async fn retrying_a_carry_reuses_the_same_branch() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, true).await?;
        commit(&repo.0, "tweak").await?;
        let super::super::github::RecordedPr::Final { carried, .. } = after_pr(&repo.0, "MERGED").await? else {
            bail!("a merged pull request is final");
        };
        assert_eq!(carried.as_deref(), Some("task-2"));
        let first_head = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        let marker = run_git_checked_output(&repo.0, &["config", "--local", "--get", "branch.task-2.loka-carried-from-pr"]).await?;
        let pr_head = run_git_checked_output(&repo.0, &["rev-parse", "refs/loka-pr/7"]).await?;
        assert_eq!(marker.trim(), format!("7:{}", pr_head.trim()));

        let super::super::github::RecordedPr::Final { carried, .. } = after_pr(&repo.0, "MERGED").await? else {
            bail!("a merged pull request is final");
        };
        assert_eq!(carried.as_deref(), Some("task-2"));
        assert_eq!(current_branch(&repo.0).await?, "task-2");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, first_head);
        assert!(run_git(&repo.0, &["show-ref", "--quiet", "refs/heads/task-3"]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn carrying_avoids_a_remote_only_branch() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, true).await?;
        commit(&repo.0, "tweak").await?;
        test_git(&repo.0, &["push", "origin", "main:refs/heads/task-2"]).await?;

        let super::super::github::RecordedPr::Final { carried, .. } = after_pr(&repo.0, "MERGED").await? else {
            bail!("a merged pull request is final");
        };
        assert_eq!(carried.as_deref(), Some("task-3"));
        assert_eq!(current_branch(&repo.0).await?, "task-3");
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_pr_head_ignores_a_stale_cached_ref_and_does_not_carry() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        commit(&repo.0, "feature").await?;
        run_git(&repo.0, &["update-ref", "refs/loka-pr/7", "HEAD"]).await?;
        let head = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;

        let error = match after_pr(&repo.0, "CLOSED").await {
            Ok(_) => bail!("a missing pull request head must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Could not fetch the head of pull request #7"), "{error}");
        assert_eq!(current_branch(&repo.0).await?, "task");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, head);
        assert!(run_git(&repo.0, &["show-ref", "--quiet", "refs/heads/task-2"]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn a_clean_merge_can_be_carried() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, false).await?;
        run_git(&repo.0, &["switch", "-c", "side"]).await?;
        commit(&repo.0, "side-work").await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        run_git(&repo.0, &["merge", "--no-ff", "side", "-m", "merge side"]).await?;

        let super::super::github::RecordedPr::Final { carried, .. } = after_pr(&repo.0, "CLOSED").await? else {
            bail!("a closed pull request is final");
        };
        assert_eq!(carried.as_deref(), Some("task-2"));
        assert_eq!(tokio::fs::read_to_string(repo.0.join("side-work")).await?, "side-work");
        Ok(())
    }

    #[tokio::test]
    async fn a_merge_with_manual_changes_is_not_carried() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, false).await?;
        run_git(&repo.0, &["switch", "-c", "side"]).await?;
        commit(&repo.0, "side-work").await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        run_git(&repo.0, &["merge", "--no-commit", "--no-ff", "side"]).await?;
        tokio::fs::write(repo.0.join("merge-only"), "manual").await?;
        run_git(&repo.0, &["add", "merge-only"]).await?;
        run_git(&repo.0, &["commit", "-m", "merge side manually"]).await?;
        let head = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;

        let error = match after_pr(&repo.0, "CLOSED").await {
            Ok(_) => bail!("a merge with manual changes must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("contains manual changes that cannot be carried safely"), "{error}");
        assert!(error.contains("Nothing was published"), "{error}");
        assert_eq!(current_branch(&repo.0).await?, "task");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, head);
        assert_eq!(tokio::fs::read_to_string(repo.0.join("merge-only")).await?, "manual");
        assert!(run_git(&repo.0, &["show-ref", "--quiet", "refs/heads/task-2"]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn a_final_pr_without_new_work_publishes_nothing() -> Result<()> {
        for (state, squash) in [("MERGED", true), ("CLOSED", false)] {
            let (_remote, repo) = repo_with_remote().await?;
            merged_pr(&repo.0, 7, squash).await?;
            let head = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
            let super::super::github::RecordedPr::Final { carried, .. } = after_pr(&repo.0, state).await? else {
                bail!("{state} is final");
            };
            assert_eq!(carried, None, "{state}");
            assert_eq!(current_branch(&repo.0).await?, "task", "{state}");
            assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, head, "{state}");
            assert!(run_git(&repo.0, &["show-ref", "--quiet", "refs/heads/task-2"]).await.is_err(), "{state}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_open_pr_keeps_the_usual_publish() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, false).await?;
        commit(&repo.0, "tweak").await?;
        for state in [Some("OPEN"), None] {
            assert!(matches!(
                super::super::github::carry_past_recorded_pr(&repo.0, "main", "task", 7, state, Some(TEST_PROTOCOLS)).await?,
                super::super::github::RecordedPr::Open
            ));
        }
        assert_eq!(current_branch(&repo.0).await?, "task");
        Ok(())
    }

    #[tokio::test]
    async fn a_replay_conflict_fails_loudly_and_leaves_the_quest_branch_alone() -> Result<()> {
        let (_remote, repo) = repo_with_remote().await?;
        merged_pr(&repo.0, 7, true).await?;
        run_git(&repo.0, &["switch", "main"]).await?;
        tokio::fs::write(repo.0.join("feature"), "changed on main").await?;
        run_git(&repo.0, &["commit", "-am", "main edit"]).await?;
        test_git(&repo.0, &["push", "origin", "main:main"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        tokio::fs::write(repo.0.join("feature"), "changed by the quest").await?;
        run_git(&repo.0, &["commit", "-am", "quest edit"]).await?;
        let head = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        let error = match after_pr(&repo.0, "MERGED").await {
            Ok(_) => bail!("a conflicting replay must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Pull request #7 is already merged") && error.contains("\"quest edit\" conflicts in feature"), "{error}");
        assert!(error.contains("Nothing was published"), "{error}");
        assert_eq!(current_branch(&repo.0).await?, "task");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, head);
        assert!(pending_changes(&repo.0, "--untracked-files=all").await?.trim().is_empty());
        assert!(run_git(&repo.0, &["show-ref", "--quiet", "refs/heads/task-2"]).await.is_err());
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
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&empty_home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        ensure_signed_commits(&repo.0, "main").await?;
        assert!(!commit_isolated(&repo.0, "feat: nothing", Some(&empty_home)).await?);
        Ok(())
    }

    #[tokio::test]
    async fn signing_uses_the_configured_key_else_the_loka_key_and_ignores_every_other_key() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-sign-home-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        for other in ["karna_signing", "signing_key_active", "id_ed25519"] {
            ssh_key(&home.join(".ssh").join(other)).await?;
        }
        let path = std::env::var_os("PATH");
        let path = path.as_deref();
        let none = GitSigning::from_config_list("");
        assert_eq!(signing_choice(&none, Some(&home), path), Signing::Unset);
        assert_eq!(signing_choice(&none, None, path), Signing::Unset);
        ssh_key(&home.join(LOKA_SIGNING_KEY)).await?;
        assert_eq!(signing_choice(&none, Some(&home), path), Signing::Loka(home.join(".ssh/loka_signing")));
        let explicit = GitSigning::from_config_list("user.signingkey\n/missing/key.pub\0gpg.format\nssh\0");
        assert_eq!(signing_choice(&explicit, Some(&home), path), Signing::Configured, "an explicit key is never swapped, even a broken one");
        assert!(explicit.problem(Some(&home), path).unwrap().contains("does not exist"));
        let no_gpg = GitSigning::from_config_list("commit.gpgsign\ntrue\0gpg.program\n/nowhere/gpg\0");
        assert_eq!(signing_choice(&no_gpg, Some(&home), path), Signing::Loka(home.join(".ssh/loka_signing")), "gpgsign with no gpg cannot sign, so the Loka key signs");
        tokio::fs::remove_dir_all(&home).await?;
        Ok(())
    }

    #[test]
    fn a_mac_runner_creates_the_loka_key_once_and_only_when_nothing_else_signs() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-key-home-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home)?;
        assert!(!ensure_loka_key_in(&home, true)?, "a configured signer is left alone");
        assert!(!home.join(LOKA_SIGNING_KEY).exists());
        assert!(ensure_loka_key_in(&home, false)?);
        let key = home.join(LOKA_SIGNING_KEY);
        assert!(key.is_file() && home.join(".ssh/loka_signing.pub").is_file());
        let first = std::fs::read(&key)?;
        assert!(!ensure_loka_key_in(&home, false)?, "an existing Loka key is never replaced");
        assert_eq!(std::fs::read(&key)?, first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(home.join(".ssh"))?.permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&key)?.permissions().mode() & 0o077, 0, "the private key is private");
        }
        assert_eq!(signing_choice(&GitSigning::default(), Some(&home), std::env::var_os("PATH").as_deref()), Signing::Loka(key));
        std::fs::remove_dir_all(&home)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_loka_key_is_reported_and_never_replaced() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-key-home-{}", Uuid::new_v4()));
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(&ssh)?;
        let key = home.join(LOKA_SIGNING_KEY);
        let public = ssh.join("loka_signing.pub");

        std::fs::write(&public, "ssh-ed25519 AAAA orphan")?;
        let error = ensure_loka_key_in(&home, false).unwrap_err().to_string();
        assert!(error.contains("without its private key"), "{error}");
        assert!(std::fs::symlink_metadata(&key).is_err(), "no key is created next to an orphan public key");
        assert_eq!(std::fs::read_to_string(&public)?, "ssh-ed25519 AAAA orphan");
        std::fs::remove_file(&public)?;

        std::os::unix::fs::symlink(ssh.join("nowhere"), &public)?;
        assert!(ensure_loka_key_in(&home, false).is_err(), "a dangling public key link counts as an orphan");
        assert!(std::fs::symlink_metadata(&key).is_err());
        std::fs::remove_file(&public)?;

        let target = ssh.join("moved_away");
        std::os::unix::fs::symlink(&target, &key)?;
        let error = ensure_loka_key_in(&home, false).unwrap_err().to_string();
        assert!(error.contains("not a key file"), "{error}");
        assert!(!target.exists(), "ssh-keygen never follows a dangling link");
        assert!(std::fs::symlink_metadata(&public).is_err());
        assert_eq!(std::fs::read_link(&key)?, target);
        std::fs::remove_file(&key)?;

        let real = ssh.join("real_key");
        std::fs::write(&real, "private")?;
        std::os::unix::fs::symlink(&real, &key)?;
        assert!(!ensure_loka_key_in(&home, false)?, "a link to a real key is an existing key");
        assert_eq!(std::fs::read_to_string(&real)?, "private");
        assert!(std::fs::symlink_metadata(&public).is_err());

        std::fs::remove_dir_all(&home)?;
        Ok(())
    }

    #[test]
    fn git_signing_reads_the_config_list_and_finds_a_missing_signer() {
        let git = GitSigning::from_config_list("user.name\nT\0commit.gpgsign\nyes\0gpg.format\nSSH\0gpg.ssh.program\n/opt/ssh-keygen\0user.signingkey\n~/.ssh/k.pub\0");
        assert_eq!(git, GitSigning { key: "~/.ssh/k.pub".into(), gpgsign: true, format: "ssh".into(), program: "/opt/ssh-keygen".into() });
        let bare = GitSigning::from_config_list("commit.gpgsign\0");
        assert!(bare.gpgsign && bare.format == "openpgp" && bare.key.is_empty());
        let empty_path = std::ffi::OsString::new();
        assert!(!bare.configured(Some(&empty_path)), "gpgsign without an installed gpg cannot sign");
        assert!(bare.problem(None, Some(&empty_path)).unwrap().contains("gpg is not installed"));
        let ssh_only = GitSigning::from_config_list("commit.gpgsign\ntrue\0gpg.format\nssh\0");
        assert!(!ssh_only.configured(std::env::var_os("PATH").as_deref()), "ssh signing needs a key");
    }

    fn session_command(repo: &Path, home: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("git");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("TMPDIR", std::env::temp_dir())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(repo);
        command
    }

    fn session_config(command: &tokio::process::Command) -> std::collections::BTreeMap<String, String> {
        let env: std::collections::BTreeMap<String, String> = command
            .as_std()
            .get_envs()
            .filter_map(|(name, value)| Some((name.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned())))
            .collect();
        let count = env.get("GIT_CONFIG_COUNT").and_then(|count| count.parse::<usize>().ok()).unwrap_or(0);
        (0..count).map(|i| (env[&format!("GIT_CONFIG_KEY_{i}")].clone(), env[&format!("GIT_CONFIG_VALUE_{i}")].clone())).collect()
    }

    #[test]
    fn signing_pins_copy_the_configured_settings_or_the_loka_key() {
        let configured = "gpg.format\nssh\0user.signingkey\n~/.ssh/loka_signing.pub\0commit.gpgsign\ntrue\0gpg.ssh.allowedsignersfile\n/s/allowed\0user.name\nT\0";
        let pins = signing_pins(configured, None, None);
        let names: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["gpg.format", "user.signingKey", "commit.gpgSign", "gpg.ssh.allowedSignersFile"]);
        assert_eq!(pins[1].1, "~/.ssh/loka_signing.pub");
        assert_eq!(pins[3].1, "/s/allowed");

        let key_only = signing_pins("user.signingkey\n/k.pub\0", None, None);
        assert_eq!(key_only, [("user.signingKey".to_string(), "/k.pub".to_string())]);

        assert!(signing_pins("user.name\nT\0", None, None).is_empty(), "nothing configured and no Loka key means nothing is written");

        let home = std::env::temp_dir().join(format!("loka-pins-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(LOKA_SIGNING_KEY), "key").unwrap();
        let loka = signing_pins("user.name\nT\0", Some(&home), None);
        std::fs::remove_dir_all(&home).ok();
        assert_eq!(
            loka,
            [
                ("gpg.format".to_string(), "ssh".to_string()),
                ("user.signingkey".to_string(), home.join(LOKA_SIGNING_KEY).display().to_string()),
                ("commit.gpgsign".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn scopes_split_into_inherited_and_repo_level_settings() {
        let scoped = "global\0gpg.format\nssh\0worktree\0gpg.format\nopenpgp\0local\0extensions.worktreeconfig\ntrue\0command\0safe.directory\n*\0";
        let (inherited, repo_level) = split_by_scope(scoped);
        assert_eq!(inherited, "gpg.format\nssh\0safe.directory\n*\0");
        assert_eq!(repo_level, "gpg.format\nopenpgp\0extensions.worktreeconfig\ntrue\0");
    }

    #[tokio::test]
    async fn quest_worktrees_sign_without_the_global_git_config() -> Result<()> {
        let base = temp_path("loka-pin");
        let home = base.join("home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        let key = home.join(".ssh/loka_signing");
        ssh_key(&key).await?;
        let public = format!("{}.pub", key.display());
        let signers = base.join("allowed_signers");
        tokio::fs::write(&signers, "").await?;
        tokio::fs::write(
            home.join(".gitconfig"),
            format!(
                "[user]\n\tname = T\n\temail = t@example.com\n[gpg]\n\tformat = ssh\n[gpg \"ssh\"]\n\tallowedSignersFile = {}\n[user]\n\tsigningkey = {public}\n[commit]\n\tgpgsign = true\n",
                signers.display()
            ),
        )
        .await?;
        let host = GitHost::isolated(&home, None);

        let main = base.join("main");
        tokio::fs::create_dir_all(&main).await?;
        run_git_as(&host, &main, &["init", "--initial-branch=main"]).await?;
        tokio::fs::write(main.join("base"), "base").await?;
        run_git_as(&host, &main, &["add", "base"]).await?;
        run_git_as(&host, &main, &["commit", "-q", "-m", "base"]).await?;
        let quest = base.join("quest");
        run_git_as(&host, &main, &["worktree", "add", "-b", "quest", &quest.to_string_lossy()]).await?;
        let sibling = base.join("sibling");
        run_git_as(&host, &main, &["worktree", "add", "-b", "sibling", &sibling.to_string_lossy()]).await?;

        pin_signing_in(&quest, &host).await?;
        pin_signing_in(&quest, &host).await?;

        let without_global = |dir: &Path, args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_remove("GIT_CONFIG_GLOBAL")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", base.join("empty-home"))
                .env("GIT_AUTHOR_NAME", "T")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "T")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        };
        let worktree_config = without_global(&quest, &["config", "--worktree", "--list"]);
        let text = String::from_utf8_lossy(&worktree_config.stdout).to_lowercase();
        assert!(text.contains("gpg.format=ssh") && text.contains("commit.gpgsign=true"), "{text}");
        assert!(text.contains(&format!("user.signingkey={}", public.to_lowercase())), "{text}");
        assert!(text.contains("gpg.ssh.allowedsignersfile"), "{text}");
        assert_eq!(text.matches("gpg.format").count(), 1, "pinning twice must not duplicate entries: {text}");

        tokio::fs::write(quest.join("work"), "work").await?;
        assert!(without_global(&quest, &["add", "work"]).status.success());
        let committed = without_global(&quest, &["commit", "-q", "-m", "feat: work"]);
        assert!(committed.status.success(), "{}", String::from_utf8_lossy(&committed.stderr));
        let commit = without_global(&quest, &["cat-file", "commit", "HEAD"]);
        assert!(has_signature(&String::from_utf8_lossy(&commit.stdout)), "a commit made with GIT_CONFIG_GLOBAL unset must stay signed");

        let sibling_config = without_global(&sibling, &["config", "--get", "commit.gpgsign"]);
        assert!(!sibling_config.status.success(), "other worktrees of the repo are not touched");
        let shared = std::fs::read_to_string(main.join(".git/config"))?;
        assert!(!shared.contains("signingkey"), "the shared config never receives the signing settings:\n{shared}");
        std::fs::remove_dir_all(&base).ok();
        Ok(())
    }

    #[tokio::test]
    async fn sessions_get_the_loka_key_only_when_nothing_else_is_configured() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "--unset", "commit.gpgsign"]).await?;
        let home = repo.0.join(".git/home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(".ssh/karna_signing")).await?;

        let mut command = session_command(&repo.0, &home);
        assert_eq!(sign_with_loka_key(&mut command, &repo.0), Signing::Unset);
        assert!(session_config(&command).is_empty(), "another project's key must never be picked");

        ssh_key(&home.join(LOKA_SIGNING_KEY)).await?;
        let mut command = session_command(&repo.0, &home);
        command.env("GIT_CONFIG_COUNT", "1").env("GIT_CONFIG_KEY_0", "safe.directory").env("GIT_CONFIG_VALUE_0", "*");
        let key = home.join(LOKA_SIGNING_KEY);
        assert_eq!(sign_with_loka_key(&mut command, &repo.0), Signing::Loka(key.clone()));
        let config = session_config(&command);
        assert_eq!(config.get("safe.directory").map(String::as_str), Some("*"));
        assert_eq!(config.get("gpg.format").map(String::as_str), Some("ssh"));
        assert_eq!(config.get("user.signingkey"), Some(&key.display().to_string()));
        assert_eq!(config.get("commit.gpgsign").map(String::as_str), Some("true"));

        tokio::fs::write(repo.0.join("work"), "work").await?;
        let added = session_command(&repo.0, &home).args(["add", "work"]).output().await?;
        assert!(added.status.success());
        command.args(["commit", "-q", "-m", "feat: session work"]);
        let committed = command.output().await?;
        assert!(committed.status.success(), "{}", String::from_utf8_lossy(&committed.stderr));
        assert!(commit_is_signed(&repo.0, "HEAD").await?, "a plain git commit in the session is signed with the Loka key");

        run_git(&repo.0, &["config", "user.signingkey", "/owner/key.pub"]).await?;
        let mut command = session_command(&repo.0, &home);
        assert_eq!(sign_with_loka_key(&mut command, &repo.0), Signing::Configured);
        assert!(session_config(&command).is_empty(), "the owner's own key always wins");
        Ok(())
    }

    #[tokio::test]
    async fn publish_never_signs_with_another_key_found_in_home() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        let home = repo.0.join(".git/home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(".ssh/karna_signing")).await?;
        ssh_key(&home.join(".ssh/signing_key_active")).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&home)).await?);
        assert!(!commit_is_signed(&repo.0, "HEAD").await?, "no configured key and no Loka key means unsigned, never a borrowed key");

        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        tokio::fs::write(repo.0.join("more"), "more").await?;
        let error = commit_isolated(&repo.0, "feat: more", Some(&home)).await.unwrap_err();
        assert!(error.to_string().contains("never signs with another key"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        Ok(())
    }

    #[tokio::test]
    async fn publish_signs_leftovers_with_the_loka_key_even_with_gpgsign_off() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        let home = repo.0.join("home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(".ssh/loka_signing")).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        let log = run_git_checked_output(&repo.0, &["log", "--format=%s", "-1"]).await?;
        assert_eq!(log.trim(), "feat: leftovers");
        Ok(())
    }

    async fn commit_isolated(repo: &Path, message: &str, home: Option<&Path>) -> Result<bool> {
        let gnupg = repo.join(".git/gnupg");
        tokio::fs::create_dir_all(&gnupg).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&gnupg, std::fs::Permissions::from_mode(0o700)).await?;
        }
        commit_signed_with_home(repo, message, home, Some(&gnupg)).await
    }

    #[tokio::test]
    async fn publish_sees_untracked_leftovers_when_status_hides_them() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        run_git(&repo.0, &["config", "status.showUntrackedFiles", "no"]).await?;
        tokio::fs::create_dir_all(repo.0.join("new/dir")).await?;
        tokio::fs::write(repo.0.join("new/dir/left"), "left").await?;
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&repo.0.join(".git/home"))).await?);
        let tree = run_git_checked_output(&repo.0, &["ls-tree", "-r", "--name-only", "HEAD"]).await?;
        assert!(tree.lines().any(|path| path == "new/dir/left"), "{tree}");
        Ok(())
    }

    #[tokio::test]
    async fn a_signing_program_that_waits_for_a_passphrase_is_given_up() -> Result<()> {
        let repo = test_repo().await?;
        let pid_file = repo.0.join(".git/signer.pid");
        let helper_file = repo.0.join(".git/helper.pid");
        let signer = repo.0.join(".git/waiting-signer");
        tokio::fs::write(
            &signer,
            format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\necho $$ > '{}'\nwait\n", helper_file.display(), pid_file.display()),
        )
        .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&signer, std::fs::Permissions::from_mode(0o755)).await?;
        }
        tokio::fs::write(repo.0.join("left"), "left").await?;
        run_git(&repo.0, &["add", "left"]).await?;
        let program = format!("gpg.program={}", signer.display());
        let started = std::time::Instant::now();
        let error = run_signing_git(
            &repo.0,
            &["-c", "gpg.format=openpgp", "-c", &program, "-c", "user.signingkey=waiting", "commit", "-S", "-m", "feat: wait"],
            None,
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("did not finish within 2 s"), "{error:#}");
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        for file in [&pid_file, &helper_file] {
            let pid = tokio::fs::read_to_string(file).await?;
            let pid: libc::pid_t = pid.trim().parse()?;
            let mut alive = true;
            for _ in 0..50 {
                alive = unsafe { libc::kill(pid, 0) } == 0;
                if !alive {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(!alive, "{} still runs after signing was given up", file.display());
        }
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
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&repo.0.join("home"))).await?);
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
        let error = commit_isolated(&repo.0, "feat: leftovers", Some(&repo.0.join("home")))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("will not commit them unsigned"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        assert!(staged(&repo.0).await?.is_empty());
        assert!(repo.0.join("sprite-stack.ts").is_file());
        let error = commit_isolated(&repo.0, "feat: leftovers", None).await.unwrap_err();
        assert!(error.to_string().contains("will not commit them unsigned"), "{error:#}");
        Ok(())
    }

    #[tokio::test]
    async fn gpgsign_without_a_signer_refuses_with_the_setup_card_or_uses_the_loka_key() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        run_git(&repo.0, &["config", "gpg.program", "/nowhere/gpg"]).await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        let home = repo.0.join(".git/home");
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(".ssh/karna_signing")).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        let error = commit_isolated(&repo.0, "feat: leftovers", Some(&home)).await.unwrap_err();
        assert!(error.to_string().contains("setup:signing"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        assert!(staged(&repo.0).await?.is_empty());

        ssh_key(&home.join(LOKA_SIGNING_KEY)).await?;
        assert!(commit_isolated(&repo.0, "feat: leftovers", Some(&home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        Ok(())
    }

    #[tokio::test]
    async fn publish_refuses_when_the_signing_key_does_not_sign() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        tokio::fs::write(repo.0.join("left"), "left").await?;
        let error = commit_isolated(&repo.0, "feat: leftovers", Some(&repo.0.join("home")))
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
        assert!(commit_isolated(&repo.0, "feat: leftovers", None).await.is_err());
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

    async fn merging_main(home: &Path) -> Result<TestRepo> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        tokio::fs::create_dir_all(home.join(".ssh")).await?;
        ssh_key(&home.join(LOKA_SIGNING_KEY)).await?;
        tokio::fs::write(repo.0.join("quest"), "quest").await?;
        run_git(&repo.0, &["add", "quest"]).await?;
        run_git(&repo.0, &["commit", "-m", "quest work"]).await?;
        run_git(&repo.0, &["switch", "-q", "-c", "upstream", "main"]).await?;
        tokio::fs::write(repo.0.join("main-only"), "main").await?;
        run_git(&repo.0, &["add", "main-only"]).await?;
        run_git(&repo.0, &["commit", "-m", "main moved"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/main", "HEAD"]).await?;
        run_git(&repo.0, &["switch", "-q", "task"]).await?;
        run_git(&repo.0, &["merge", "--no-commit", "--no-ff", "origin/main"]).await?;
        tokio::fs::write(repo.0.join("leftover"), "after the merge").await?;
        Ok(repo)
    }

    async fn parents(repo: &Path) -> Result<Vec<String>> {
        Ok(run_git_checked_output(repo, &["log", "-1", "--format=%P"]).await?.split_whitespace().map(String::from).collect())
    }

    #[tokio::test]
    async fn a_merge_with_main_keeps_both_parents_when_publish_signs_it() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-merge-home-{}", Uuid::new_v4()));
        let repo = merging_main(&home).await?;
        let quest = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        assert!(commit_isolated(&repo.0, "feat: quest", Some(&home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        let main = run_git_checked_output(&repo.0, &["rev-parse", "origin/main"]).await?;
        assert_eq!(parents(&repo.0).await?, vec![quest.trim().to_string(), main.trim().to_string()]);
        assert!(!has_rev(&repo.0, "MERGE_HEAD").await);
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn the_api_fallback_signs_the_unsigned_merge_in_place() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-merge-home-{}", Uuid::new_v4()));
        let repo = merging_main(&home).await?;
        assert!(commit_unsigned(&repo.0, "feat: quest").await?);
        let unsigned = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?.trim().to_string();
        let before = parents(&repo.0).await?;
        let tree = run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?;
        assert_eq!(before.len(), 2);
        assert!(!commit_is_signed(&repo.0, "HEAD").await?);
        let committed = PublishCommit { api: true, leftover: Some(unsigned) };
        let gnupg = repo.0.join(".git/gnupg");
        tokio::fs::create_dir_all(&gnupg).await?;
        sign_leftover_in_place(&repo.0, &committed, Some(&home), Some(&gnupg)).await?;
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        assert_eq!(parents(&repo.0).await?, before, "signing in place keeps the merge parent");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?, tree);
        let message = run_git_checked_output(&repo.0, &["log", "-1", "--format=%s"]).await?;
        assert_eq!(message.trim(), "feat: quest");
        let moved = PublishCommit { api: true, leftover: Some(before[0].clone()) };
        let error = sign_leftover_in_place(&repo.0, &moved, Some(&home), Some(&gnupg)).await.unwrap_err();
        assert!(error.to_string().contains("no longer the newest commit"), "{error:#}");
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn a_merge_that_cannot_be_signed_stays_in_progress() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-merge-home-{}", Uuid::new_v4()));
        let repo = merging_main(&home).await?;
        let quest = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        let main = run_git_checked_output(&repo.0, &["rev-parse", "origin/main"]).await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;
        let error = commit_isolated(&repo.0, "feat: quest", Some(&home)).await.unwrap_err();
        assert!(error.to_string().contains("could not sign"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, quest);
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "MERGE_HEAD"]).await?, main, "the merge is still in progress");
        assert!(staged(&repo.0).await?.is_empty());
        assert!(repo.0.join("leftover").is_file() && repo.0.join("main-only").is_file());

        run_git(&repo.0, &["config", "user.signingkey", ""]).await?;
        assert!(commit_isolated(&repo.0, "feat: quest", Some(&home)).await?);
        assert!(commit_is_signed(&repo.0, "HEAD").await?);
        assert_eq!(parents(&repo.0).await?, vec![quest.trim().to_string(), main.trim().to_string()]);
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn an_octopus_merge_that_cannot_be_signed_keeps_every_parent() -> Result<()> {
        let home = std::env::temp_dir().join(format!("loka-octopus-home-{}", Uuid::new_v4()));
        let repo = test_repo().await?;
        let quest = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        run_git(&repo.0, &["switch", "-c", "side-one"]).await?;
        commit(&repo.0, "one").await?;
        let one = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        run_git(&repo.0, &["switch", "-c", "side-two"]).await?;
        commit(&repo.0, "two").await?;
        let two = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        run_git(&repo.0, &["switch", "task"]).await?;
        let tree = run_git_checked_output(&repo.0, &["rev-parse", "HEAD^{tree}"]).await?;
        let octopus = run_git_checked_output(
            &repo.0,
            &["commit-tree", tree.trim(), "-p", quest.trim(), "-p", one.trim(), "-p", two.trim(), "-m", "octopus"],
        )
        .await?;
        run_git(&repo.0, &["update-ref", "HEAD", octopus.trim()]).await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", &repo.0.join("missing-key").to_string_lossy()]).await?;

        assert!(sign_new_commit(&repo.0, Some(&home), None).await.is_err());
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, quest);
        let merge_head = run_git_checked_output(&repo.0, &["rev-parse", "--path-format=absolute", "--git-path", "MERGE_HEAD"]).await?;
        let heads: Vec<String> = tokio::fs::read_to_string(merge_head.trim()).await?.lines().map(str::to_string).collect();
        assert_eq!(heads, [one.trim(), two.trim()]);
        assert!(staged(&repo.0).await?.is_empty());
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn publish_refuses_changes_another_session_left_behind() -> Result<()> {
        let repo = test_repo().await?;
        let before = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        tokio::fs::write(repo.0.join("theirs"), "someone else").await?;
        let error = commit_for_publish(&repo.0, "o/r", "feat: mine", Some("a follow-up (50c87771) of another session of this quest (984d02ea)"))
            .await
            .err()
            .expect("publish refuses");
        assert!(error.to_string().contains("did not make") && error.to_string().contains("50c87771"), "{error:#}");
        assert_eq!(run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?, before);
        assert!(staged(&repo.0).await?.is_empty());
        assert!(repo.0.join("theirs").is_file());
        Ok(())
    }

    #[tokio::test]
    async fn a_quest_on_another_quests_branch_gets_its_own_worktree() -> Result<()> {
        let repo = test_repo().await?;
        tokio::fs::write(repo.0.join("first"), "first quest").await?;
        run_git(&repo.0, &["add", "first"]).await?;
        run_git(&repo.0, &["commit", "-m", "first quest"]).await?;
        run_git(&repo.0, &["update-ref", "refs/remotes/origin/task", "HEAD"]).await?;
        let tip = run_git_checked_output(&repo.0, &["rev-parse", "HEAD"]).await?;
        let second = std::env::temp_dir().join(format!("loka-dependent-{}", Uuid::new_v4()));
        create_worktree(&repo.0, &second, "second-quest", "task").await?;
        assert_ne!(second, repo.0);
        assert_eq!(run_git_checked_output(&second, &["rev-parse", "HEAD"]).await?, tip, "the dependent quest starts from the other quest's branch");
        assert_eq!(current_branch(&second).await?, "second-quest");
        assert_eq!(current_branch(&repo.0).await?, "task", "the first quest's folder keeps its own branch");
        tokio::fs::write(second.join("second"), "second quest").await?;
        assert!(pending_changes(&repo.0, "--untracked-files=all").await?.trim().is_empty(), "work in one folder never shows in the other");
        let _ = run_git(&repo.0, &["worktree", "remove", "--force", &second.to_string_lossy()]).await;
        Ok(())
    }

    #[tokio::test]
    async fn leftovers_for_the_api_path_are_committed_without_a_signature() -> Result<()> {
        let repo = test_repo().await?;
        run_git(&repo.0, &["config", "commit.gpgsign", "true"]).await?;
        run_git(&repo.0, &["config", "gpg.format", "ssh"]).await?;
        run_git(&repo.0, &["config", "user.signingkey", "/nonexistent/key"]).await?;
        assert!(!commit_unsigned(&repo.0, "feat: nothing").await?);
        tokio::fs::write(repo.0.join("leftover"), "work").await?;
        assert!(commit_unsigned(&repo.0, "feat: work\n\nCo-Authored-By: A <a@b>").await?);
        assert!(!commit_is_signed(&repo.0, "HEAD").await?);
        let message = run_git_checked_output(&repo.0, &["log", "-1", "--format=%B"]).await?;
        assert!(!message.contains("Co-Authored-By"), "{message}");
        undo_head_commit(&repo.0).await?;
        assert!(pending_changes(&repo.0, "--untracked-files=all").await?.contains("leftover"));
        Ok(())
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

        let home = temp_path("loka-home");
        let error = configure_git_signing(Some(&signing), &GitHost::isolated(&home, None)).await.unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    fn temp_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
    }

    async fn global_git_config(host: &GitHost, key: &str) -> Option<String> {
        let mut git = Command::new("git");
        let output = host.apply(&mut git).args(["config", "--global", key]).output().await.ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn fake_gh(script: &str) -> Result<(PathBuf, std::ffi::OsString)> {
        let bin = temp_path("loka-bin");
        tokio::fs::create_dir_all(&bin).await?;
        tokio::fs::write(bin.join("gh"), script).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).await?;
        }
        let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())))?;
        Ok((bin, path))
    }

    async fn empty_repo(prefix: &str) -> Result<TestRepo> {
        let repo = TestRepo(temp_path(prefix));
        tokio::fs::create_dir_all(&repo.0).await?;
        run_git(&repo.0, &["init", "--initial-branch=main"]).await?;
        Ok(repo)
    }

    #[tokio::test]
    async fn startup_never_rewrites_a_global_git_identity() -> Result<()> {
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        tokio::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Existing User\n\temail = me@example.com\n",
        )
        .await?;
        let host = GitHost::isolated(&home, None);

        configure_git_auth("test-token", &host).await?;

        assert_eq!(global_git_config(&host, "user.name").await.as_deref(), Some("Existing User"));
        assert_eq!(global_git_config(&host, "user.email").await.as_deref(), Some("me@example.com"));
        let credentials = home.join(".git-credentials");
        assert_eq!(tokio::fs::read_to_string(&credentials).await?, "https://x-access-token:test-token@github.com\n");
        assert_eq!(
            global_git_config(&host, "credential.helper").await,
            Some(format!("store --file={}", credentials.display()))
        );
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    pending.push(entry.path());
                }
                found.push((entry.path(), meta.len(), meta.modified().ok()));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn workspace_tests_leave_the_home_they_run_under_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let base = temp_path("loka-sentinel");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".gitconfig"), "[user]\n\tname = Sentinel\n\temail = sentinel@example.com\n").unwrap();
        let before = snapshot(&home);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["git::workspace::tests::", "--skip", "git::workspace::tests::workspace_tests_leave_the_home_they_run_under_untouched"])
            .env("HOME", &home)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        let after = snapshot(&home);
        std::fs::remove_dir_all(&base).ok();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("test result: ok.") && !stdout.contains(" 0 passed"), "{stdout}");
        assert_eq!(before, after, "a workspace test wrote into the home it ran under");
    }

    #[test]
    fn the_running_process_keeps_its_own_git_home_and_credentials() {
        let host = GitHost::INHERITED;
        assert_eq!(host.credentials(), Path::new("/tmp/.git-credentials"));
        assert_eq!(host.home(), PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())));
        let mut command = Command::new("git");
        host.apply(&mut command);
        assert_eq!(command.as_std().get_envs().count(), 0, "production commands keep the environment they inherit");
    }

    #[tokio::test]
    async fn repo_identity_is_left_untouched_when_present() -> Result<()> {
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        let host = GitHost::isolated(&home, None);

        let repo = empty_repo("loka-ident").await?;
        run_git(&repo.0, &["config", "user.name", "Repo Owner"]).await?;
        run_git(&repo.0, &["config", "user.email", "owner@repo.example"]).await?;

        ensure_repo_identity(&repo.0, "test-token", &host).await?;

        assert_eq!(git_config_get(&repo.0, "user.name", &host).await, "Repo Owner");
        assert_eq!(git_config_get(&repo.0, "user.email", &host).await, "owner@repo.example");
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn owner_identity_comes_from_gh_repo_level_only() -> Result<()> {
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        let (bin, path) = fake_gh(
            "#!/bin/sh\nif [ \"$1\" = \"api\" ] && [ \"$2\" = \"user\" ]; then\n  printf '%s' '{\"login\":\"mona\",\"name\":\"Mona Lisa\",\"email\":null}'\n  exit 0\nfi\nexit 1\n",
        )
        .await?;
        let host = GitHost::isolated(&home, Some(path));

        let repo = empty_repo("loka-ident").await?;
        ensure_repo_identity(&repo.0, "test-token", &host).await?;

        assert_eq!(git_config_get(&repo.0, "user.name", &host).await, "Mona Lisa");
        assert_eq!(git_config_get(&repo.0, "user.email", &host).await, "mona@users.noreply.github.com");
        assert_eq!(global_git_config(&host, "user.name").await, None);
        assert_eq!(global_git_config(&host, "user.email").await, None);

        tokio::fs::write(repo.0.join("authored"), "authored").await?;
        run_git_as(&host, &repo.0, &["add", "authored"]).await?;
        run_git_as(&host, &repo.0, &["commit", "-m", "authored"]).await?;
        let author = run_git_checked_output_as(&host, &repo.0, &["log", "-1", "--format=%an <%ae>"]).await?;
        assert_eq!(author.trim(), "Mona Lisa <mona@users.noreply.github.com>");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&bin);
        Ok(())
    }

    #[tokio::test]
    async fn missing_owner_identity_falls_back_without_global_writes() -> Result<()> {
        let home = temp_path("loka-home");
        tokio::fs::create_dir_all(&home).await?;
        let (bin, path) = fake_gh("#!/bin/sh\nexit 1\n").await?;
        let host = GitHost::isolated(&home, Some(path));

        let repo = empty_repo("loka-ident").await?;
        ensure_repo_identity(&repo.0, "test-token", &host).await?;

        assert_eq!(git_config_get(&repo.0, "user.name", &host).await, FALLBACK_AUTHOR_NAME);
        assert_eq!(git_config_get(&repo.0, "user.email", &host).await, FALLBACK_AUTHOR_EMAIL);
        assert_eq!(global_git_config(&host, "user.name").await, None);
        assert_eq!(global_git_config(&host, "user.email").await, None);
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

    async fn plant_logins(repo: &Path) -> Result<()> {
        for dir in [".mcp-auth/owner/server", "nested/.mcp-auth", "***", "nested/***/deep"] {
            tokio::fs::create_dir_all(repo.join(dir)).await?;
            tokio::fs::write(repo.join(dir).join("tokens.json"), "secret").await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn mcp_logins_are_never_committed() -> Result<()> {
        let repo = test_repo().await?;
        plant_logins(&repo.0).await?;
        assert!(!commit_signed_with_home(&repo.0, "feat: nothing real", None, None).await?);
        tokio::fs::write(repo.0.join("real.txt"), "real").await?;
        tokio::fs::write(repo.0.join("nested").join("kept.txt"), "kept").await?;
        assert!(commit_signed_with_home(&repo.0, "feat: real work", None, None).await?);
        let files = run_git_checked_output(&repo.0, &["ls-tree", "-r", "--name-only", "HEAD"]).await?;
        let files: Vec<&str> = files.lines().collect();
        assert_eq!(files, ["base", "nested/kept.txt", "real.txt"]);
        assert!(repo.0.join(".mcp-auth/owner/server/tokens.json").exists());
        Ok(())
    }
}
