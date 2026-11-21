use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli_errors::{self, FailureKind, FRIENDLY_MARK};

pub const TAIL_MAX: usize = 1_000;

pub const RETRY_BACKOFF_SECS: i64 = 20;

pub const RESUME_AFTER_ERROR: &str = "[resumed after an interruption]\nYour last run was cut off by a problem outside your work, and the Loka resumed this same session for you. Do not start over and do not redo finished steps. Check where you stopped, carry on, and end your final message with the full report block exactly as the task prompt describes.";

pub const FRESH_RETRY_CAUSE: &str = "context_fresh";

pub const UPDATE_PAUSE: &str = "Paused while Loka restarted for an update";

pub const RESUME_AFTER_UPDATE: &str = "[resumed after a Loka update]\nLoka restarted for an update while you were working and resumed this same session for you. Do not start over and do not redo finished steps. Check where you stopped (the last command may not have finished), carry on, and end your final message with the full report block exactly as the task prompt describes.";

pub const RESUME_DIRECT_AFTER_UPDATE: &str = "Loka restarted for an update while you were working. Check where you stopped (the last command may not have finished) and carry on with what you were doing.";

pub fn update_pause(error: &str) -> bool {
    error.contains(UPDATE_PAUSE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    NeverStarted,
    CliError,
    NoReport,
    TimedOut,
    Stopped,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::NeverStarted => "never_started",
            Cause::CliError => "cli_error",
            Cause::NoReport => "no_report",
            Cause::TimedOut => "timed_out",
            Cause::Stopped => "stopped",
        }
    }
}

pub struct RunEnd<'a> {
    pub kind: &'a str,
    pub error: Option<&'a str>,
    pub output: &'a str,
    pub turns: i64,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub cause: Cause,
    #[serde(default)]
    pub failure: Option<String>,
    pub stage: String,
    pub reason: String,
    #[serde(default)]
    pub turns: i64,
    #[serde(default)]
    pub duration_secs: i64,
    #[serde(default)]
    pub tail: String,
    #[serde(default)]
    pub auto_retried: bool,
    #[serde(default)]
    pub resets_at: Option<DateTime<Utc>>,
}

const RUNNER_LOST: &[&str] = &[
    "runner lease",
    "lease expired",
    "lease was cancelled",
    "lease ended",
    "device was asleep",
    "stopped responding",
    "moved to another device",
    "uncheckpointed",
    "runner stopped before",
    "runner completion returned",
];

const WORKSPACE: &[&str] = &[
    "workspace",
    "worktree",
    "git clone",
    "clone failed",
    "could not clone",
    "repository not found",
    "git fetch",
    "fatal: ",
];

const UNRELATED_HISTORY: &str = "has commits but no history in common with";

pub fn unrelated_history_error(base: &str) -> String {
    format!("The quest branch {UNRELATED_HISTORY} {base}; nothing was published, rebase it onto the new base.")
}

pub fn diagnose(end: &RunEnd) -> Diagnosis {
    let started = end.turns > 0;
    let at_run = if started { "run" } else { "start" };
    let error = end.error.map(str::trim).filter(|error| !error.is_empty());
    let (cause, failure, stage, reason, resets_at) = match error {
        Some(error) => {
            let raw = error.split(FRIENDLY_MARK).last().unwrap_or(error);
            let lower = raw.to_ascii_lowercase();
            let first = first_line(raw, 200);
            if update_pause(raw) {
                let cause = if started { Cause::CliError } else { Cause::NeverStarted };
                (cause, Some("update".to_string()), at_run, "Loka restarted for an update, so it picks up where it left off.".to_string(), None)
            } else if lower.contains(UNRELATED_HISTORY) {
                (Cause::CliError, None, "publish", first, None)
            } else if lower.starts_with("stopped by") {
                (Cause::Stopped, None, at_run, format!("{} stopped it.", raw.trim_start_matches("Stopped by ").split('.').next().unwrap_or("Someone")), None)
            } else if end.timed_out || lower.contains("timed out after") || lower.contains("time limit") || lower.contains("runner job timed out") {
                (Cause::TimedOut, None, at_run, "It ran out of time before it finished.".to_string(), None)
            } else if RUNNER_LOST.iter().any(|marker| lower.contains(marker)) {
                let cause = if started { Cause::CliError } else { Cause::NeverStarted };
                let reason = if lower.contains("asleep") {
                    "The device running it went to sleep or offline, so the run was lost."
                } else if lower.contains("moved to another device") {
                    "The agent was moved to another device while it ran."
                } else {
                    "The device running it went offline or lost contact with the Loka."
                };
                (cause, Some("runner".to_string()), at_run, reason.to_string(), None)
            } else if let Some(failure) = cli_errors::classify(raw) {
                let resets_at = failure.resets_at;
                (Cause::CliError, Some(failure.kind.as_str().to_string()), at_run, failure.summary(), resets_at)
            } else if !started && WORKSPACE.iter().any(|marker| lower.contains(marker)) {
                (Cause::NeverStarted, None, "workspace", format!("Its workspace could not be prepared: {first}"), None)
            } else if !started {
                (Cause::NeverStarted, None, "start", format!("The CLI stopped before its first step: {first}"), None)
            } else {
                (Cause::CliError, None, "run", format!("The CLI stopped with an error after {} steps: {first}", end.turns), None)
            }
        }
        None if end.output.trim().is_empty() && !started => (
            Cause::NeverStarted,
            None,
            "start",
            "The CLI exited at once without doing anything.".to_string(),
            None,
        ),
        None => (
            Cause::NoReport,
            None,
            "report",
            format!("It worked for {} steps but ended without the report block.", end.turns),
            None,
        ),
    };
    let stage = match end.kind {
        "review" => "review",
        "publish" => "publish",
        _ => stage,
    };
    Diagnosis {
        cause,
        failure,
        stage: stage.to_string(),
        reason,
        turns: end.turns,
        duration_secs: 0,
        tail: String::new(),
        auto_retried: false,
        resets_at,
    }
}

