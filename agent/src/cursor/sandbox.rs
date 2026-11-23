use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::claude::sandbox::Mode;

const HOME_TOOLCHAINS: &[&str] = &[
    ".gitconfig",
    ".config/git",
    ".gitignore_global",
    ".ssh/known_hosts",
    ".ssh/config",
    ".cargo/bin",
    ".cargo/env",
    ".rustup",
    ".nvm",
    ".volta",
    ".bun/bin",
    ".deno",
    ".pyenv",
    ".rbenv",
    ".local/bin",
    ".local/share/mise",
    ".asdf",
    ".sdkman",
    "go/bin",
];

pub(crate) struct RunTemp(pub PathBuf, bool);

impl Drop for RunTemp {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub(crate) fn run_temp(working_dir: &Path) -> std::io::Result<RunTemp> {
    if let Some(owned) = crate::cli::owned_temp_dir(working_dir) {
        return Ok(RunTemp(owned, true));
    }
    let dir = crate::cli::temp_dir().join(format!("loka-cursor-tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    let dir = RunTemp(std::fs::canonicalize(&dir)?, false);
    crate::cli::give_to_job_user(&dir.0)?;
    Ok(dir)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

fn existing(paths: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
    let mut out: Vec<String> = paths.into_iter().filter(|p| p.exists()).map(|p| p.display().to_string()).collect();
    out.sort();
    out.dedup();
    out
}

pub(crate) fn policy(mode: Mode, working_dir: &Path, readable: &[PathBuf], temp: Option<&Path>) -> Option<Value> {
    if mode == Mode::Off {
        return None;
    }
    let home = home();
    let mut read: Vec<PathBuf> = readable.to_vec();
    read.extend(crate::cli::launcher_alias_dir());
    let mut write: Vec<PathBuf> = temp.map(crate::cli::private_temp_grants).unwrap_or_default();
    if let Some(home) = &home {
        read.extend(HOME_TOOLCHAINS.iter().map(|rel| home.join(rel)));
        write.extend(crate::claude::sandbox::writable_in(home));
        if let Some(path) = std::env::var_os("PATH") {
            read.extend(std::env::split_paths(&path).filter(|dir| dir.starts_with(home)));
        }
    }
    write.extend(crate::claude::sandbox::git_write_roots(working_dir).into_iter().map(PathBuf::from));
    if let Some(home) = home.as_ref() {
        read.extend(crate::claude::sandbox::owner_ssh_readable(Some(home)));
        read.extend(std::env::var_os("SSH_AUTH_SOCK").filter(|v| !v.is_empty()).map(PathBuf::from));
        write.extend(crate::claude::sandbox::OWNER_WRITABLE_HOME.iter().map(|rel| home.join(rel)));
    }
    let network = if mode == Mode::Strict {
        json!({ "default": "deny", "allow": crate::claude::sandbox::GITHUB_HOSTS })
    } else {
        json!({ "default": "allow" })
    };
    Some(json!({
        "type": "workspace_readwrite",
        "readBoundary": "workspace",
        "additionalReadPaths": existing(read),
        "additionalReadwritePaths": existing(write),
        "networkPolicy": network,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_runs_keep_the_quest_or_session_folder_and_never_delete_it() {
        let quest = uuid::Uuid::new_v4();
        let owned = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(quest));
        std::fs::create_dir_all(&owned).unwrap();
        let cwd = PathBuf::from(format!("/r/workspaces/tasks/{quest}/world"));
        let temp = run_temp(&cwd).unwrap();
        assert_eq!(temp.0, std::fs::canonicalize(&owned).unwrap());
        let listed = policy(Mode::Auto, &cwd, &[], Some(&temp.0)).unwrap();
        let writable: Vec<&str> = listed["additionalReadwritePaths"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(writable.contains(&temp.0.to_string_lossy().as_ref()), "{writable:?}");
        drop(temp);
        assert!(owned.is_dir());
        let _ = std::fs::remove_dir_all(&owned);

        let session = uuid::Uuid::new_v4();
        let owned = crate::cli::private_temp_root().join(crate::cli::session_temp_name(session));
        std::fs::create_dir_all(&owned).unwrap();
        let temp = crate::cli::SESSION_TEMP.sync_scope(session, || run_temp(Path::new("/Users/a/Projects/app"))).unwrap();
        assert_eq!(temp.0, std::fs::canonicalize(&owned).unwrap());
        drop(temp);
        assert!(owned.is_dir());
        let _ = std::fs::remove_dir_all(&owned);
    }

    #[test]
    fn cursor_runs_with_no_owner_get_a_throwaway_folder() {
        let temp = run_temp(Path::new("/Users/a/Projects/app")).unwrap();
        let dir = temp.0.clone();
        assert!(dir.is_dir() && dir.to_string_lossy().contains("loka-cursor-tmp-"));
        drop(temp);
        assert!(!dir.exists());
    }

    #[test]
    fn credentials_stay_outside_the_read_boundary() {
        let dir = std::env::temp_dir();
        let auto = policy(Mode::Auto, &dir, &[], None).unwrap();
        assert_eq!(auto["readBoundary"], "workspace");
        assert_eq!(auto["networkPolicy"]["default"], "allow");
        assert_eq!(policy(Mode::Strict, &dir, &[], None).unwrap()["networkPolicy"]["default"], "deny");
        let listed: Vec<&str> = auto["additionalReadPaths"].as_array().unwrap().iter().chain(auto["additionalReadwritePaths"].as_array().unwrap()).filter_map(Value::as_str).collect();
        for secret in [".aws", ".codex", ".config/gh", ".kube", ".npmrc", ".cursor/agent-cli-state"] {
            assert!(!listed.iter().any(|path| path.contains(secret)), "{secret} must stay hidden: {listed:?}");
        }
        assert!(!listed.iter().any(|path| path.ends_with("/.ssh") || path.ends_with(".ssh/id_ed25519") || path.ends_with(".ssh/id_rsa")), "owners read only signing keys, config and known_hosts in ssh: {listed:?}");
        assert!(policy(Mode::Off, &dir, &[], None).is_none());
    }
}
