use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedPr<'a> {
    url: &'a str,
    repo: &'a str,
    number: i32,
}

impl<'a> TrackedPr<'a> {
    pub fn parse(url: &'a str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        if scheme != "https" {
            return None;
        }
        let (host, path) = rest.split_once('/')?;
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        let (repo, number) = path.rsplit_once("/pull/")?;
        validate_repo(repo).ok()?;
        if number.is_empty()
            || number.starts_with('0')
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let number = number.parse::<i32>().ok().filter(|number| *number > 0)?;
        Some(Self { url, repo, number })
    }

    pub fn url(self) -> &'a str {
        self.url
    }

    pub fn repo(self) -> &'a str {
        self.repo
    }

    pub fn number(self) -> i32 {
        self.number
    }

    pub fn same_identity(self, other: Self) -> bool {
        self.number == other.number && self.repo.eq_ignore_ascii_case(other.repo)
    }
}

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
        assert!(validate_repo("acme/castle").is_ok());
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

    #[test]
    fn tracked_pull_requests_keep_repo_and_number_together() {
        let tracked = TrackedPr::parse("https://github.com/Acme/App/pull/21").unwrap();
        assert_eq!(tracked.repo(), "Acme/App");
        assert_eq!(tracked.number(), 21);
        assert_eq!(tracked.url(), "https://github.com/Acme/App/pull/21");
        assert!(tracked.same_identity(TrackedPr::parse("https://github.com/acme/app/pull/21").unwrap()));
        assert!(!tracked.same_identity(TrackedPr::parse("https://github.com/other/app/pull/21").unwrap()));
        assert!(TrackedPr::parse("https://github.com/acme/app/pull/0").is_none());
        assert!(TrackedPr::parse("https://github.com/acme/app/pull/021").is_none());
        assert!(TrackedPr::parse("https://github.com/acme/app/pull/2147483648").is_none());
        assert!(TrackedPr::parse("https://github.com/acme/app/pull/21/files").is_none());
    }
}
