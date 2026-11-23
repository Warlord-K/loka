use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::Result;
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::WorldStore;

pub const MAX_DAYS: i32 = 90;
const FAILURE_SAMPLES: i64 = 400;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct UsageRow {
    pub agent_id: Uuid,
    pub agent: String,
    pub agent_cli: String,
    pub agent_model: String,
    pub cli: String,
    pub model: String,
    pub kind: String,
    pub runs: i64,
    pub completed: i64,
    pub failed: i64,
    pub timed_out: i64,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub avg_secs: Option<f64>,
    pub max_secs: Option<f64>,
    #[sqlx(default)]
    pub estimated: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct QuestCost {
    pub task_id: Uuid,
    pub title: String,
    pub status: String,
    pub agent: String,
    pub runs: i64,
    pub failed: i64,
    pub cost_usd: f64,
    pub time_limit_secs: Option<i32>,
}

#[derive(Debug, Clone, FromRow)]
struct FailureSample {
    agent: String,
    kind: String,
    status: String,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureGroup {
    pub agent: String,
    pub kind: String,
    pub cause: String,
    pub count: i64,
    pub example: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub days: i32,
    pub rows: Vec<UsageRow>,
    pub quests: Vec<QuestCost>,
    pub failures: Vec<FailureGroup>,
}

impl WorldStore {
    pub async fn usage_report(&self, owner: Uuid, agent: Option<Uuid>, days: i32) -> Result<UsageReport> {
        let days = days.clamp(1, MAX_DAYS);
        let rows = sqlx::query_as::<_, UsageRow>(
            "SELECT a.id AS agent_id, a.name AS agent, a.cli AS agent_cli, a.model AS agent_model,
                    s.backend AS cli, s.model,
                    CASE WHEN r.kind = 'aside' AND s.asked_by->>'kind' = 'question' THEN 'answer' ELSE r.kind END AS kind,
                    COUNT(*) AS runs,
                    COUNT(*) FILTER (WHERE r.status = 'completed') AS completed,
                    COUNT(*) FILTER (WHERE r.status = 'failed') AS failed,
                    COUNT(*) FILTER (WHERE r.status = 'timed_out') AS timed_out,
                    COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    COALESCE(SUM(r.input_tokens), 0)::int8 AS input_tokens,
                    COALESCE(SUM(r.cached_input_tokens), 0)::int8 AS cached_input_tokens,
                    COALESCE(SUM(r.output_tokens), 0)::int8 AS output_tokens,
                    AVG(EXTRACT(EPOCH FROM (r.finished_at - r.started_at)))::float8 AS avg_secs,
                    MAX(EXTRACT(EPOCH FROM (r.finished_at - r.started_at)))::float8 AS max_secs
             FROM session_runs r
             JOIN agent_sessions s ON s.id = r.session_id
             JOIN agent_profiles a ON a.id = r.agent_id
             WHERE a.owner_user_id = $1
               AND ($2::uuid IS NULL OR a.id = $2)
               AND r.created_at >= NOW() - make_interval(days => $3)
             GROUP BY a.id, a.name, a.cli, a.model, s.backend, s.model, 7
             ORDER BY cost_usd DESC, runs DESC",
        )
        .bind(owner)
        .bind(agent)
        .bind(days)
        .fetch_all(self.pool())
        .await?;
        let rows = rows.into_iter().map(estimate_unpriced).collect();
        let quests = sqlx::query_as::<_, QuestCost>(
            "SELECT t.id AS task_id, t.title, t.status, a.name AS agent,
                    COUNT(*) AS runs,
                    COUNT(*) FILTER (WHERE r.status IN ('failed', 'timed_out')) AS failed,
                    COALESCE(SUM(r.cost_usd), 0)::float8 AS cost_usd,
                    t.time_limit_secs
             FROM session_runs r
             JOIN agent_tasks t ON t.id = r.task_id
             JOIN agent_profiles a ON a.id = r.agent_id
             WHERE a.owner_user_id = $1
               AND ($2::uuid IS NULL OR a.id = $2)
               AND r.created_at >= NOW() - make_interval(days => $3)
             GROUP BY t.id, t.title, t.status, a.name, t.time_limit_secs
             ORDER BY cost_usd DESC
             LIMIT 10",
        )
        .bind(owner)
        .bind(agent)
        .bind(days)
        .fetch_all(self.pool())
        .await?;
        let samples = sqlx::query_as::<_, FailureSample>(
            "SELECT a.name AS agent, r.kind, r.status, r.error
             FROM session_runs r
             JOIN agent_profiles a ON a.id = r.agent_id
             WHERE a.owner_user_id = $1
               AND ($2::uuid IS NULL OR a.id = $2)
               AND r.created_at >= NOW() - make_interval(days => $3)
               AND r.status IN ('failed', 'timed_out')
             ORDER BY r.created_at DESC
             LIMIT $4",
        )
        .bind(owner)
        .bind(agent)
        .bind(days)
        .bind(FAILURE_SAMPLES)
        .fetch_all(self.pool())
        .await?;
        Ok(UsageReport { days, rows, quests, failures: group_failures(samples) })
    }
}

fn failure_cause(status: &str, error: &str) -> String {
    if status == "timed_out" || error.contains("timed out") {
        return "time_limit".to_string();
    }
    if let Some(failure) = crate::cli_errors::classify(error) {
        return failure.kind.as_str().to_string();
    }
    let lower = error.to_ascii_lowercase();
    if lower.contains("lease") || lower.contains("runner") || lower.contains("checkpoint") {
        return "runner".to_string();
    }
    if lower.contains("report") {
        return "missing_report".to_string();
    }
    if lower.contains("stopped") || lower.contains("cancel") {
        return "stopped".to_string();
    }
    "other".to_string()
}

fn group_failures(samples: Vec<FailureSample>) -> Vec<FailureGroup> {
    let mut groups: BTreeMap<(String, String, String), FailureGroup> = BTreeMap::new();
    for sample in samples {
        let error = sample.error.unwrap_or_default();
        let cause = failure_cause(&sample.status, &error);
        let example: String = crate::cli_errors::describe(&error)
            .split(crate::cli_errors::FRIENDLY_MARK)
            .next()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect();
        groups
            .entry((sample.agent.clone(), sample.kind.clone(), cause.clone()))
            .and_modify(|g| g.count += 1)
            .or_insert(FailureGroup { agent: sample.agent, kind: sample.kind, cause, count: 1, example });
    }
    let mut out: Vec<FailureGroup> = groups.into_values().collect();
    out.sort_by_key(|g| std::cmp::Reverse(g.count));
    out
}

pub fn tokens(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

fn minutes(secs: Option<f64>) -> String {
    match secs {
        Some(s) if s >= 60.0 => format!("{:.0}m", s / 60.0),
        Some(s) => format!("{s:.0}s"),
        None => "-".to_string(),
    }
}

fn estimate_unpriced(mut row: UsageRow) -> UsageRow {
    if row.cost_usd > 0.0 || !crate::backends::tokens_only(&row.cli) {
        return row;
    }
    let tokens = |n: i64| n.max(0) as u64;
    let estimate = crate::pricing::cost(&row.model, tokens(row.input_tokens), tokens(row.cached_input_tokens), tokens(row.output_tokens));
    if estimate > 0.0 {
        row.cost_usd = estimate;
        row.estimated = true;
    }
    row
}

fn cost_note(report: &UsageReport) -> String {
    let mut notes = vec!["Claude and omp report their own dollars, priced at API list prices: when Claude Code is signed in with a Claude plan, the plan is billed, not these dollars.".to_string()];
    let tokens_only: Vec<&UsageRow> = report.rows.iter().filter(|r| crate::backends::tokens_only(&r.cli)).collect();
    if !tokens_only.is_empty() {
        notes.push("Cursor and Codex report only tokens, so their dollars are estimates at public API list prices, not the bill of a Cursor or ChatGPT plan, which spends plan quota instead; rows marked (est.) were estimated now for runs recorded before prices were known.".to_string());
        if tokens_only.iter().any(|r| r.cli == "cursor") {
            notes.push("Cursor chat turns in its fast warm mode report no tokens at all, so they add $0.".to_string());
        }
        let mut unpriced: Vec<&str> = tokens_only
            .iter()
            .filter(|r| r.cost_usd == 0.0 && r.input_tokens + r.output_tokens > 0)
            .map(|r| r.model.as_str())
            .collect();
        unpriced.sort_unstable();
        unpriced.dedup();
        if !unpriced.is_empty() {
            notes.push(format!("No list price is known for {}, so those runs show $0; read their tokens instead.", unpriced.join(", ")));
        }
    }
    notes.join(" ")
}

pub fn render(report: &UsageReport) -> String {
    if report.rows.is_empty() {
        return format!("No runs in the last {} days.", report.days);
    }
    let mut out = String::new();
    let total: f64 = report.rows.iter().map(|r| r.cost_usd).sum();
    let runs: i64 = report.rows.iter().map(|r| r.runs).sum();
    let failed: i64 = report.rows.iter().map(|r| r.failed + r.timed_out).sum();
    let _ = writeln!(
        out,
        "Last {} days: ${total:.2} over {runs} runs, {failed} failed or timed out. {}",
        report.days,
        cost_note(report)
    );
    let mut by_agent: BTreeMap<(String, String, String), Vec<&UsageRow>> = BTreeMap::new();
    for row in &report.rows {
        by_agent
            .entry((row.agent.clone(), row.agent_cli.clone(), row.agent_model.clone()))
            .or_default()
            .push(row);
    }
    let mut agents: Vec<_> = by_agent.into_iter().collect();
    agents.sort_by(|a, b| {
        let cost = |rows: &Vec<&UsageRow>| rows.iter().map(|r| r.cost_usd).sum::<f64>();
        cost(&b.1).total_cmp(&cost(&a.1))
    });
    out.push_str("\nPer agent (kind, cli/model: runs ok/failed/timed out, cost, tokens in/cached/out, avg and max run time):\n");
    for ((agent, cli, model), rows) in agents {
        let cost: f64 = rows.iter().map(|r| r.cost_usd).sum();
        let share = if total > 0.0 { cost / total * 100.0 } else { 0.0 };
        let _ = writeln!(out, "- {agent} (now {cli}/{model}): ${cost:.2}, {share:.0}% of spend");
        for r in rows {
            let _ = writeln!(
                out,
                "    {} {}/{}: {} runs {}/{}/{}, ${:.2}{}, {}/{}/{}, avg {} max {}",
                r.kind,
                r.cli,
                r.model,
                r.runs,
                r.completed,
                r.failed,
                r.timed_out,
                r.cost_usd,
                if r.estimated { " (est.)" } else { "" },
                tokens(r.input_tokens),
                tokens(r.cached_input_tokens),
                tokens(r.output_tokens),
                minutes(r.avg_secs),
                minutes(r.max_secs)
            );
        }
    }
    if !report.quests.is_empty() {
        out.push_str("\nCostliest quests:\n");
        for q in &report.quests {
            let limit = q.time_limit_secs.map(|s| format!(", limit {}m", s / 60)).unwrap_or_default();
            let _ = writeln!(
                out,
                "- {} {} ({}, {}): ${:.2} over {} runs, {} failed{limit}",
                &q.task_id.to_string()[..8],
                q.title.chars().take(70).collect::<String>(),
                q.agent,
                q.status,
                q.cost_usd,
                q.runs,
                q.failed
            );
        }
    }
    if !report.failures.is_empty() {
        out.push_str("\nFailures by cause (time_limit = run hit its time limit; raise it with set_time_limit or time_limit_minutes):\n");
        for f in report.failures.iter().take(15) {
            let _ = writeln!(out, "- {} {} {} x{}: {}", f.agent, f.kind, f.cause, f.count, f.example);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_failures_by_cause() {
        let sample = |status: &str, error: &str| FailureSample {
            agent: "Ada".into(),
            kind: "task".into(),
            status: status.into(),
            error: Some(error.into()),
        };
        let groups = group_failures(vec![
            sample("timed_out", "timed out after 3600s"),
            sample("failed", "runner job timed out"),
            sample("failed", "runner lease expired before it could be renewed"),
        ]);
        assert_eq!(groups[0].cause, "time_limit");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[1].cause, "runner");
    }

    #[test]
    fn renders_empty_reports() {
        let report = UsageReport { days: 7, rows: Vec::new(), quests: Vec::new(), failures: Vec::new() };
        assert_eq!(render(&report), "No runs in the last 7 days.");
    }
}
