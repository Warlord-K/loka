use anyhow::{bail, Result};

pub fn validate_repo(repo: &str) -> Result<&str> {
    let parts = repo.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 100
                || matches!(*part, "." | "..")
                || part.starts_with('-')
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        bail!("repository must be an owner/name GitHub repository");
    }
    Ok(repo)
}

pub fn github_repo(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let bare = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    let bare = bare.strip_suffix(".git").unwrap_or(bare);
    Ok(validate_repo(bare)?.to_string())
}

pub fn validate_git_ref(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 240
        || value.starts_with('-')
        || value.contains("..")
        || value.contains("@{")
        || value.bytes().any(|byte| byte <= b' ' || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\'))
    {
        bail!("invalid Git ref");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_must_be_plain_owner_name() {
        assert!(validate_repo("Warlord-K/world").is_ok());
        assert!(validate_repo("owner/../../repo").is_err());
        assert!(validate_repo("a/..").is_err());
        assert!(validate_repo("a/.").is_err());
        assert!(validate_repo("-x/repo").is_err());
        assert!(validate_repo("https://github.com/a/b").is_err());
        assert_eq!(github_repo("https://github.com/a/b.git").unwrap(), "a/b");
        assert_eq!(github_repo("a/b").unwrap(), "a/b");
        assert!(github_repo("https://evil.example/x/--config=core.fsmonitor=sh").is_err());
        assert!(github_repo("https://github.com/a/--upload-pack=x").is_err());
    }

    #[test]
    fn refs_cannot_be_options() {
        assert!(validate_git_ref("world/task-42").is_ok());
        assert!(validate_git_ref("--upload-pack=bad").is_err());
        assert!(validate_git_ref("a..b").is_err());
        assert!(validate_git_ref("a b").is_err());
    }
}
