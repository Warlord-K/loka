use super::tests::test_ctx;
use super::{fail_with, session_id_after_run, settle_completed, spawn_activity, Ctx};
use crate::claude::TurnReader;
use crate::cli::{CliResult, Redactor, StreamEvent};
use crate::remote_runner::{errored_completion, map_event, success_completion, ExecutionOutcome};
use loka_agent::runner_server::RunnerCoordinator;
use loka_shared::cli_errors::{classify, FailureKind};
use loka_shared::run_recording::EVENT_MAX_CHARS;
use loka_shared::runners::{BeginCompletion, CreateRunner, RunnerCompletion, RunnerStore};
use loka_shared::world::{AgentSession, WorldAgent};
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

const SECRET: &str = "sk-live-4f9a2c71d8e3b6a0";
const REPORT: &str = r#"<!-- report {"status":"done","summary":"All three tests pass"} report -->"#;

fn stream() -> Vec<Value> {
    vec![
        json!({"type": "system", "subtype": "init", "session_id": "cs-1", "permissionMode": "auto"}),
        json!({"type": "assistant", "uuid": "u1", "message": {"id": "m1",
            "usage": {"input_tokens": 100, "cache_creation_input_tokens": 20, "cache_read_input_tokens": 300, "output_tokens": 30},
            "content": [{"type": "text", "text": "Running the tests."},
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}}]}}),
        json!({"type": "user", "uuid": "u2", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "test result: ok. 3 passed"}]}}),
        json!({"type": "assistant", "uuid": "u3", "message": {"id": "m2",
            "usage": {"input_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 500, "output_tokens": 40},
            "content": [{"type": "text", "text": "All good."}]}}),
        json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "cs-1", "num_turns": 2,
            "total_cost_usd": 0.42, "result": format!("All three tests pass.\n{REPORT}"),
            "usage": {"input_tokens": 110, "cache_creation_input_tokens": 20, "cache_read_input_tokens": 800, "output_tokens": 70}}),
    ]
}

fn read(lines: &[Value]) -> (Vec<StreamEvent>, CliResult) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut reader = TurnReader::new(Instant::now());
    for line in lines {
        reader.feed(line, Some(&tx));
    }
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (events, reader.into_result(0))
}

fn scripted() -> (Vec<StreamEvent>, CliResult) {
    read(&stream())
}

fn without_session_start(events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    events.into_iter().filter(|event| !matches!(event, StreamEvent::SessionStarted { .. })).collect()
}

fn answered(exit_code: i32) -> (Vec<StreamEvent>, CliResult) {
    let (events, mut result) = scripted();
    result.exit_code = exit_code;
    (events, result)
}

#[test]
fn the_scripted_stream_reads_the_same_for_cold_and_warm_turns() {
    let (events, result) = scripted();
    let shapes: Vec<String> = events
        .iter()
        .map(|event| match event {
            StreamEvent::SessionStarted { session_id } => format!("session {session_id}"),
            StreamEvent::Usage(usage) => format!("usage {}/{}/{}", usage.input_tokens, usage.cached_input_tokens, usage.output_tokens),
            StreamEvent::ToolUse { tool, input_summary } => format!("tool {tool} {input_summary}"),
            StreamEvent::ToolResult { tool, output } => format!("result {tool} {output}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        shapes,
        [
            "session cs-1",
            "usage 120/300/30",
            "tool Bash cargo test",
            "result Bash test result: ok. 3 passed",
            "usage 130/800/70",
        ],
        "events come in the order the CLI printed them, with live usage summed over calls"
    );
    assert_eq!(result.output, format!("All three tests pass.\n{REPORT}"));
    assert_eq!(result.session_id.as_deref(), Some("cs-1"));
    assert_eq!((result.usage.input_tokens, result.usage.cached_input_tokens, result.usage.output_tokens), (130, 800, 70));
    assert_eq!(result.usage.context_tokens, 510, "the context is what the last model call read");
    assert!((result.cost_usd - 0.42).abs() < 1e-9);
    assert_eq!(result.checkpoint.as_deref(), Some("u3"));
    let report = super::prompts::parse_report(&result.output).expect("the report block parses");
    assert_eq!(report.summary.as_deref(), Some("All three tests pass"));
}

fn pieces(secret: &str) -> Vec<&str> {
    (0..=secret.len() - loka_shared::redaction::MIN_SECRET_BYTES).map(|start| &secret[start..start + loka_shared::redaction::MIN_SECRET_BYTES]).collect()
}

fn straddling_events(pad: usize) -> Vec<StreamEvent> {
    let around = |filler: char| format!("{}{SECRET}{}", filler.to_string().repeat(pad), "z".repeat(40));
    vec![
        StreamEvent::ToolResult { tool: "Bash".into(), output: around('a') },
        StreamEvent::ToolUse { tool: "Bash".into(), input_summary: around('b') },
        StreamEvent::AssistantText(around('c')),
        StreamEvent::Error(around('d')),
    ]
}

fn cut_points() -> impl Iterator<Item = usize> {
    [crate::cli::TOOL_INPUT_MAX_CHARS, EVENT_MAX_CHARS]
        .into_iter()
        .flat_map(|limit| limit - SECRET.len() - 1..=limit + 1)
}

#[test]
fn a_secret_straddling_any_cut_point_never_leaks_from_the_runner_event_step() {
    let secrets = Redactor::new([SECRET]);
    for pad in cut_points() {
        for event in straddling_events(pad) {
            let shown = map_event(event, &secrets).expect("an event").text;
            for piece in pieces(SECRET) {
                assert!(!shown.contains(piece), "a piece of the secret got through at pad {pad}: ...{}", &shown[shown.len().saturating_sub(24)..]);
            }
        }
    }
}

#[test]
fn a_secret_cut_inside_the_parser_is_hidden_because_the_parser_no_longer_cuts() {
    let secrets = Redactor::new([SECRET]);
    for pad in [2_990, 2_994, 2_999, 3_000] {
        let lines = [
            json!({"type": "assistant", "uuid": "u1", "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": format!("{}{SECRET}", "b".repeat(pad.min(118)))}}]}}),
            json!({"type": "user", "uuid": "u2", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": format!("{}{SECRET}", "a".repeat(pad))}]}}),
        ];
        let (events, _) = read(&lines);
        assert!(events.len() >= 2);
        for event in events {
            if let Some(shown) = map_event(event, &secrets) {
                for piece in pieces(SECRET) {
                    assert!(!shown.text.contains(piece), "a piece of the secret got through the parser at pad {pad}");
                }
            }
        }
    }
}