impl Diagnosis {
    pub fn transient(&self) -> bool {
        match self.cause {
            Cause::NeverStarted => true,
            Cause::CliError => matches!(
                self.failure.as_deref(),
                Some("network" | "overloaded" | "rate_limited" | "usage_limit" | "runner" | "update")
            ),
            _ => false,
        }
    }

    pub fn fork_too_long(&self) -> bool {
        self.cause == Cause::CliError && self.failure.as_deref() == Some("context_too_long") && self.turns <= 1
    }

    pub fn for_update(&self) -> bool {
        self.failure.as_deref() == Some("update")
    }

    pub fn retry_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        if self.for_update() {
            return now;
        }
        let limit = match self.failure.as_deref() {
            Some("usage_limit") => Some(FailureKind::UsageLimit),
            Some("rate_limited") => Some(FailureKind::RateLimited),
            _ => None,
        };
        limit
            .and_then(|kind| cli_errors::CliFailure { kind, resets_at: self.resets_at }.retry_at(now))
            .unwrap_or(now + Duration::seconds(RETRY_BACKOFF_SECS))
    }

    pub fn payload(&self) -> Value {
        json!({
            "cause": self.cause.as_str(),
            "reason": self.reason,
            "stage": self.stage,
            "turns": self.turns,
            "duration_secs": self.duration_secs,
            "tail": self.tail,
            "auto_retried": self.auto_retried,
        })
    }

    pub fn merge_into(&self, payload: &mut Value) {
        if let (Some(target), Value::Object(fields)) = (payload.as_object_mut(), self.payload()) {
            target.extend(fields);
        }
    }
}

