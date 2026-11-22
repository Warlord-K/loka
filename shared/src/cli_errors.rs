use chrono::{DateTime, Duration, TimeZone, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    UsageLimit,
    RateLimited,
    Overloaded,
    Auth,
    MissingScope,
    Billing,
    ContextTooLong,
    Network,
    CliMissing,
    CliOutdated,
    Permissions,
    Vault,
    DiskFull,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::UsageLimit => "usage_limit",
            FailureKind::RateLimited => "rate_limited",
            FailureKind::Overloaded => "overloaded",
            FailureKind::Auth => "auth",
            FailureKind::MissingScope => "missing_scope",
            FailureKind::Billing => "billing",
            FailureKind::ContextTooLong => "context_too_long",
            FailureKind::Network => "network",
            FailureKind::CliMissing => "cli_missing",
            FailureKind::CliOutdated => "cli_outdated",
            FailureKind::Permissions => "permissions",
            FailureKind::Vault => "vault",
            FailureKind::DiskFull => "disk_full",
        }
    }

    pub fn is_limit(self) -> bool {
        matches!(self, FailureKind::UsageLimit | FailureKind::RateLimited)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliFailure {
    pub kind: FailureKind,
    pub resets_at: Option<DateTime<Utc>>,
}

pub const FRIENDLY_MARK: &str = " Details: ";

pub const ERROR_TEXT_MAX: usize = 8_000;

pub const STATUS_DETAIL_MAX: usize = 1_000;

impl CliFailure {
    pub fn retry_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let fallback = match self.kind {
            FailureKind::UsageLimit => Duration::minutes(30),
            FailureKind::RateLimited => Duration::minutes(5),
            _ => return None,
        };
        let at = self.resets_at.filter(|at| *at > now).map(|at| at + Duration::seconds(30));
        Some(at.unwrap_or(now + fallback).min(now + Duration::hours(6)))
    }

    pub fn summary(&self) -> String {
        let resets = self
            .resets_at
            .map(|at| format!(" It resets at {} UTC.", at.format("%b %-d %H:%M")))
            .unwrap_or_default();
        match self.kind {
            FailureKind::UsageLimit => format!(
                "The CLI account hit its usage limit.{resets} Waiting work resumes after it resets, or switch the agent to another CLI or model."
            ),
            FailureKind::RateLimited => format!(
                "The model provider is rate limiting requests.{resets} It usually clears within a few minutes."
            ),
            FailureKind::Overloaded => {
                "The model provider is overloaded or returned a server error. Retrying usually works.".to_string()
            }
            FailureKind::Auth => {
                "The CLI is not signed in or its key was rejected. Sign in again on the execution device (claude /login, codex login) or check the key in You > Environment.".to_string()
            }
            FailureKind::MissingScope => {
                "The CLI is signed in, but its login is not allowed to make this call (missing scopes). Update the Loka app on the execution device. If the agent uses an OpenAI API key, give that key write access to the Responses API or use an unrestricted key.".to_string()
            }
            FailureKind::Billing => {
                "The provider account is out of credits or billing failed. Top it up, then retry.".to_string()
            }
            FailureKind::ContextTooLong => {
                "The conversation got too long for the model. Reset the agent's context or start a fresh quest.".to_string()
            }
            FailureKind::Network => {
                "The execution device could not reach the model provider. Check its connection, then retry.".to_string()
            }
            FailureKind::CliMissing => {
                "The CLI is not installed on the execution device, or Loka cannot find it. Install it there (Claude Code: curl -fsSL https://claude.ai/install.sh | bash), then restart the Loka app or runner.".to_string()
            }
            FailureKind::CliOutdated => {
                "The CLI on the execution device is too old for Loka. Update it there (claude update), then retry.".to_string()
            }
            FailureKind::Permissions => {
                "The CLI refused the permission mode Loka asked for, so it could not edit files or run commands. Loka never switches to full access on its own: pick a model with auto mode, set the agent's approval to Full, or set it to Read only. If the device's Claude settings turn full access off, allow it there.".to_string()
            }
            FailureKind::DiskFull => {
                "The disk on the device that ran this work is full, so nothing could be written. Loka clears closed quest workspaces and temp folders when space runs low, and does not start new runs until 5 GB are free, but there was nothing left to clear. Free up space on that device (the bell lists its biggest folders), or clear closed quests under You > Storage, then retry.".to_string()
            }
            FailureKind::Vault => {
                "A key the agent uses is a 1Password reference (op://) that could not be read. Vault references are read only on your own Mac: install the 1Password CLI there (brew install 1password-cli), turn on 1Password > Settings > Developer > Integrate with 1Password CLI, unlock 1Password, check the reference in You > Environment, then retry.".to_string()
            }
        }
    }
}