#[test]
fn one_minimum_length_decides_what_is_hidden() {
    let short = "ab1";
    let secrets = Redactor::new([short, SECRET]);
    let event = StreamEvent::ToolResult { tool: "Bash".into(), output: format!("ab1 build 21 {SECRET}") };
    assert_eq!(map_event(event, &secrets).expect("an event").text, "ab1 build 21 [REDACTED]");
    assert_eq!(secrets.values(), [SECRET.to_string()]);
}

struct Rig {
    ctx: Ctx,
    coordinator: RunnerCoordinator,
    runners: RunnerStore,
}

async fn rig() -> anyhow::Result<Option<Rig>> {
    let Some(ctx) = test_ctx().await? else {
        anyhow::ensure!(
            std::env::var_os("CONTRACT_REQUIRE_DB").is_none(),
            "CONTRACT_REQUIRE_DB is set but STEER_TEST_DATABASE_URL is not, so the worker contract cases cannot run"
        );
        eprintln!("SKIPPED: set STEER_TEST_DATABASE_URL to a migrated database to run the worker contract cases");
        return Ok(None);
    };
    let coordinator = RunnerCoordinator::new(ctx.db.clone(), ctx.store.clone());
    let runners = RunnerStore::new(ctx.db.pool().clone());
    Ok(Some(Rig { ctx, coordinator, runners }))
}

struct Run {
    agent: WorldAgent,
    session: Uuid,
    run: Uuid,
    workspace: String,
    runner: Uuid,
    lease: Option<Uuid>,
}

impl Rig {
    async fn run(&self, backend: &str, model: &str, remote: bool) -> anyhow::Result<Run> {
        self.run_of("direct", backend, model, remote).await
    }

    async fn run_of(&self, kind: &str, backend: &str, model: &str, remote: bool) -> anyhow::Result<Run> {
        let pool = self.ctx.db.pool();
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
            .bind(format!("contract-{tag}@example.test"))
            .fetch_one(pool)
            .await?;
        let runner = self.runners.register(owner, &CreateRunner { name: "Contract Mac".into(), platform: "macos".into() }).await?.runner.id;
        let agent_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, runner_id) VALUES ($1, 'Bun', $2, $3, $4, $5) RETURNING id",
        )
        .bind(format!("contract-{tag}"))
        .bind(backend)
        .bind(model)
        .bind(owner)
        .bind(remote.then_some(runner))
        .fetch_one(pool)
        .await?;
        let workspace = format!("agents/contract-{tag}");
        let session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, working_dir, runner_id)
             VALUES ($1, 'main', $2, $3, 'active', $4, $5) RETURNING id",
        )
        .bind(agent_id)
        .bind(backend)
        .bind(model)
        .bind(&workspace)
        .bind(remote.then_some(runner))
        .fetch_one(pool)
        .await?;
        if kind == "followup" {
            sqlx::query("UPDATE agent_sessions SET cli_session_id = 'cs-0' WHERE id = $1").bind(session).execute(pool).await?;
        }
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at)
             VALUES ($1, $2, $4, $3, 'go', CASE WHEN $3 = 'running' THEN NOW() END) RETURNING id",
        )
        .bind(session)
        .bind(agent_id)
        .bind(if remote { "queued" } else { "running" })
        .bind(kind)
        .fetch_one(pool)
        .await?;
        let lease = if remote {
            let claim = self
                .runners
                .claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &[kind.to_string()])
                .await?
                .expect("the run is claimed");
            assert_eq!(claim.run_id, run);
            Some(claim.lease_id)
        } else {
            None
        };
        let agent = self.ctx.store.get_agent(agent_id).await?.expect("agent");
        Ok(Run { agent, session, run, workspace, runner, lease })
    }

    async fn session(&self, run: &Run) -> anyhow::Result<AgentSession> {
        Ok(self.ctx.store.get_session(run.session).await?.expect("session"))
    }

    async fn locally(&self, run: &Run, events: Vec<StreamEvent>, result: CliResult, redactor: &Redactor) -> anyhow::Result<()> {
        let record = self.ctx.store.get_run(run.run).await?.expect("run");
        let (tx, activity) = spawn_activity(self.ctx.store.clone(), record.clone(), redactor.clone());
        for event in events {
            tx.send(event)?;
        }
        drop(tx);
        activity.await?;
        let session = self.session(run).await?;
        let saved_session_id = session_id_after_run(&session.backend, false, result.session_id.as_deref(), None).map(str::to_string);
        settle_completed(&self.ctx, &run.agent, session, &record, result, saved_session_id, redactor).await
    }

    async fn show(&self, run: &Run, events: Vec<StreamEvent>, secrets: &Redactor) -> anyhow::Result<()> {
        let lease = self
            .runners
            .lease_for_runner(run.runner, run.lease.expect("a lease"))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Stop the local job"))?;
        let shown: Vec<_> = events.into_iter().filter_map(|event| map_event(event, secrets)).collect();
        self.coordinator.ingest_events(&lease, shown).await
    }

    async fn remotely(&self, run: &Run, events: Vec<StreamEvent>, result: CliResult, secrets: &Redactor) -> anyhow::Result<()> {
        self.show(run, events, secrets).await?;
        let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
        let session_id = outcome.result.session_id.clone();
        self.deliver(run, success_completion(outcome, session_id, run.workspace.clone(), Vec::new())).await
    }

    async fn deliver(&self, run: &Run, completion: RunnerCompletion) -> anyhow::Result<()> {
        let lease_id = run.lease.expect("a lease");
        let Some(submission) = self.runners.lease_for_submission(run.runner, lease_id).await? else {
            self.coordinator.keep_late_spend(run.runner, lease_id, &completion).await?;
            anyhow::bail!("Completion is stale")
        };
        let completion = self.coordinator.redact_completion(&submission, &completion).await?;
        let hash = vec![7u8; 32];
        match self.runners.begin_completion(run.runner, lease_id, &hash, &completion).await? {
            BeginCompletion::Begun => {}
            BeginCompletion::AlreadyCompleted => return Ok(()),
            other => anyhow::bail!("completion refused: {other:?}"),
        }
        let lease = self
            .runners
            .lease_for_completion(run.runner, lease_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Completion is stale"))?;
        self.coordinator.complete(&lease, &completion).await?;
        anyhow::ensure!(self.runners.finish_completion(run.runner, lease_id).await?, "Completion was cancelled");
        Ok(())
    }

    async fn snapshot(&self, run: &Run) -> anyhow::Result<Snapshot> {
        let pool = self.ctx.db.pool();
        let (status, output, error, cost, input, cached, generated): (String, Option<String>, Option<String>, f64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT status, output, error, cost_usd::float8, input_tokens::bigint, cached_input_tokens::bigint, output_tokens::bigint
                 FROM session_runs WHERE id = $1",
            )
            .bind(run.run)
            .fetch_one(pool)
            .await?;
        let (cli_session, baseline, context): (Option<String>, Value, i64) =
            sqlx::query_as("SELECT cli_session_id, usage_baseline, context_tokens::bigint FROM agent_sessions WHERE id = $1")
                .bind(run.session)
                .fetch_one(pool)
                .await?;
        let events: Vec<(String, Option<String>, String)> =
            sqlx::query_as("SELECT kind, tool, body FROM session_run_events WHERE run_id = $1 ORDER BY id")
                .bind(run.run)
                .fetch_all(pool)
                .await?;
        let announcements: Vec<String> = sqlx::query_scalar("SELECT type FROM world_events WHERE run_id = $1 AND type LIKE 'run.%' ORDER BY seq")
            .bind(run.run)
            .fetch_all(pool)
            .await?;
        Ok(Snapshot { status, output, error, cost, tokens: (input, cached, generated), cli_session, context, baseline, events, announcements })
    }
}

