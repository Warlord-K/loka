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

pub(crate) struct RunTemp(pub PathBuf);

impl Drop for RunTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn run_temp() -> std::io::Result<RunTemp> {
    let dir = crate::cli::temp_dir().join(format!("loka-cursor-tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    let dir = RunTemp(std::fs::canonicalize(&dir)?);
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
    let mut write: Vec<PathBuf> = temp.map(Path::to_path_buf).into_iter().collect();
    if let Some(home) = &home {
        read.extend(HOME_TOOLCHAINS.iter().map(|rel| home.join(rel)));
        write.extend(crate::claude::sandbox::writable_in(home));
        if let Some(path) = std::env::var_os("PATH") {
            read.extend(std::env::split_paths(&path).filter(|dir| dir.starts_with(home)));
        }
    }
    write.extend(crate::claude::sandbox::git_write_roots(working_dir).into_iter().map(PathBuf::from));
    if let Some(home) = home.as_ref() {
        read.push(home.join(crate::claude::sandbox::OWNER_SSH));
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
        assert!(policy(Mode::Off, &dir, &[], None).is_none());
    }
}