const VAULT_FAILURE: &str = "Vault reference for ";

pub fn classify(error: &str) -> Option<CliFailure> {
    let raw = error.split(FRIENDLY_MARK).last().unwrap_or(error);
    let lower = raw.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let vault_failure = |line: &str| line.trim_start().starts_with(VAULT_FAILURE) || line.contains(&format!(": {VAULT_FAILURE}"));
    let kind = if raw.lines().any(vault_failure) {
        FailureKind::Vault
    } else if has(&["no space left on device", "enospc", "os error 28", "disk quota exceeded"]) {
        FailureKind::DiskFull
    } else if has(&["is not installed on this device", "env: node: no such file or directory", "claude: command not found"]) {
        FailureKind::CliMissing
    } else if has(&["is too old for loka", "error: unknown option '--"]) {
        FailureKind::CliOutdated
    } else if has(&["would not start in auto mode", "cannot start in auto mode", "would not start with full access", "auto mode is unavailable"]) {
        FailureKind::Permissions
    } else if has(&["missing scopes:", "missing scope:", "insufficient permissions for this operation"])
        && has(&["api.responses", "api.model.", "/responses", "openai", "chatgpt"])
    {
        FailureKind::MissingScope
    } else if has(&["credit balance is too low", "insufficient_quota", "billing_error", "payment required"]) {
        FailureKind::Billing
    } else if has(&[
        "usage limit",
        "hit your limit",
        "5-hour limit reached",
        "weekly limit reached",
        "session limit reached",
        "out of extra usage",
        "quota exceeded",
        "exceeded your current quota",
    ]) {
        FailureKind::UsageLimit
    } else if has(&["rate limit", "rate_limit", "too many requests", "status: 429", "status 429", "error: 429", "code 429"]) {
        FailureKind::RateLimited
    } else if has(&[
        "invalid api key",
        "authentication_error",
        "please run /login",
        "run /login to re-authenticate",
        "oauth token has expired",
        "oauth session expired",
        "refresh token is no longer valid",
        "failed to authenticate",
        "not logged in",
        "401 unauthorized",
        "api error: 401",
        "status: 401",
        "invalid x-api-key",
        "invalid_api_key",
        "incorrect api key",
        "api key not valid",
        "permission_error",
        "no auth credentials",
    ]) || ["401", "403"].iter().any(|code| has_status(&lower, code))
    {
        FailureKind::Auth
    } else if has(&[
        "prompt is too long",
        "context_length_exceeded",
        "context length",
        "context window",
        "input is too long",
        "exceed context limit",
        "exceeds the context",
        "conversation too long",
    ]) {
        FailureKind::ContextTooLong
    } else if has(&["overloaded", "api error: 5", "status: 5", "internal server error", "service unavailable", "bad gateway"]) {
        FailureKind::Overloaded
    } else if has(&[
        "econnreset",
        "econnrefused",
        "enotfound",
        "etimedout",
        "stream disconnected",
        "network error",
        "fetch failed",
        "connection reset",
        "error sending request",
        "printed nothing for",
    ]) {
        FailureKind::Network
    } else {
        return None;
    };
    let resets_at = if kind.is_limit() { reset_time(raw, Utc::now()) } else { None };
    Some(CliFailure { kind, resets_at })
}

fn has_status(lower: &str, code: &str) -> bool {
    const BEFORE: &[&str] = &["status: ", "status ", "error: ", "code ", "http ", "failed: ", "error "];
    const AFTER: &[&str] = &[" unauthorized", " forbidden", " status code"];
    lower.match_indices(code).any(|(at, _)| {
        let head = &lower[..at];
        let tail = &lower[at + code.len()..];
        let bounded = !head.ends_with(|c: char| c.is_ascii_digit()) && !tail.starts_with(|c: char| c.is_ascii_digit());
        bounded && (BEFORE.iter().any(|b| head.ends_with(b)) || AFTER.iter().any(|a| tail.starts_with(a)))
    })
}