struct Snapshot {
    status: String,
    output: Option<String>,
    error: Option<String>,
    cost: f64,
    tokens: (i64, i64, i64),
    cli_session: Option<String>,
    context: i64,
    baseline: Value,
    events: Vec<(String, Option<String>, String)>,
    announcements: Vec<String>,
}

impl Snapshot {
    fn without_session_banner(mut self) -> Self {
        self.events.retain(|(kind, _, body)| !(kind == "text" && body.starts_with("Session started: ")));
        self
    }

    fn checkpoints(&self) -> Vec<(&'static str, String)> {
        vec![
            ("events", format!("{:?}", self.events)),
            ("run status", self.status.clone()),
            ("run output", format!("{:?}", self.output)),
            ("run error", format!("{:?}", self.error)),
            ("run tokens", format!("{:?}", self.tokens)),
            ("run cost", format!("{:.4}", self.cost)),
            ("session cli id", format!("{:?}", self.cli_session)),
            ("session context tokens", self.context.to_string()),
            ("session usage baseline", self.baseline.to_string()),
            ("announcements", format!("{:?}", self.announcements)),
        ]
    }
}

fn same(local: &Snapshot, remote: &Snapshot) {
    for ((step, on_local), (_, on_remote)) in local.checkpoints().into_iter().zip(remote.checkpoints()) {
        assert_eq!(on_local, on_remote, "the workers first differ at: {step}");
    }
}

fn no_secrets() -> (Redactor, Redactor) {
    (Redactor::default(), Redactor::default())
}