pub fn tail_text(parts: &[&str]) -> String {
    let joined = parts.iter().map(|part| part.trim()).filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n");
    let length = joined.chars().count();
    if length <= TAIL_MAX {
        return joined;
    }
    let cut: String = joined.chars().skip(length - TAIL_MAX).collect();
    match cut.split_once('\n') {
        Some((_, rest)) if rest.chars().count() > TAIL_MAX / 2 => rest.to_string(),
        _ => cut,
    }
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_string();
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end<'a>(error: Option<&'a str>, output: &'a str, turns: i64) -> RunEnd<'a> {
        RunEnd { kind: "task", error, output, turns, timed_out: false }
    }

    #[test]
    fn empty_runs_never_started() {
        let d = diagnose(&end(None, "  ", 0));
        assert_eq!(d.cause, Cause::NeverStarted);
        assert!(d.transient());
        let d = diagnose(&end(Some("Claude Code exited with code 1: spawn ENOENT"), "", 0));
        assert_eq!((d.cause, d.stage.as_str()), (Cause::NeverStarted, "start"));
        let d = diagnose(&end(Some("failed to create worktree: fatal: invalid reference"), "", 0));
        assert_eq!((d.cause, d.stage.as_str()), (Cause::NeverStarted, "workspace"));
        let d = diagnose(&end(Some(crate::runners::RUNNER_ASLEEP_ERROR), "", 0));
        assert_eq!((d.cause, d.failure.as_deref()), (Cause::NeverStarted, Some("runner")));
    }

    #[test]
    fn update_pauses_resume_at_once() {
        let d = diagnose(&end(Some(UPDATE_PAUSE), "", 4));
        assert_eq!((d.cause, d.failure.as_deref()), (Cause::CliError, Some("update")));
        assert!(d.transient() && d.for_update());
        let now = Utc::now();
        assert_eq!(d.retry_at(now), now);
        let d = diagnose(&end(Some(UPDATE_PAUSE), "", 0));
        assert_eq!(d.cause, Cause::NeverStarted);
    }

    #[test]
    fn only_early_context_failures_retry_fresh() {
        let d = diagnose(&end(Some("Prompt is too long"), "", 0));
        assert_eq!((d.cause, d.failure.as_deref()), (Cause::CliError, Some("context_too_long")));
        assert!(d.fork_too_long());
        assert!(diagnose(&end(Some("Prompt is too long"), "", 1)).fork_too_long());
        assert!(!diagnose(&end(Some("Prompt is too long"), "", 6)).fork_too_long());
        assert!(!diagnose(&end(Some("API Error: 529 Overloaded"), "", 0)).fork_too_long());
    }

    #[test]
    fn work_without_report_is_no_report() {
        let d = diagnose(&end(None, "I changed the files.", 12));
        assert_eq!((d.cause, d.stage.as_str()), (Cause::NoReport, "report"));
        assert!(!d.transient());
        assert_eq!(diagnose(&end(None, "Cannot do this.", 0)).cause, Cause::NoReport);
    }

    #[test]
    fn errors_after_work() {
        let d = diagnose(&end(Some("API Error: 529 Overloaded"), "", 4));
        assert_eq!((d.cause, d.failure.as_deref()), (Cause::CliError, Some("overloaded")));
        assert!(d.transient());
        let auth = diagnose(&end(Some("Invalid API key · Please run /login"), "", 0));
        assert_eq!(auth.cause, Cause::CliError);
        assert!(!auth.transient());
        let lost = diagnose(&end(Some("runner lease expired before it could be renewed"), "", 9));
        assert_eq!((lost.cause, lost.failure.as_deref()), (Cause::CliError, Some("runner")));
        assert!(lost.transient());
        let refused = diagnose(&end(Some("Claude Code cannot start in auto mode for model haiku. Loka never switches to full access on its own."), "", 0));
        assert_eq!((refused.cause, refused.failure.as_deref()), (Cause::CliError, Some("permissions")));
        assert!(!refused.transient(), "a refused permission mode must not be retried with more access");
        let other = diagnose(&end(Some("Claude Code exited with code 1"), "", 3));
        assert_eq!(other.cause, Cause::CliError);
        assert!(!other.transient());
    }

    #[test]
    fn stops_and_timeouts_are_never_retried() {
        let stopped = diagnose(&end(Some("Stopped by Yatharth. Do not restart this work unless they ask."), "", 5));
        assert_eq!(stopped.cause, Cause::Stopped);
        assert_eq!(stopped.reason, "Yatharth stopped it.");
        assert!(!stopped.transient());
        let timed = diagnose(&end(Some("timed out after 3600s"), "", 40));
        assert_eq!(timed.cause, Cause::TimedOut);
        assert!(!timed.transient());
    }

    #[test]
    fn limits_retry_at_reset_and_others_back_off() {
        let now = Utc::now();
        let limit = diagnose(&end(Some("Claude AI usage limit reached|4102444800"), "", 0));
        assert_eq!(limit.failure.as_deref(), Some("usage_limit"));
        assert_eq!(limit.retry_at(now), now + Duration::hours(6));
        let net = diagnose(&end(Some("stream disconnected before completion"), "", 2));
        assert_eq!(net.retry_at(now), now + Duration::seconds(RETRY_BACKOFF_SECS));
    }

    #[test]
    fn tail_is_capped_and_has_no_em_dashes() {
        let long = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let t = tail_text(&[&long, "boom"]);
        assert!(t.chars().count() <= TAIL_MAX);
        assert!(t.ends_with("boom"));
        for d in [diagnose(&end(None, "", 0)), diagnose(&end(None, "x", 3)), diagnose(&end(Some("x"), "", 1))] {
            assert!(!d.reason.contains('\u{2014}'));
        }
    }
}