pub fn is_context_too_long(error: &str) -> bool {
    classify(error).is_some_and(|failure| failure.kind == FailureKind::ContextTooLong)
}

pub fn is_rejected_key(error: &str) -> bool {
    classify(error).is_some_and(|failure| matches!(failure.kind, FailureKind::Auth | FailureKind::MissingScope))
}

pub fn describe(error: &str) -> String {
    if error.contains(FRIENDLY_MARK) {
        return error_text(error);
    }
    match classify(error) {
        Some(failure) => format!("{}{FRIENDLY_MARK}{}", failure.summary(), error_text(error.trim())),
        None => error_text(error),
    }
}

pub fn failure_payload(task_id: Option<uuid::Uuid>, error: &str, session_id: uuid::Uuid, kind: &str) -> serde_json::Value {
    let failure = classify(error);
    serde_json::json!({
        "task_id": task_id,
        "error": error_text(error),
        "session_id": session_id,
        "run_kind": kind,
        "failure": failure.as_ref().map(|f| f.kind.as_str()),
        "resets_at": failure.and_then(|f| f.resets_at),
    })
}

pub fn status_detail(error: &str) -> String {
    match classify(error) {
        Some(failure) => failure.summary(),
        None => clip(error, STATUS_DETAIL_MAX),
    }
}

pub fn error_text(error: &str) -> String {
    clip(error, ERROR_TEXT_MAX)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("...");
    out
}

fn reset_time(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some((_, tail)) = raw.split_once('|') {
        let digits: String = tail.trim().chars().take_while(char::is_ascii_digit).collect();
        if let Ok(epoch) = digits.parse::<i64>() {
            let epoch = if epoch > 10_000_000_000 { epoch / 1000 } else { epoch };
            if let Some(at) = Utc.timestamp_opt(epoch, 0).single() {
                return Some(at);
            }
        }
    }
    let lower = raw.to_ascii_lowercase();
    for marker in ["try again in ", "retry after ", "resets in ", "retry in "] {
        if let Some(idx) = lower.find(marker) {
            if let Some(after) = relative(&lower[idx + marker.len()..]) {
                return Some(now + after);
            }
        }
    }
    None
}