#[tokio::test]
async fn provider_errors_keep_their_class_on_both_workers_whatever_wraps_them() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let causes = [
        ("429 rate_limit_error: you are being rate limited", FailureKind::RateLimited, true),
        ("Claude usage limit reached. Your limit will reset at 5pm", FailureKind::UsageLimit, true),
        ("API Error: 529 overloaded_error", FailureKind::Overloaded, false),
        ("Your credit balance is too low to access the Anthropic API", FailureKind::Billing, false),
        ("Invalid API key - Please run /login", FailureKind::Auth, false),
        ("Prompt is too long", FailureKind::ContextTooLong, false),
        ("No space left on device (os error 28)", FailureKind::DiskFull, false),
    ];
    let redactor = Redactor::default();
    for (cause, kind, waits) in causes {
        let wrapped = || anyhow::anyhow!("{cause}").context("Failed to start Claude Code").context("warm session");

        let local = rig.run("claude", "sonnet", false).await?;
        let record = rig.ctx.store.get_run(local.run).await?.expect("run");
        let session = rig.session(&local).await?;
        fail_with(&rig.ctx, &record, &session, &wrapped(), &redactor).await?;

        let remote = rig.run("claude", "sonnet", true).await?;
        let completion = errored_completion(&wrapped(), None, remote.workspace.clone(), false);
        rig.deliver(&remote, completion).await?;

        for (path, run) in [("local", &local), ("remote", &remote)] {
            let stored = rig.snapshot(run).await?;
            assert_eq!(stored.status, "failed", "{path}: {cause}");
            let error = stored.error.unwrap_or_default();
            let failure = classify(&error).unwrap_or_else(|| panic!("{path} stored an error that lost its class: {cause} -> {error}"));
            assert_eq!(failure.kind, kind, "{path}: {cause}");
            assert_eq!(failure.retry_at(chrono::Utc::now()).is_some(), waits, "{path} retry class: {cause}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn one_scripted_run_lands_the_same_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let local = rig.run("claude", "sonnet", false).await?;
    let (events, result) = scripted();
    rig.locally(&local, events, result, &redactor).await?;
    let remote = rig.run("claude", "sonnet", true).await?;
    let (events, result) = scripted();
    rig.remotely(&remote, events, result, &secrets).await?;

    let (local, remote) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?.without_session_banner());
    for snapshot in [&local, &remote] {
        assert_eq!(snapshot.status, "completed");
        assert_eq!(snapshot.output.as_deref(), Some(format!("All three tests pass.\n{REPORT}").as_str()));
        assert_eq!(snapshot.tokens, (130, 800, 70));
        assert!((snapshot.cost - 0.42).abs() < 1e-6, "{}", snapshot.cost);
        assert_eq!(snapshot.cli_session.as_deref(), Some("cs-1"));
        assert_eq!(snapshot.context, 510);
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!((snapshot.events[0].0.as_str(), snapshot.events[0].1.as_deref(), snapshot.events[0].2.as_str()), ("tool", Some("Bash"), "cargo test"));
        assert_eq!((snapshot.events[1].0.as_str(), snapshot.events[1].2.as_str()), ("result", "test result: ok. 3 passed"));
        assert_eq!(snapshot.announcements, ["run.activity", "run.completed"], "the agent shows what it is doing, then the run completes");
    }
    same(&local, &remote);
    Ok(())
}

#[tokio::test]
async fn codex_cumulative_usage_is_charged_per_run_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let local = rig.run("codex", "gpt-5", false).await?;
    let remote = rig.run("codex", "gpt-5", true).await?;
    let totals = [(1_000, 400, 100), (1_500, 700, 180), (200, 50, 20)];
    let charged = [(1_000, 400, 100), (500, 300, 80), (200, 50, 20)];
    for (round, (total, expected)) in totals.into_iter().zip(charged).enumerate() {
        let result = || CliResult {
            output: format!("round {round}"),
            session_id: Some("thread-1".into()),
            cost_usd: 0.0,
            exit_code: 0,
            usage: crate::cli::Usage { input_tokens: total.0, cached_input_tokens: total.1, output_tokens: total.2, context_tokens: 0 },
            checkpoint: None,
        };
        for (worker, side) in [(&local, true), (&remote, false)] {
            let record_run = if round == 0 {
                worker.run
            } else {
                let next: Uuid = sqlx::query_scalar(
                    "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at) VALUES ($1, $2, 'direct', $3, 'go', NOW()) RETURNING id",
                )
                .bind(worker.session)
                .bind(worker.agent.id)
                .bind(if side { "running" } else { "queued" })
                .fetch_one(rig.ctx.db.pool())
                .await?;
                next
            };
            let step = Run {
                agent: worker.agent.clone(),
                session: worker.session,
                run: record_run,
                workspace: worker.workspace.clone(),
                runner: worker.runner,
                lease: if side || round == 0 { worker.lease } else { None },
            };
            let step = if !side && round > 0 {
                let claim = rig.runners.claim_kinds(step.runner, &format!("mcp-{}", Uuid::new_v4()), &["direct".to_string()]).await?.expect("claimed");
                Run { lease: Some(claim.lease_id), run: claim.run_id, ..step }
            } else {
                step
            };
            if side {
                rig.locally(&step, Vec::new(), result(), &redactor).await?;
            } else {
                rig.remotely(&step, Vec::new(), result(), &secrets).await?;
            }
            let (input, cached, generated): (i64, i64, i64) = sqlx::query_as(
                "SELECT input_tokens::bigint, cached_input_tokens::bigint, output_tokens::bigint FROM session_runs WHERE id = $1",
            )
            .bind(step.run)
            .fetch_one(rig.ctx.db.pool())
            .await?;
            let path = if side { "local" } else { "remote" };
            assert_eq!((input, cached, generated), (expected.0, expected.1, expected.2), "{path} round {round}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_cli_that_exits_badly_after_answering_still_completes_the_run_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    for exit_code in [1, 143] {
        let local = rig.run("claude", "sonnet", false).await?;
        let (events, result) = answered(exit_code);
        rig.locally(&local, events, result, &redactor).await?;
        let remote = rig.run("claude", "sonnet", true).await?;
        let (events, result) = answered(exit_code);
        rig.remotely(&remote, events, result, &secrets).await?;
        let (local, remote) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?.without_session_banner());
        assert_eq!(local.status, "completed", "local, exit {exit_code}");
        assert_eq!(remote.status, "completed", "remote, exit {exit_code}: {:?}", remote.error);
        same(&local, &remote);
    }
    Ok(())
}

#[tokio::test]
async fn a_cli_that_exits_badly_with_nothing_to_say_fails_the_run() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let remote = rig.run("claude", "sonnet", true).await?;
    let (_, mut result) = answered(2);
    result.output = String::new();
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
    rig.deliver(&remote, success_completion(outcome, None, remote.workspace.clone(), Vec::new())).await?;
    let snapshot = rig.snapshot(&remote).await?;
    assert_eq!(snapshot.status, "failed");
    assert!(snapshot.error.as_deref().is_some_and(|error| error.contains("exited with 2")), "{:?}", snapshot.error);
    Ok(())
}

