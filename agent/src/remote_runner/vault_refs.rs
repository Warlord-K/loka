use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Result};
use loka_shared::environment::valid_vault_reference;

const READ_TIMEOUT: Duration = Duration::from_secs(120);
const TRUSTED_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

const MISSING: &str = "the 1Password CLI (op) is not installed on this Mac. Install it with brew install 1password-cli, then turn on Integrate with 1Password CLI in the 1Password app (Settings > Developer).";
const LOCKED: &str = "1Password is locked or op is not signed in. Unlock the 1Password app on this Mac and turn on Integrate with 1Password CLI (Settings > Developer).";
const NOT_FOUND: &str = "1Password has no vault, item or field at that reference. Check the reference in Settings > Keys.";
const TIMED_OUT: &str = "op did not answer within 2 minutes. Unlock 1Password on this Mac and approve the request when it asks.";
const EMPTY: &str = "1Password returned an empty value for it.";
const INVALID: &str = "it is not a valid op://vault/item/field reference.";

pub(crate) fn find_op() -> Option<PathBuf> {
    let trusted = TRUSTED_DIRS.iter().map(PathBuf::from);
    let on_path = std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>()).unwrap_or_default();
    trusted.chain(on_path).map(|dir| dir.join("op")).find(|path| path.is_file())
}

pub(crate) struct Resolver {
    op: Option<PathBuf>,
    cache: HashMap<String, String>,
}

impl Resolver {
    pub(crate) fn new(op: Option<PathBuf>) -> Self {
        Self { op, cache: HashMap::new() }
    }

    pub(crate) async fn resolve(&mut self, refs: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
        let mut values = BTreeMap::new();
        for (name, reference) in refs {
            let value = self.read(reference).await.map_err(|hint| failure(name, hint))?;
            values.insert(name.clone(), value);
        }
        Ok(values)
    }

    async fn read(&mut self, reference: &str) -> std::result::Result<String, &'static str> {
        if !valid_vault_reference(reference) {
            return Err(INVALID);
        }
        if let Some(value) = self.cache.get(reference) {
            return Ok(value.clone());
        }
        let op = self.op.as_deref().ok_or(MISSING)?;
        let value = read_with(op, reference).await?;
        self.cache.insert(reference.to_string(), value.clone());
        Ok(value)
    }
}

pub(crate) fn failure(name: &str, hint: &str) -> anyhow::Error {
    anyhow!("Vault reference for {name} could not be read: {hint}")
}

async fn read_with(op: &Path, reference: &str) -> std::result::Result<String, &'static str> {
    let mut command = tokio::process::Command::new(op);
    command
        .args(["read", "--", reference])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => MISSING,
        _ => "op could not start on this Mac.",
    })?;
    let output = match tokio::time::timeout(READ_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return Err("op stopped before it answered."),
        Err(_) => return Err(TIMED_OUT),
    };
    if !output.status.success() {
        return Err(explain(&String::from_utf8_lossy(&output.stderr)));
    }
    let mut value = String::from_utf8(output.stdout).map_err(|_| "1Password returned a value that is not text.")?;
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    if value.is_empty() {
        return Err(EMPTY);
    }
    Ok(value)
}

fn explain(stderr: &str) -> &'static str {
    let text = stderr.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| text.contains(word));
    if has(&["isn't an item", "isn't a vault", "isn't a field", "could not find", "not found", "no item", "invalid secret reference"]) {
        NOT_FOUND
    } else if has(&["locked", "sign in", "signin", "signed in", "authoriz", "unauthenticated", "connect", "session", "biometric", "integration"]) {
        LOCKED
    } else {
        "op read failed. Check the reference and that 1Password is unlocked on this Mac."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loka_shared::cli_errors::{classify, FailureKind};
    use std::os::unix::fs::PermissionsExt;

    fn fake_op(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("op");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loka-vault-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn refs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(name, reference)| (name.to_string(), reference.to_string())).collect()
    }

    #[tokio::test]
    async fn missing_op_fails_with_the_vault_cause_and_a_hint() {
        let mut resolver = Resolver::new(None);
        let error = resolver.resolve(&refs(&[("GH_TOKEN", "op://Work/GitHub/token")])).await.unwrap_err();
        let text = format!("{error:#}");
        assert!(text.starts_with("Vault reference for GH_TOKEN could not be read:"));
        assert!(text.contains("brew install 1password-cli"));
        assert_eq!(classify(&text).map(|failure| failure.kind), Some(FailureKind::Vault));
    }

    #[tokio::test]
    async fn bad_references_never_reach_op() {
        let dir = scratch();
        let marker = dir.join("called");
        let op = fake_op(&dir, &format!("touch '{}'\necho value", marker.display()));
        let mut resolver = Resolver::new(Some(op));
        for reference in ["op://-Work/GitHub/token", "op://Work/GitHub", "op://Work/../token", "op://Work/Git;rm/token", " op://Work/GitHub/token"] {
            let error = resolver.resolve(&refs(&[("KEY", reference)])).await.unwrap_err();
            assert!(format!("{error:#}").contains(INVALID), "{reference}");
        }
        assert!(!marker.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn values_come_from_argv_and_are_read_once_per_run() {
        let dir = scratch();
        let count = dir.join("count");
        let op = fake_op(
            &dir,
            &format!(
                "echo x >> '{}'\n[ \"$1\" = read ] && [ \"$2\" = -- ] || exit 9\nprintf 'secret-for-%s\\n' \"$3\"",
                count.display()
            ),
        );
        let mut resolver = Resolver::new(Some(op));
        let wanted = refs(&[("A", "op://Work/GitHub/token"), ("B", "op://Work/GitHub/token"), ("C", "op://Work/Fly/api key")]);
        let values = resolver.resolve(&wanted).await.unwrap();
        assert_eq!(values["A"], "secret-for-op://Work/GitHub/token");
        assert_eq!(values["B"], values["A"]);
        assert_eq!(values["C"], "secret-for-op://Work/Fly/api key");
        resolver.resolve(&wanted).await.unwrap();
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
        assert_eq!(Resolver::new(Some(dir.join("op"))).resolve(&wanted).await.unwrap(), values);
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn failures_never_carry_the_value_or_op_output() {
        let dir = scratch();
        let op = fake_op(&dir, "echo hunter2-value\necho '[ERROR] item hunter2-value isn'\"'\"'t an item' >&2\nexit 1");
        let error = Resolver::new(Some(op)).resolve(&refs(&[("KEY", "op://Work/Missing/password")])).await.unwrap_err();
        let text = format!("{error:#}");
        assert!(!text.contains("hunter2"));
        assert!(text.contains(NOT_FOUND));
        assert_eq!(classify(&text).map(|failure| failure.kind), Some(FailureKind::Vault));
        let op = fake_op(&dir, "echo '[ERROR] You are not currently signed in.' >&2\nexit 1");
        let text = format!("{:#}", Resolver::new(Some(op)).resolve(&refs(&[("KEY", "op://Work/Item/password")])).await.unwrap_err());
        assert!(text.contains(LOCKED));
        let op = fake_op(&dir, "printf '\\n'");
        let text = format!("{:#}", Resolver::new(Some(op)).resolve(&refs(&[("KEY", "op://Work/Item/password")])).await.unwrap_err());
        assert!(text.contains(EMPTY));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