fn relative(text: &str) -> Option<Duration> {
    let mut total = Duration::zero();
    let mut found = false;
    let mut words = text.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).peekable();
    while let Some(word) = words.next() {
        let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
        let Ok(n) = digits.parse::<i64>() else {
            if word == "and" {
                continue;
            }
            break;
        };
        let unit_inline = &word[digits.len()..];
        let unit = if unit_inline.is_empty() { words.next().unwrap_or("") } else { unit_inline };
        let step = match unit.trim_end_matches(['.', ')']) {
            u if u.starts_with('d') => Duration::days(n),
            u if u.starts_with('h') => Duration::hours(n),
            u if u.starts_with("ms") => Duration::milliseconds(n),
            u if u.starts_with('m') => Duration::minutes(n),
            u if u.starts_with('s') => Duration::seconds(n),
            _ => break,
        };
        total += step;
        found = true;
    }
    found.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_too_long_wording_is_a_context_failure() {
        for raw in [
            "prompt is too long: 215000 tokens > 200000 maximum",
            "Claude Code returned error: Prompt is too long",
            "Codex turn failed: input exceeds the context window of this model",
            "Request would exceed context limit",
            "Error: conversation too long, please start a new one",
        ] {
            assert!(is_context_too_long(raw), "{raw}");
            assert!(is_context_too_long(&describe(raw)), "{raw}");
        }
        assert!(!is_context_too_long("Claude AI usage limit reached"));
    }

    #[test]
    fn claude_usage_limit_with_epoch() {
        let failure = classify("Claude Code returned error: Claude AI usage limit reached|1761930000").unwrap();
        assert_eq!(failure.kind, FailureKind::UsageLimit);
        assert_eq!(failure.resets_at, Utc.timestamp_opt(1761930000, 0).single());
    }

    #[test]
    fn codex_usage_limit_with_relative_reset() {
        let now = Utc::now();
        let failure = classify("Codex turn failed: You've hit your usage limit. Upgrade to Pro or try again in 2 hours 5 minutes.").unwrap();
        assert_eq!(failure.kind, FailureKind::UsageLimit);
        let at = failure.resets_at.unwrap();
        assert!((at - now - Duration::minutes(125)).num_seconds().abs() < 5);
    }

    #[test]
    fn retry_waits_for_reset_but_never_more_than_six_hours() {
        let now = Utc::now();
        let soon = CliFailure { kind: FailureKind::UsageLimit, resets_at: Some(now + Duration::minutes(10)) };
        assert_eq!(soon.retry_at(now), Some(now + Duration::minutes(10) + Duration::seconds(30)));
        let far = CliFailure { kind: FailureKind::UsageLimit, resets_at: Some(now + Duration::days(4)) };
        assert_eq!(far.retry_at(now), Some(now + Duration::hours(6)));
        let unknown = CliFailure { kind: FailureKind::RateLimited, resets_at: None };
        assert_eq!(unknown.retry_at(now), Some(now + Duration::minutes(5)));
        let auth = CliFailure { kind: FailureKind::Auth, resets_at: None };
        assert_eq!(auth.retry_at(now), None);
    }

    #[test]
    fn other_kinds() {
        let kind = |e: &str| classify(e).map(|f| f.kind);
        assert_eq!(kind("API Error: 529 {\"type\":\"overloaded_error\"}"), Some(FailureKind::Overloaded));
        assert_eq!(kind("Invalid API key · Please run /login"), Some(FailureKind::Auth));
        assert_eq!(kind("Credit balance is too low"), Some(FailureKind::Billing));
        assert_eq!(kind("API Error: 400 prompt is too long: 210000 tokens > 200000 maximum"), Some(FailureKind::ContextTooLong));
        assert_eq!(kind("stream disconnected before completion: error sending request"), Some(FailureKind::Network));
        assert_eq!(kind("exceeded retry limit, last status: 429 Too Many Requests"), Some(FailureKind::RateLimited));
        assert_eq!(kind("cargo test failed: 3 tests"), None);
    }

    #[test]
    fn a_full_disk_is_named_instead_of_left_raw() {
        let kind = |text: &str| classify(text).map(|failure| failure.kind);
        assert_eq!(kind("failed to create workspace: No space left on device (os error 28)"), Some(FailureKind::DiskFull));
        assert_eq!(kind("git worktree add failed: fatal: could not write: ENOSPC"), Some(FailureKind::DiskFull));
        assert_eq!(kind("API Error: 529 {\"type\":\"overloaded_error\"}"), Some(FailureKind::Overloaded));
        let text = describe("Claude Code exited with code 1: No space left on device");
        assert!(text.starts_with("The disk on the device that ran this work is full"), "{text}");
        assert!(text.contains("No space left on device"));
    }

    #[test]
    fn claude_setup_failures_explain_the_fix() {
        let kind = |e: &str| classify(e).map(|f| f.kind);
        assert_eq!(kind("Claude Code returned error: Failed to authenticate: OAuth session expired and could not be refreshed"), Some(FailureKind::Auth));
        assert_eq!(kind("API Error: OAuth refresh token is no longer valid; run /login to re-authenticate"), Some(FailureKind::Auth));
        assert_eq!(kind("Claude Code is not installed on this device, or Loka cannot find the claude command."), Some(FailureKind::CliMissing));
        assert_eq!(kind("Claude Code exited with code 127: env: node: No such file or directory"), Some(FailureKind::CliMissing));
        assert_eq!(kind("Claude Code exited with code 1: error: unknown option '--effort'"), Some(FailureKind::CliOutdated));
        assert_eq!(kind("Claude Code 1.0.3 is too old for Loka (it has no --tools)."), Some(FailureKind::CliOutdated));
        assert_eq!(kind("Claude Code would not start in auto mode (it fell back to default mode)."), Some(FailureKind::Permissions));
        assert_eq!(kind("Claude Code returned error: Auto mode is unavailable for your plan"), Some(FailureKind::Permissions));
        assert_eq!(kind("Vault reference for GITHUB_TOKEN could not be read: 1Password CLI is not signed in."), Some(FailureKind::Vault));
        assert_eq!(kind("run failed: Vault reference for GITHUB_TOKEN could not be read: 1Password is locked."), Some(FailureKind::Vault));
        assert_ne!(kind("The agent wrote a vault reference for the docs page."), Some(FailureKind::Vault));
        assert_ne!(kind("Error: no vault reference found"), Some(FailureKind::Vault));
        for error in ["Claude Code would not start in auto mode", "Claude Code is not installed on this device", "error: unknown option '--x'"] {
            assert!(!is_rejected_key(error), "{error}");
            assert!(!describe(error).contains('\u{2014}'));
        }
    }

    #[test]
    fn rejected_keys_are_final() {
        for error in [
            "omp model call failed: 401 status code (no body)",
            "omp model call failed: 403 Forbidden",
            "Codex turn failed: unexpected status 401 Unauthorized: Incorrect API key provided",
            "unexpected status 403 Forbidden: {\"error\":{\"type\":\"permission_error\"}}",
            "API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}",
            "Error: 401 No auth credentials found",
        ] {
            assert!(is_rejected_key(error), "{error}");
            let failure = classify(error).unwrap();
            assert_eq!(failure.retry_at(Utc::now()), None, "{error}");
        }
        for error in [
            "exceeded retry limit, last status: 429 Too Many Requests",
            "API Error: 503 Service Unavailable",
            "stream disconnected before completion: error sending request",
            "cargo test failed: 4012 tests, 403920 assertions",
            "Credit balance is too low",
        ] {
            assert!(!is_rejected_key(error), "{error}");
        }
    }

    #[test]
    fn missing_scopes_are_not_a_missing_login_but_still_pause() {
        let error = "Codex turn failed: unexpected status 401 Unauthorized: You have insufficient permissions for this operation. Missing scopes: api.responses.write. Check that you have the correct role in your organization (Reader, Writer, Owner) and project (Member, Owner), and if you're using a restricted API key, that it has the necessary scopes., url: http://127.0.0.1:49644/u/a646383338bb27cc/responses";
        let failure = classify(error).unwrap();
        assert_eq!(failure.kind, FailureKind::MissingScope);
        assert_eq!(failure.kind.as_str(), "missing_scope");
        assert!(is_rejected_key(error));
        assert_eq!(failure.retry_at(Utc::now()), None);
        assert!(describe(error).starts_with("The CLI is signed in, but"));
        assert!(!failure.summary().contains('\u{2014}'));
        for (near_miss, expected) in [
            ("Codex turn failed: unexpected status 401 Unauthorized: Incorrect API key provided", Some(FailureKind::Auth)),
            ("unexpected status 403 Forbidden: {\"error\":{\"type\":\"permission_error\"}}", Some(FailureKind::Auth)),
            ("Error: Not logged in. Run codex login", Some(FailureKind::Auth)),
            ("insufficient_quota: You exceeded your current quota", Some(FailureKind::Billing)),
            ("test missing_scope_handling failed", None),
            ("warning: missing scope for variable x", None),
            ("insufficient permissions to write ./target", None),
            ("Codex turn failed: insufficient permissions for this operation", None),
            ("gh: GraphQL: missing scope: repo (createPullRequest)", None),
            ("gh: Missing scopes: read:org, url: https://api.github.com/graphql", None),
            ("GraphQL: Your token has not been granted the required scopes to execute this query. The 'login' field requires one of the following scopes: ['read:user']", None),
        ] {
            assert_eq!(classify(near_miss).map(|f| f.kind), expected, "{near_miss}");
            if expected.is_none() {
                assert!(!is_rejected_key(near_miss), "{near_miss}");
            }
        }
    }

    #[test]
    fn describe_is_idempotent_and_keeps_details() {
        let once = describe("API Error: 529 Overloaded");
        assert!(once.starts_with("The model provider is overloaded"));
        assert!(once.ends_with("API Error: 529 Overloaded"));
        assert_eq!(describe(&once), once);
        assert_eq!(describe("plain failure"), "plain failure");
        assert!(!once.contains('\u{2014}'));
        let long = format!("Invalid API key {}", "x".repeat(1000));
        assert!(describe(&long).ends_with(&long));
        assert_eq!(error_text(&"y".repeat(9000)).chars().count(), ERROR_TEXT_MAX + 3);
        assert_eq!(status_detail(&"z".repeat(9000)).chars().count(), STATUS_DETAIL_MAX + 3);
    }
}