#[tokio::test]
async fn a_secret_at_any_cut_point_is_hidden_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let secrets = Redactor::new([SECRET, "ab1"]);
    let redactor = secrets.clone();
    let boundary = || {
        let (mut events, result) = scripted();
        let mut at = 2;
        for pad in [crate::cli::TOOL_INPUT_MAX_CHARS - 6, EVENT_MAX_CHARS - 10] {
            for event in straddling_events(pad) {
                events.insert(at, event);
                at += 1;
            }
        }
        events.insert(at, StreamEvent::AssistantText("ab1 is short and stays readable".into()));
        (events, result)
    };
    let local = rig.run("claude", "sonnet", false).await?;
    let (events, result) = boundary();
    rig.locally(&local, events, result, &redactor).await?;
    let remote = rig.run("claude", "sonnet", true).await?;
    let (events, result) = boundary();
    rig.remotely(&remote, events, result, &secrets).await?;
    for (path, snapshot) in [("local", rig.snapshot(&local).await?), ("remote", rig.snapshot(&remote).await?)] {
        assert!(snapshot.events.len() > 10, "{path} stored too few events");
        for (_, _, body) in &snapshot.events {
            for piece in pieces(SECRET) {
                assert!(!body.contains(piece), "{path} showed part of a secret");
            }
        }
        assert!(
            snapshot.events.iter().any(|(_, _, body)| body == "ab1 is short and stays readable"),
            "{path} garbled a value shorter than the minimum"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_completion_after_the_run_was_closed_keeps_its_spend_and_never_touches_the_session_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let redactor = Redactor::default();
    let stopped = "Stopped by Tara. Do not restart this work unless they ask.";
    for (backend, model) in [("claude", "sonnet"), ("codex", "gpt-5"), ("omp", "sonnet"), ("cursor", "composer-2.5"), ("opencode", "sonnet")] {
        let local = rig.run(backend, model, false).await?;
        sqlx::query("UPDATE session_runs SET status = 'failed', error = $2, finished_at = NOW() WHERE id = $1")
            .bind(local.run)
            .bind(stopped)
            .execute(rig.ctx.db.pool())
            .await?;
        let (events, result) = scripted();
        rig.locally(&local, without_session_start(events), result, &redactor).await?;

        let remote = rig.run(backend, model, true).await?;
        assert!(rig.coordinator.stop_run(remote.run, "Tara").await?);
        let (_, result) = scripted();
        let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
        let late = rig.deliver(&remote, success_completion(outcome, Some("cs-1".into()), remote.workspace.clone(), Vec::new())).await;
        assert!(late.is_err(), "{backend}: the runner is told its lease is gone");

        let (local, remote) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?);
        for (path, snapshot) in [("local", &local), ("remote", &remote)] {
            assert_eq!(snapshot.status, "failed", "{backend} {path}");
            assert_eq!(snapshot.output, None, "{backend} {path} keeps the closed outcome");
            assert!(snapshot.error.as_deref().is_some_and(|error| error.starts_with("Stopped by Tara")), "{backend} {path}: {:?}", snapshot.error);
            assert!(!snapshot.announcements.iter().any(|name| name == "run.completed"), "{backend} {path} never announces a closed run as completed");
            assert_eq!(snapshot.tokens, (130, 800, 70), "{backend} {path} keeps what the stopped run spent");
            assert!(snapshot.cost > 0.0, "{backend} {path} keeps its cost: {}", snapshot.cost);
            assert_eq!(snapshot.cli_session, None, "{backend} {path} never touches the session of a closed run");
            assert_eq!(snapshot.context, 0, "{backend} {path}");
        }
        for ((step, on_local), (_, on_remote)) in local.checkpoints().into_iter().zip(remote.checkpoints()) {
            if matches!(step, "events" | "announcements") {
                continue;
            }
            assert_eq!(on_local, on_remote, "{backend}: the workers first differ at: {step}");
        }
    }
    Ok(())
}

impl Rig {
    async fn close(&self, run: &Run, remote: bool) -> anyhow::Result<()> {
        if remote {
            anyhow::ensure!(self.coordinator.stop_run(run.run, "Tara").await?);
        } else {
            sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Stopped by Tara.', finished_at = NOW() WHERE id = $1")
                .bind(run.run)
                .execute(self.ctx.db.pool())
                .await?;
        }
        Ok(())
    }

    async fn late(&self, run: &Run, remote: bool) -> anyhow::Result<()> {
        let (events, result) = scripted();
        self.late_with(run, remote, without_session_start(events), result).await
    }

    async fn late_with(&self, run: &Run, remote: bool, events: Vec<StreamEvent>, result: CliResult) -> anyhow::Result<()> {
        if remote {
            let session_id = result.session_id.clone();
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
            let late = self.deliver(run, success_completion(outcome, session_id, run.workspace.clone(), Vec::new())).await;
            anyhow::ensure!(late.is_err(), "the runner is told its lease is gone");
            Ok(())
        } else {
            self.locally(run, events, result, &Redactor::default()).await
        }
    }

    async fn next_run(&self, base: &Run, remote: bool) -> anyhow::Result<Run> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, started_at) VALUES ($1, $2, 'direct', $3, 'next', CASE WHEN $3 = 'running' THEN NOW() END) RETURNING id",
        )
        .bind(base.session)
        .bind(base.agent.id)
        .bind(if remote { "queued" } else { "running" })
        .fetch_one(self.ctx.db.pool())
        .await?;
        if !remote {
            return Ok(Run { agent: base.agent.clone(), session: base.session, run: id, workspace: base.workspace.clone(), runner: base.runner, lease: None });
        }
        let claim = self.runners.claim_kinds(base.runner, &format!("mcp-{}", Uuid::new_v4()), &["direct".to_string()]).await?.expect("claimed");
        assert_eq!(claim.run_id, id);
        Ok(Run { agent: base.agent.clone(), session: base.session, run: id, workspace: base.workspace.clone(), runner: base.runner, lease: Some(claim.lease_id) })
    }

    async fn newer_run(&self, run: &Run, status: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'direct', $3, 'later')")
            .bind(run.session)
            .bind(run.agent.id)
            .bind(status)
            .execute(self.ctx.db.pool())
            .await?;
        Ok(())
    }

    async fn advance_session(&self, run: &Run) -> anyhow::Result<()> {
        sqlx::query("UPDATE agent_sessions SET cli_session_id = 'cs-9', usage_baseline = '{\"input_tokens\": 999}'::jsonb, context_tokens = 77 WHERE id = $1")
            .bind(run.session)
            .execute(self.ctx.db.pool())
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn a_late_completion_never_rewinds_a_session_a_newer_run_moved_on_for_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    for newer in [None, Some("queued"), Some("completed")] {
        for remote in [false, true] {
            let path = if remote { "remote" } else { "local" };
            let run = rig.run("claude", "sonnet", remote).await?;
            rig.close(&run, remote).await?;
            if let Some(status) = newer {
                rig.newer_run(&run, status).await?;
                rig.advance_session(&run).await?;
            }
            let before = rig.snapshot(&run).await?;
            rig.late(&run, remote).await?;
            let after = rig.snapshot(&run).await?;
            assert_eq!(after.tokens, (130, 800, 70), "{path} newer {newer:?}: the closed run still keeps its spend");
            assert!(after.cost > 0.0, "{path} newer {newer:?}");
            assert_eq!((after.cli_session, after.context), (before.cli_session, before.context), "{path} newer {newer:?}: the session is left as it was");
            for (key, held) in before.baseline.as_object().into_iter().flatten().filter(|(_, held)| held.is_number()) {
                assert!(after.baseline[key].as_f64() >= held.as_f64(), "{path} newer {newer:?}: the baseline never goes down at {key}");
            }
            assert_eq!((after.status, after.output), ("failed".to_string(), None), "{path} newer {newer:?}");
        }
    }
    Ok(())
}

fn codex_total(total: (u64, u64, u64)) -> CliResult {
    CliResult {
        output: "done".into(),
        session_id: Some("thread-1".into()),
        cost_usd: 0.0,
        exit_code: 0,
        usage: crate::cli::Usage { input_tokens: total.0, cached_input_tokens: total.1, output_tokens: total.2, context_tokens: 0 },
        checkpoint: None,
    }
}

impl Rig {
    async fn complete_live(&self, run: &Run, remote: bool, result: CliResult) -> anyhow::Result<()> {
        if remote {
            let session_id = result.session_id.clone();
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
            self.deliver(run, success_completion(outcome, session_id, run.workspace.clone(), Vec::new())).await
        } else {
            self.locally(run, Vec::new(), result, &Redactor::default()).await
        }
    }

    async fn billed(&self, session: Uuid) -> anyhow::Result<(i64, i64, i64)> {
        Ok(sqlx::query_as(
            "SELECT COALESCE(SUM(input_tokens), 0)::bigint, COALESCE(SUM(cached_input_tokens), 0)::bigint, COALESCE(SUM(output_tokens), 0)::bigint FROM session_runs WHERE session_id = $1",
        )
        .bind(session)
        .fetch_one(self.ctx.db.pool())
        .await?)
    }
}

#[tokio::test]
async fn a_late_codex_completion_and_a_live_run_on_one_thread_bill_it_once_in_either_order() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    for remote in [false, true] {
        for late_first in [true, false] {
            let path = format!("{} {}", if remote { "remote" } else { "local" }, if late_first { "late first" } else { "live first" });
            let stopped = rig.run("codex", "gpt-5", remote).await?;
            rig.close(&stopped, remote).await?;
            if late_first {
                rig.late_with(&stopped, remote, Vec::new(), codex_total((1_000, 400, 100))).await?;
                assert_eq!(rig.snapshot(&stopped).await?.tokens, (1_000, 400, 100), "{path}: the stopped run keeps its spend");
            }
            let live = rig.next_run(&stopped, remote).await?;
            rig.complete_live(&live, remote, codex_total((1_500, 700, 180))).await?;
            if !late_first {
                rig.late_with(&stopped, remote, Vec::new(), codex_total((1_000, 400, 100))).await?;
            }
            assert_eq!(rig.billed(stopped.session).await?, (1_500, 700, 180), "{path}: the thread is billed once in total");
            let baseline = rig.snapshot(&stopped).await?.baseline;
            assert_eq!(baseline["input_tokens"].as_u64(), Some(1_500), "{path}: the thread baseline ends at the highest total");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_failure_completion_repeated_for_an_already_failed_lease_keeps_its_spend_once() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let run = rig.run("claude", "sonnet", true).await?;
    rig.close(&run, true).await?;
    let (_, result) = scripted();
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
    let mut failed = success_completion(outcome, Some("cs-1".into()), run.workspace.clone(), Vec::new());
    failed.exit_code = 1;
    failed.error = Some("the CLI crashed".into());
    for attempt in 1..=3 {
        assert!(rig.deliver(&run, failed.clone()).await.is_err(), "attempt {attempt}: the runner is told its lease is gone");
        let snapshot = rig.snapshot(&run).await?;
        assert_eq!(snapshot.tokens, (130, 800, 70), "attempt {attempt}: counted once");
        assert_eq!(snapshot.status, "failed");
        assert!(snapshot.error.as_deref().is_some_and(|error| error.starts_with("Stopped by Tara")), "{:?}", snapshot.error);
    }
    Ok(())
}

#[tokio::test]
async fn a_late_completion_for_a_publish_run_keeps_no_spend_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    for remote in [false, true] {
        let path = if remote { "remote" } else { "local" };
        let run = rig.run_of("publish", "claude", "sonnet", remote).await?;
        rig.close(&run, remote).await?;
        let (events, result) = scripted();
        if remote {
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
            assert!(rig.deliver(&run, success_completion(outcome, None, run.workspace.clone(), Vec::new())).await.is_err());
        } else {
            rig.locally(&run, without_session_start(events), result, &Redactor::default()).await?;
        }
        let snapshot = rig.snapshot(&run).await?;
        assert_eq!((snapshot.tokens, snapshot.cost), ((0, 0, 0), 0.0), "{path}: a publish run is never charged");
    }
    Ok(())
}

#[tokio::test]
async fn a_late_completion_posted_twice_is_counted_once_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    for remote in [false, true] {
        let path = if remote { "remote" } else { "local" };
        let run = rig.run("claude", "sonnet", remote).await?;
        rig.close(&run, remote).await?;
        rig.late(&run, remote).await?;
        let first = rig.snapshot(&run).await?;
        assert_eq!(first.tokens, (130, 800, 70), "{path}");
        let first_cost = first.cost;
        rig.late(&run, remote).await?;
        let second = rig.snapshot(&run).await?;
        assert_eq!(second.tokens, (130, 800, 70), "{path} a repeat adds nothing");
        assert!((second.cost - first_cost).abs() < 1e-9, "{path} a repeat adds no cost");
    }
    Ok(())
}

#[tokio::test]
async fn a_completion_sent_twice_is_applied_once() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, _) = no_secrets();
    let remote = rig.run("claude", "sonnet", true).await?;
    let (events, result) = scripted();
    rig.remotely(&remote, events, result, &secrets).await?;
    let first = rig.snapshot(&remote).await?;
    let (_, again) = answered(0);
    let outcome = ExecutionOutcome { result: again, pr_url: None, pr_number: None, publish_pr_state: None, work_summary: None };
    let second = rig.deliver(&remote, success_completion(outcome, Some("cs-1".into()), remote.workspace.clone(), Vec::new())).await;
    assert!(second.is_ok() || second.is_err(), "the answer to the runner does not matter, the state does");
    let after = rig.snapshot(&remote).await?;
    for ((step, before), (_, now)) in first.checkpoints().into_iter().zip(after.checkpoints()) {
        assert_eq!(before, now, "a repeated completion changed: {step}");
    }
    Ok(())
}

#[tokio::test]
async fn a_lost_lease_cannot_complete_the_run() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, _) = no_secrets();
    let remote = rig.run("claude", "sonnet", true).await?;
    sqlx::query("UPDATE runner_leases SET status = 'expired' WHERE id = $1").bind(remote.lease).execute(rig.ctx.db.pool()).await?;
    let (events, result) = scripted();
    assert!(rig.remotely(&remote, events, result, &secrets).await.is_err());
    let snapshot = rig.snapshot(&remote).await?;
    assert_ne!(snapshot.status, "completed");
    assert_eq!(snapshot.output, None);
    assert_eq!(snapshot.tokens, (0, 0, 0));
    Ok(())
}

#[tokio::test]
async fn the_workers_differ_only_where_the_list_says() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let long = || {
        let (mut events, result) = scripted();
        events.insert(2, StreamEvent::ToolResult { tool: "Bash".into(), output: "x".repeat(5_000) });
        (events, result)
    };
    let local = rig.run("claude", "sonnet", false).await?;
    let (events, result) = long();
    rig.locally(&local, events, result, &redactor).await?;
    let remote = rig.run("claude", "sonnet", true).await?;
    let (events, result) = long();
    rig.remotely(&remote, events, result, &secrets).await?;
    let (local, remote) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?);
    let longest = |snapshot: &Snapshot| snapshot.events.iter().map(|(_, _, body)| body.chars().count()).max().unwrap_or(0);
    assert_eq!((longest(&local), longest(&remote)), (EVENT_MAX_CHARS, EVENT_MAX_CHARS), "both workers clip an event at the one shared limit");

    let banner = |snapshot: &Snapshot| snapshot.events.iter().filter(|(kind, _, body)| kind == "text" && body.starts_with("Session started: ")).count();
    assert_eq!((banner(&local), banner(&remote)), (0, 1), "known difference: only the runner path shows a session banner, the local worker saves the session id at once");
    same(&local, &remote.without_session_banner());
    Ok(())
}

fn steps() -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUse { tool: "Bash".into(), input_summary: "cargo build".into() },
        StreamEvent::ToolResult { tool: "Bash".into(), output: "Compiling".into() },
        StreamEvent::AssistantText("Building now.".into()),
    ]
}

#[tokio::test]
async fn an_aside_leaves_the_same_events_and_no_activity_line_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let local = rig.run_of("aside", "claude", "sonnet", false).await?;
    let record = rig.ctx.store.get_run(local.run).await?.expect("run");
    let (tx, activity) = spawn_activity(rig.ctx.store.clone(), record, redactor);
    for event in steps() {
        tx.send(event)?;
    }
    drop(tx);
    activity.await?;
    let remote = rig.run_of("aside", "claude", "sonnet", true).await?;
    rig.show(&remote, steps(), &secrets).await?;

    let (local, remote) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?);
    assert_eq!(local.events.len(), 3, "an aside's steps are recorded on the local worker too");
    assert_eq!(local.events, remote.events);
    assert!(local.announcements.is_empty() && remote.announcements.is_empty(), "an aside shows no live activity line: {:?} {:?}", local.announcements, remote.announcements);
    Ok(())
}

#[tokio::test]
async fn a_run_refused_for_lack_of_disk_waits_the_same_way_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (_, redactor) = no_secrets();
    let full = || anyhow::anyhow!("No space left on device (os error 28)").context("Failed to start Claude Code");
    let pool = rig.ctx.db.pool();
    let waiting = |run: Uuid| async move {
        let state: (String, Option<String>, Option<String>, bool) =
            sqlx::query_as("SELECT status, waiting_for, error, not_before > NOW() FROM session_runs WHERE id = $1").bind(run).fetch_one(pool).await?;
        anyhow::Ok(state)
    };
    for kind in ["task", "followup", "review"] {
        let local = rig.run_of(kind, "claude", "sonnet", false).await?;
        let record = rig.ctx.store.get_run(local.run).await?.expect("run");
        fail_with(&rig.ctx, &record, &rig.session(&local).await?, &full(), &redactor).await?;
        let remote = rig.run_of(kind, "claude", "sonnet", true).await?;
        rig.deliver(&remote, errored_completion(&full(), None, remote.workspace.clone(), false)).await?;

        for (path, run) in [("local", &local), ("remote", &remote)] {
            let (status, waiting_for, error, later) = waiting(run.run).await?;
            assert_eq!((status.as_str(), waiting_for.as_deref(), error, later), ("queued", Some("disk"), None, true), "{path} {kind} waits for room");
        }
        let (local_view, remote_view) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?);
        assert_eq!(local_view.announcements, ["run.waiting"], "{kind}: nothing says the run failed");
        assert_eq!(local_view.announcements, remote_view.announcements, "{kind}");

        let record = rig.ctx.store.get_run(local.run).await?.expect("run");
        sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1").bind(local.run).execute(rig.ctx.db.pool()).await?;
        rig.ctx.store.release_space_wait(&record).await?;
        let (_, waiting_for, _, _) = waiting(local.run).await?;
        assert_eq!(waiting_for, None, "{kind}: a run that starts no longer waits");
        let released: Vec<Value> = sqlx::query_scalar("SELECT payload FROM world_events WHERE run_id = $1 AND type = 'run.waiting' ORDER BY seq")
            .bind(local.run)
            .fetch_all(rig.ctx.db.pool())
            .await?;
        let flags: Vec<bool> = released.iter().filter_map(|payload| payload["waiting"].as_bool()).collect();
        assert_eq!(flags, [true, false], "{kind}: the wait is announced once and lifted once");
    }
    Ok(())
}

#[tokio::test]
async fn a_disk_failure_after_the_run_did_work_or_for_other_runs_still_fails_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let full = || anyhow::anyhow!("No space left on device (os error 28)");
    for (kind, worked) in [("task", true), ("direct", false)] {
        let local = rig.run_of(kind, "claude", "sonnet", false).await?;
        let record = rig.ctx.store.get_run(local.run).await?.expect("run");
        if worked {
            let (tx, activity) = spawn_activity(rig.ctx.store.clone(), record.clone(), redactor.clone());
            for event in steps() {
                tx.send(event)?;
            }
            drop(tx);
            activity.await?;
        }
        fail_with(&rig.ctx, &record, &rig.session(&local).await?, &full(), &redactor).await?;
        let remote = rig.run_of(kind, "claude", "sonnet", true).await?;
        if worked {
            rig.show(&remote, steps(), &secrets).await?;
        }
        rig.deliver(&remote, errored_completion(&full(), None, remote.workspace.clone(), false)).await?;
        for (path, run) in [("local", &local), ("remote", &remote)] {
            let snapshot = rig.snapshot(run).await?;
            assert_eq!(snapshot.status, "failed", "{path} {kind}");
            assert!(snapshot.announcements.iter().any(|name| name == "run.failed"), "{path} {kind}: {:?}", snapshot.announcements);
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_local_worker_starts_a_fork_from_its_memory_and_a_new_agent_from_nothing() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let pool = rig.ctx.db.pool();
    let tag = Uuid::new_v4().simple().to_string();
    let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name, world_role) VALUES ($1, 'Ana', 'write') RETURNING id")
        .bind(format!("local-first-{tag}@example.test"))
        .fetch_one(pool)
        .await?;
    let source: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Source', 'claude', 'sonnet', $2) RETURNING id")
        .bind(format!("local-source-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
    sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body) VALUES ($1, 'user', $2, 'remember the launch plan')")
        .bind(source)
        .bind(owner)
        .execute(pool)
        .await?;
    let fork = rig.ctx.store.fork_agent(source, owner, &format!("local-fork-{tag}"), "Fork", "Ana").await?.expect("fork").agent_id;
    let fresh: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Fresh', 'claude', 'sonnet', $2) RETURNING id")
        .bind(format!("local-fresh-{tag}"))
        .bind(owner)
        .fetch_one(pool)
        .await?;
    sqlx::query("UPDATE agent_profiles SET runner_id = NULL WHERE id = ANY($1)").bind(vec![fork, fresh]).execute(pool).await?;

    for (id, remembers) in [(fork, true), (fresh, false)] {
        let agent = rig.ctx.store.get_agent(id).await?.expect("agent");
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE agent_id = $1").bind(id).fetch_one(pool).await?;
        let (session, handoff) = crate::world::turn::ensure_main_session(&rig.ctx, &agent).await?;
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions WHERE agent_id = $1").bind(id).fetch_one(pool).await?;
        assert_eq!(after, before + i64::from(!remembers), "a fork keeps its memory session, a new agent gets one");
        assert!(handoff.is_none() && session.cli_session_id.is_none());
        let prompt = super::prompts::prepend_memory_handoff("[event] message", handoff.as_deref().or(session.summary.as_deref()), session.cli_session_id.is_none());
        assert_eq!(prompt.starts_with("[memory handoff]"), remembers, "only a fork starts from a remembered past");
        assert_eq!(prompt.contains("remember the launch plan"), remembers);
        assert!(prompt.ends_with("[event] message"), "the message itself reaches the CLI");
        assert!(session.working_dir.as_deref().is_some_and(|dir| !dir.trim().is_empty()), "every main session has a home folder");
    }
    sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(vec![source, fork, fresh]).execute(pool).await?;
    Ok(())
}
