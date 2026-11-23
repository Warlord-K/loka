use super::tests::test_ctx;
use super::{fail_with, session_id_after_run, settle_completed, spawn_activity, Ctx};
use crate::world::prompts::Action;
use crate::claude::TurnReader;
use crate::cli::{CliResult, Redactor, StreamEvent};
use crate::remote_runner::{errored_completion, map_event, success_completion, ExecutionOutcome};
use loka_agent::runner_server::RunnerCoordinator;
use loka_shared::cli_errors::{classify, FailureKind};
use loka_shared::context_budget::Prepared;
use loka_shared::context::{SentContext, INSTRUCTIONS_BLOCK, SKILLS_BLOCK};
use loka_shared::run_recording::EVENT_MAX_CHARS;
use loka_shared::runners::{BeginCompletion, CreateRunner, RunnerCompletion, RunnerStore};
use loka_shared::world::{clone_run, AgentSession, InboxItem, NewEvent, RunClone, WorldAgent};
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
    claimed: Option<loka_shared::runners::ClaimedRun>,
}

impl Rig {
    async fn run(&self, backend: &str, model: &str, remote: bool) -> anyhow::Result<Run> {
        self.run_of("direct", backend, model, remote).await
    }

    async fn run_of(&self, kind: &str, backend: &str, model: &str, remote: bool) -> anyhow::Result<Run> {
        self.run_with(kind, backend, model, remote, 0).await
    }

    async fn run_with(&self, kind: &str, backend: &str, model: &str, remote: bool, context_tokens: i64) -> anyhow::Result<Run> {
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
        if context_tokens > 0 {
            sqlx::query("UPDATE agent_sessions SET cli_session_id = 'cs-0', context_tokens = $2, context_sent = jsonb_build_object('skills', md5('skills')) WHERE id = $1")
                .bind(session)
                .bind(context_tokens)
                .execute(pool)
                .await?;
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
        let claimed = if remote {
            let claim = self
                .runners
                .claim_kinds(runner, &format!("mcp-{}", Uuid::new_v4()), &[kind.to_string()])
                .await?
                .expect("the run is claimed");
            assert_eq!(claim.run_id, run);
            Some(claim)
        } else {
            None
        };
        let lease = claimed.as_ref().map(|claim| claim.lease_id);
        let agent = self.ctx.store.get_agent(agent_id).await?.expect("agent");
        Ok(Run { agent, session, run, workspace, runner, lease, claimed })
    }

    async fn session(&self, run: &Run) -> anyhow::Result<AgentSession> {
        Ok(self.ctx.store.get_session(run.session).await?.expect("session"))
    }

    async fn locally(&self, run: &Run, events: Vec<StreamEvent>, result: CliResult, redactor: &Redactor) -> anyhow::Result<()> {
        self.locally_sending(run, events, result, None, redactor).await
    }

    async fn locally_sending(
        &self,
        run: &Run,
        events: Vec<StreamEvent>,
        result: CliResult,
        sent: Option<SentContext>,
        redactor: &Redactor,
    ) -> anyhow::Result<()> {
        let record = self.ctx.store.get_run(run.run).await?.expect("run");
        let (tx, activity) = spawn_activity(self.ctx.store.clone(), record.clone(), redactor.clone());
        for event in events {
            tx.send(event)?;
        }
        drop(tx);
        activity.await?;
        let session = self.session(run).await?;
        let saved_session_id = session_id_after_run(&session.backend, false, result.session_id.as_deref(), None).map(str::to_string);
        settle_completed(&self.ctx, &run.agent, session, &record, result, super::Kept { cli_session_id: saved_session_id, sent }, redactor).await
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
        let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
async fn accepted_chat_clarification_reaches_a_local_quest() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let turn = rig.run("claude", "sonnet", false).await?;
    let owner = turn.agent.owner_user_id.expect("owner");
    let task_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id) VALUES ($1, 'Build the canonical store', 'todo', $2) RETURNING id",
    )
    .bind(owner)
    .bind(turn.agent.id)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let bytes = b"canonical\0answer\xff";
    let upload = rig.ctx.store.insert_upload("application/octet-stream", "answer.bin", bytes, Some(owner)).await?;
    let request_tail = "REQUEST_AFTER_5500";
    let answer_tail = "ANSWER_AFTER_5500";
    let request = format!("{}{request_tail}", "request ".repeat(800));
    let answer = format!("{}{answer_tail}", "answer ".repeat(800));
    let payload = json!({
        "author": "Ana",
        "author_kind": "user",
        "message_id": Uuid::new_v4(),
        "body": answer,
        "clarification_continuation": Uuid::new_v4(),
        "clarification_handoff": {
            "request": request,
            "question": "Which database?",
            "answer": answer
        },
        "attachments": [{ "id": upload.id, "name": "answer.bin", "mime": "application/octet-stream" }]
    });
    let seq = rig
        .ctx
        .store
        .emit(
            NewEvent::new("message.created")
                .agent(turn.agent.id)
                .actor(Some(owner))
                .payload(payload),
            Some((turn.agent.id, "message")),
        )
        .await?;
    let item = sqlx::query_as::<_, InboxItem>(
        "UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1 AND event_seq = $2
         RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
    )
    .bind(turn.agent.id)
    .bind(seq)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert_eq!(item.payload.get("from_owner").and_then(Value::as_bool), Some(true));
    let main = rig.session(&turn).await?;
    let task = rig.ctx.db.get_task(task_id).await?.expect("task");
    let invalid: Action = serde_json::from_value(json!({
        "type": "start_task",
        "task_id": task_id,
        "brief": "must not run",
        "clarification_source": "msg:missing"
    }))?;
    super::super::turn::apply_actions_for_contract(
        &rig.ctx,
        &turn.agent,
        &main,
        turn.run,
        &item,
        &task,
        invalid,
    )
    .await?;
    let started: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
        .bind(task_id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
    assert_eq!(started, 0);
    let action: Action = serde_json::from_value(json!({
        "type": "start_task",
        "task_id": task_id,
        "brief": "model supplied the wrong brief"
    }))?;
    super::super::turn::apply_actions_for_contract(
        &rig.ctx,
        &turn.agent,
        &main,
        turn.run,
        &item,
        &task,
        action,
    )
    .await?;
    let (resulting_run, prompt): (Uuid, String) = sqlx::query_as(
        "SELECT id, prompt FROM session_runs WHERE task_id = $1 AND kind IN ('task', 'followup') ORDER BY created_at DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert!(prompt.contains(request_tail));
    assert!(prompt.contains(answer_tail));
    assert_eq!(prompt.matches(request_tail).count(), 1);
    assert_eq!(prompt.matches(answer_tail).count(), 1);
    assert!(!prompt.contains("model supplied the wrong brief"));
    let leaked: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM agent_messages
           WHERE agent_id = $1 AND author_kind = 'system'
             AND (body LIKE $2 OR body LIKE '%answer.bin%')
         )",
    )
    .bind(turn.agent.id)
    .bind(format!("%{answer_tail}%"))
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert!(!leaked);
    let payloads = rig.ctx.store.run_answer_payloads(resulting_run).await?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["attachments"][0]["id"], json!(upload.id));
    let (_, stored) = rig.ctx.store.get_upload(upload.id, None).await?.expect("upload");
    assert_eq!(stored, bytes);
    Ok(())
}

#[tokio::test]
async fn a_local_clarification_retries_when_plan_handoff_has_not_started() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let turn = rig.run("claude", "sonnet", false).await?;
    let owner = turn.agent.owner_user_id.expect("owner");
    let task_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id, plan_first)
         VALUES ($1, 'Resume after plan', 'in_progress', $2, TRUE) RETURNING id",
    )
    .bind(owner)
    .bind(turn.agent.id)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let task_session: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_sessions (agent_id, role, backend, model, cli_session_id, status, task_id, plan_state, working_dir)
         VALUES ($1, 'task', 'claude', 'sonnet', 'task-thread', 'active', $2, 'awaiting', $3) RETURNING id",
    )
    .bind(turn.agent.id)
    .bind(task_id)
    .bind(&turn.workspace)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let item = sqlx::query_as::<_, InboxItem>(
        "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts)
         VALUES ($1, 'task.assigned', $2, 'processing', 1)
         RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
    )
    .bind(turn.agent.id)
    .bind(json!({
        "task_id": task_id,
        "title": "Resume after plan",
        "brief": "Original request:\nBuild it\n\nQuestion:\nContinue?\n\nTheir answer:\nYes",
        "resume": true,
        "clarification_continuation": Uuid::new_v4(),
        "clarification_handoff": {
            "request": "Build it",
            "question": "Continue?",
            "answer": "Yes"
        },
        "clarification_status": "in_progress",
        "clarification_resume": false,
        "attachments": [{ "id": Uuid::new_v4(), "name": "choice.txt", "mime": "text/plain" }],
    }))
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let task = rig.ctx.db.get_task(task_id).await?.expect("task");
    let main = rig.session(&turn).await?;
    let action: Action = serde_json::from_value(json!({ "type": "start_task", "task_id": task_id }))?;
    super::super::turn::apply_actions_for_contract(
        &rig.ctx,
        &turn.agent,
        &main,
        turn.run,
        &item,
        &task,
        action,
    )
    .await?;
    rig.ctx.store.finish_inbox(&[item.id], "done", None).await?;
    let waiting: (String, i32, bool) = sqlx::query_as(
        "SELECT status, attempts, retry_at = 'infinity' FROM agent_inbox WHERE id = $1",
    )
        .bind(item.id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
    assert_eq!(waiting, ("pending".into(), 1, true));
    let problems: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM world_events WHERE run_id = $1 AND type = 'actions.problem'",
    )
    .bind(turn.run)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert_eq!(problems, 0);
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1")
        .bind(task_id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
    assert_eq!(before, 0);

    assert!(!rig
        .ctx
        .store
        .take_pending_inbox(turn.agent.id, 20)
        .await?
        .iter()
        .any(|candidate| candidate.id == item.id));
    let approved = rig.ctx.store.approve_plan(task_id, "owner", "test", "Approve").await?;
    assert!(matches!(approved, loka_shared::quests::PlanApproval::Approved { .. }));
    let prompts: Vec<String> = sqlx::query_scalar(
        "SELECT prompt FROM session_runs WHERE task_id = $1 AND kind = 'followup' ORDER BY created_at",
    )
        .bind(task_id)
        .fetch_all(rig.ctx.db.pool())
        .await?;
    let links: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runner_run_inbox x JOIN session_runs r ON r.id = x.run_id
         WHERE r.task_id = $1 AND x.inbox_id = $2",
    )
    .bind(task_id)
    .bind(item.id)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Their answer:\nYes"));
    assert_eq!(prompts[0].matches("Their answer:\nYes").count(), 1);
    assert!(prompts[0].contains("choice.txt"));
    assert_eq!(links, 1);
    let settled: (String, i32) = sqlx::query_as("SELECT status, attempts FROM agent_inbox WHERE id = $1")
        .bind(item.id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
    assert_eq!(settled, ("done".into(), 1));
    assert!(!rig.ctx.store.take_pending_inbox(turn.agent.id, 20).await?.iter().any(|candidate| candidate.id == item.id));
    let session = rig.ctx.store.get_session(task_session).await?.expect("task session");
    assert_eq!(session.plan_state.as_deref(), Some("approved"));
    let failed_item = sqlx::query_as::<_, InboxItem>(
        "INSERT INTO agent_inbox (agent_id, kind, payload, status, attempts)
         VALUES ($1, 'task.assigned', $2, 'processing', 1)
         RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
    )
    .bind(turn.agent.id)
    .bind(json!({
        "task_id": task_id,
        "resume": true,
        "clarification_continuation": Uuid::new_v4(),
        "clarification_handoff": { "request": "Build it", "question": "Continue?", "answer": "Yes" },
        "clarification_status": "in_progress",
        "clarification_resume": false,
    }))
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let invalid: Action = serde_json::from_value(json!({
        "type": "start_task",
        "task_id": task_id,
        "plan_first": true,
        "attempts": 2,
    }))?;
    for attempt in 1..=3 {
        sqlx::query("UPDATE agent_inbox SET status = 'processing', attempts = $2, retry_at = NULL WHERE id = $1")
            .bind(failed_item.id)
            .bind(attempt)
            .execute(rig.ctx.db.pool())
            .await?;
        super::super::turn::apply_actions_for_contract(
            &rig.ctx,
            &turn.agent,
            &main,
            turn.run,
            &failed_item,
            &task,
            invalid.clone(),
        )
        .await?;
        let status: String = sqlx::query_scalar("SELECT status FROM agent_inbox WHERE id = $1")
            .bind(failed_item.id)
            .fetch_one(rig.ctx.db.pool())
            .await?;
        assert_eq!(status, if attempt == 3 { "failed" } else { "pending" });
    }
    let exhausted: (String, i32) = sqlx::query_as("SELECT status, attempts FROM agent_inbox WHERE id = $1")
        .bind(failed_item.id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
    assert_eq!(exhausted, ("failed".into(), 3));
    Ok(())
}

#[tokio::test]
async fn a_local_refused_clarification_is_explained() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let turn = rig.run("claude", "sonnet", false).await?;
    let owner = turn.agent.owner_user_id.expect("owner");
    let main = rig.session(&turn).await?;
    let task_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_tasks (user_id, title, status, assigned_agent_id)
         VALUES ($1, 'Closed request', 'todo', $2) RETURNING id",
    )
    .bind(owner)
    .bind(turn.agent.id)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    let seq = rig
        .ctx
        .store
        .emit(
            NewEvent::new("message.created")
                .agent(turn.agent.id)
                .actor(Some(owner))
                .task(Some(task_id))
                .payload(json!({
                    "message_id": Uuid::new_v4(),
                    "body": "Old request",
                    "from_owner": true,
                    "task_id": task_id,
                })),
            Some((turn.agent.id, "message")),
        )
        .await?;
    let item = sqlx::query_as::<_, InboxItem>(
        "UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1 AND event_seq = $2
         RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
    )
    .bind(turn.agent.id)
    .bind(seq)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    sqlx::query("UPDATE agent_tasks SET status = 'done' WHERE id = $1")
        .bind(task_id)
        .execute(rig.ctx.db.pool())
        .await?;
    sqlx::query("UPDATE agent_tasks SET completed_at = NOW() + INTERVAL '1 second' WHERE id = $1")
    .bind(task_id)
    .execute(rig.ctx.db.pool())
    .await?;
    let task = rig.ctx.db.get_task(task_id).await?.expect("task");
    let action: Action = serde_json::from_value(json!({
        "type": "ask",
        "text": "Which format?",
        "choices": ["CSV"],
        "clarify_request": true,
    }))?;
    super::super::turn::apply_actions_for_contract(
        &rig.ctx,
        &turn.agent,
        &main,
        turn.run,
        &item,
        &task,
        action,
    )
    .await?;
    let replies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_messages
         WHERE agent_id = $1 AND run_id = $2 AND body = $3",
    )
    .bind(turn.agent.id)
    .bind(turn.run)
    .bind(loka_shared::world::CLARIFICATION_POST_REFUSED)
    .fetch_one(rig.ctx.db.pool())
    .await?;
    assert_eq!(replies, 1);
    Ok(())
}

#[tokio::test]
async fn identical_local_clarifications_are_deduplicated_per_quest_origin() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let turn = rig.run("claude", "sonnet", false).await?;
    let owner = turn.agent.owner_user_id.expect("owner");
    let main = rig.session(&turn).await?;
    let question = "Which database should this use?";
    let mut origins = Vec::new();
    for (title, description, note) in [
        ("Repair primary export", "Primary CSV drops rows.", "Preserve the primary ordering."),
        ("Repair archive export", "Archive CSV drops rows.", "Preserve the archive ordering."),
    ] {
        let task_id: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, description, status, assigned_agent_id)
             VALUES ($1, $2, $3, 'todo', $4) RETURNING id",
        )
        .bind(owner)
        .bind(title)
        .bind(description)
        .bind(turn.agent.id)
        .fetch_one(rig.ctx.db.pool())
        .await?;
        let seq = rig
            .ctx
            .store
            .emit(
                NewEvent::new("task.assigned")
                    .agent(turn.agent.id)
                    .actor(Some(owner))
                    .task(Some(task_id))
                    .payload(json!({ "task_id": task_id, "brief": note })),
                Some((turn.agent.id, "task.assigned")),
            )
            .await?;
        let item = sqlx::query_as::<_, InboxItem>(
            "UPDATE agent_inbox SET status = 'processing' WHERE agent_id = $1 AND event_seq = $2
             RETURNING id, agent_id, kind, event_seq, payload, status, attempts, created_at",
        )
        .bind(turn.agent.id)
        .bind(seq)
        .fetch_one(rig.ctx.db.pool())
        .await?;
        let task = rig.ctx.db.get_task(task_id).await?.expect("task");
        let action: Action = serde_json::from_value(json!({
            "type": "ask",
            "text": question,
            "choices": ["Postgres", "SQLite"],
            "clarify_request": true,
            "clarify_source": format!("quest:{task_id}")
        }))?;
        let request = format!("{title}\n\n{description}\n\nTheir note: {note}");
        origins.push((task_id, item, task, action, request));
    }
    for _ in 0..2 {
        for (_, item, task, action, _) in &origins {
            super::super::turn::apply_actions_for_contract(
                &rig.ctx,
                &turn.agent,
                &main,
                turn.run,
                item,
                task,
                action.clone(),
            )
            .await?;
        }
    }
    let cards: Vec<(String, Value)> = sqlx::query_as(
        "SELECT body, meta->'clarify_request' FROM agent_messages
         WHERE agent_id = $1 AND run_id = $2 AND meta ? 'clarify_request'
         ORDER BY created_at, id",
    )
    .bind(turn.agent.id)
    .bind(turn.run)
    .fetch_all(rig.ctx.db.pool())
    .await?;
    assert_eq!(cards.len(), origins.len());
    assert!(cards.iter().all(|(body, _)| body == question));
    for (task_id, item, _, _, request) in &origins {
        let card = cards.iter().find(|(_, origin)| origin["task_id"] == json!(task_id)).expect("origin card");
        assert_eq!(card.1["source_inbox_id"], item.id);
        assert_eq!(card.1["request"], request.as_str());
    }
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
                claimed: None,
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
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
        let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
            return Ok(Run { agent: base.agent.clone(), session: base.session, run: id, workspace: base.workspace.clone(), runner: base.runner, lease: None, claimed: None });
        }
        let claim = self.runners.claim_kinds(base.runner, &format!("mcp-{}", Uuid::new_v4()), &["direct".to_string()]).await?.expect("claimed");
        assert_eq!(claim.run_id, id);
        let lease_id = claim.lease_id;
        Ok(Run { agent: base.agent.clone(), session: base.session, run: id, workspace: base.workspace.clone(), runner: base.runner, lease: Some(lease_id), claimed: Some(claim) })
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
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
            let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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
    let outcome = ExecutionOutcome { result: again, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: None, published_head: None };
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

impl Rig {
    async fn on_quest(&self, run: &Run, title: &str) -> anyhow::Result<Uuid> {
        let pool = self.ctx.db.pool();
        let owner = run.agent.owner_user_id.expect("owner");
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, $2, 'in_progress') RETURNING id")
            .bind(owner)
            .bind(title)
            .fetch_one(pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET role = 'task', task_id = $2, context_tokens = 150000 WHERE id = $1")
            .bind(run.session)
            .bind(quest)
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt, output, created_at)
             VALUES ($1, $2, $3, 'task', 'completed', $4, 'ok', NOW() - INTERVAL '1 hour')",
        )
        .bind(run.session)
        .bind(run.agent.id)
        .bind(quest)
        .bind(format!("[task session] {title}"))
        .execute(pool)
        .await?;
        sqlx::query("UPDATE session_runs SET task_id = $2 WHERE id = $1").bind(run.run).bind(quest).execute(pool).await?;
        Ok(quest)
    }

    async fn recovery_of(&self, run: &Run) -> anyhow::Result<Option<(String, String, Option<String>, Option<String>)>> {
        Ok(sqlx::query_as(
            "SELECT kind, status, retry_cause, context_path FROM session_runs
             WHERE session_id = $1 AND id <> $2 AND context_note->>'recovers' = $2::text",
        )
        .bind(run.session)
        .bind(run.run)
        .fetch_optional(self.ctx.db.pool())
        .await?)
    }
}

fn twenty_steps() -> Vec<StreamEvent> {
    (0..10).flat_map(|_| steps().into_iter().take(1).chain(std::iter::once(StreamEvent::AssistantText("Still going.".into())))).collect()
}

#[tokio::test]
async fn a_context_overflow_late_in_a_run_continues_once_in_a_fresh_session_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let overflow = || anyhow::anyhow!("API Error: 400 prompt is too long: 214000 tokens > 200000 maximum").context("Claude Code exited with code 1");
    for kind in ["followup", "direct"] {
        let local = rig.run_of(kind, "claude", "sonnet", false).await?;
        let remote = rig.run_of(kind, "claude", "sonnet", true).await?;
        let mut quests = Vec::new();
        if kind == "followup" {
            quests.push(rig.on_quest(&local, "Ship the blue widget").await?);
            quests.push(rig.on_quest(&remote, "Ship the blue widget").await?);
        }
        let record = rig.ctx.store.get_run(local.run).await?.expect("run");
        let (tx, activity) = spawn_activity(rig.ctx.store.clone(), record.clone(), redactor.clone());
        for event in twenty_steps() {
            tx.send(event)?;
        }
        drop(tx);
        activity.await?;
        fail_with(&rig.ctx, &record, &rig.session(&local).await?, &overflow(), &redactor).await?;
        rig.show(&remote, twenty_steps(), &secrets).await?;
        rig.deliver(&remote, errored_completion(&overflow(), None, remote.workspace.clone(), false)).await?;

        let expected_kind = if kind == "followup" { "task" } else { kind };
        for (path, run) in [("local", &local), ("remote", &remote)] {
            let snapshot = rig.snapshot(run).await?;
            assert_eq!(snapshot.status, "failed", "{path} {kind}");
            assert!(!snapshot.announcements.iter().any(|name| name == "run.failed"), "{path} {kind}: {:?}", snapshot.announcements);
            let recovery = rig.recovery_of(run).await?.unwrap_or_else(|| panic!("{path} {kind}: an overflow after 20 steps continues in a fresh session"));
            assert_eq!(
                recovery,
                (expected_kind.to_string(), "queued".to_string(), Some("context_recovery".to_string()), Some("recovery".to_string())),
                "{path} {kind}"
            );
            let session = rig.session(run).await?;
            assert_eq!((session.cli_session_id, session.context_tokens), (None, 0), "{path} {kind}: the next run starts a fresh CLI session");
        }
        for quest in quests {
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(quest).fetch_one(rig.ctx.db.pool()).await?;
            assert_eq!(status, "in_progress", "{kind}: the quest is not failed while it recovers");
        }
        let (local_view, remote_view) = (rig.snapshot(&local).await?, rig.snapshot(&remote).await?.without_session_banner());
        assert_eq!(local_view.announcements, remote_view.announcements, "{kind}");
    }
    Ok(())
}

impl Rig {
    async fn overflow_session(&self, run: &Run, tokens: i64) -> anyhow::Result<()> {
        sqlx::query("UPDATE agent_sessions SET context_tokens = $2 WHERE id = $1").bind(run.session).bind(tokens).execute(self.ctx.db.pool()).await?;
        Ok(())
    }

    async fn path_of(&self, run: Uuid) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT context_path FROM session_runs WHERE id = $1").bind(run).fetch_one(self.ctx.db.pool()).await?)
    }

    async fn queue_after(&self, run: &Run, prompt: &str) -> anyhow::Result<Uuid> {
        let mut tx = self.ctx.db.pool().begin().await?;
        let later = RunClone { kind: None, fork_from: None, prompt, activity: None, retry_cause: None, not_before: None, retry: false };
        let id = clone_run(&mut tx, run.run, later).await?.id;
        tx.commit().await?;
        Ok(id)
    }

    async fn on_review(&self, run: &Run) -> anyhow::Result<Uuid> {
        let pool = self.ctx.db.pool();
        let owner = run.agent.owner_user_id.expect("owner");
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, status) VALUES ($1, 'Review the widget', 'in_progress') RETURNING id")
            .bind(owner)
            .fetch_one(pool)
            .await?;
        let task_session: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, status, task_id, working_dir, cli_session_id, runner_id, summary)
             SELECT agent_id, 'task', backend, model, 'completed', $2, working_dir, 'task-cs', runner_id,
                    '{\"status\": \"done\", \"summary\": \"Built the widget.\"}'
             FROM agent_sessions WHERE id = $1
             RETURNING id",
        )
        .bind(run.session)
        .bind(quest)
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE agent_sessions SET role = 'review', task_id = $2, parent_session_id = $3 WHERE id = $1")
            .bind(run.session)
            .bind(quest)
            .bind(task_session)
            .execute(pool)
            .await?;
        sqlx::query("UPDATE session_runs SET task_id = $2 WHERE id = $1").bind(run.run).bind(quest).execute(pool).await?;
        Ok(quest)
    }
}

#[tokio::test]
async fn a_session_over_budget_is_compacted_or_checkpointed_the_same_way_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    for (backend, model, native) in [("claude", "sonnet", true), ("cursor", "composer-2.5", false)] {
        let local = rig.run_with("followup", backend, model, false, 190_000).await?;
        let remote = rig.run_with("followup", backend, model, true, 190_000).await?;

        let mut record = rig.ctx.store.get_run(local.run).await?.expect("run");
        let mut session = rig.session(&local).await?;
        let prepared = super::prepare_request(&rig.ctx, &mut record, &mut session).await?;
        let local_compacts = crate::cli::resumes_to_compact(prepared.compact_first(), session.cli_session_id.is_some(), false, session.cli_session_id.is_some(), false);

        let claimed = remote.claimed.clone().expect("claimed");
        let job = rig.coordinator.build_job(claimed, "token".into()).await?;
        let remote_compacts = crate::remote_runner::job_compacts_first(&job);

        assert_eq!((local_compacts, remote_compacts), (native, native), "{backend}: native compaction runs first on both workers");
        assert_eq!(SentContext::carried(session.context_sent.as_ref(), true), SentContext::default(), "{backend}: local starts with nothing marked as sent");
        assert_eq!(job.context_sent, SentContext::default(), "{backend}: the runner job starts with nothing marked as sent");
        if native {
            assert_eq!((record.prompt.as_str(), job.prompt.as_str()), ("go", "go"), "{backend}: the request itself is unchanged");
            assert_eq!((session.cli_session_id.as_deref(), job.cli_session_id.as_deref()), (Some("cs-0"), Some("cs-0")));
        } else {
            for (path, prompt) in [("local", &record.prompt), ("remote", &job.prompt)] {
                assert!(prompt.starts_with("[context checkpoint]") && prompt.ends_with("Latest message:\ngo"), "{backend} {path}: {prompt}");
            }
            assert_eq!(session.cli_session_id, None, "{backend}: the local run starts a fresh CLI session");
            assert!(!job.resume && job.cli_session_id.is_none() && job.previous_run_id.is_none(), "{backend}: the runner starts a fresh CLI session");
        }
        let expected = Some(if native { "native" } else { "checkpoint" }.to_string());
        assert_eq!((rig.path_of(local.run).await?, rig.path_of(remote.run).await?), (expected.clone(), expected), "{backend}");
    }
    Ok(())
}

#[tokio::test]
async fn a_recovery_is_claimed_before_anything_queued_after_the_run_that_overflowed() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (_, redactor) = no_secrets();
    let overflow = || anyhow::anyhow!("Prompt is too long");
    let local = rig.run_of("direct", "claude", "sonnet", false).await?;
    let local_later = rig.queue_after(&local, "and then this").await?;
    let record = rig.ctx.store.get_run(local.run).await?.expect("run");
    fail_with(&rig.ctx, &record, &rig.session(&local).await?, &overflow(), &redactor).await?;
    let local_recovery = rig.recovery_of(&local).await?.expect("recovered");
    assert_eq!(local_recovery.0, "direct");
    let order: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM session_runs WHERE session_id = $1 AND status = 'queued' ORDER BY created_at")
        .bind(local.session)
        .fetch_all(rig.ctx.db.pool())
        .await?;
    assert_eq!(order.len(), 2);
    assert_ne!(order[0], local_later, "the local claim order puts the recovery first");

    let remote = rig.run_of("direct", "claude", "sonnet", true).await?;
    let remote_later = rig.queue_after(&remote, "and then this").await?;
    rig.deliver(&remote, errored_completion(&overflow(), None, remote.workspace.clone(), false)).await?;
    let claim = rig.runners.claim_kinds(remote.runner, &format!("mcp-{}", Uuid::new_v4()), &["direct".to_string()]).await?.expect("claimed");
    assert_ne!(claim.run_id, remote_later, "the runner claims the recovery first");
    let recovered_by: Option<String> = sqlx::query_scalar("SELECT context_note->>'recovered_by' FROM session_runs WHERE id = $1").bind(remote.run).fetch_one(rig.ctx.db.pool()).await?;
    assert_eq!(recovered_by, Some(claim.run_id.to_string()));
    Ok(())
}

#[tokio::test]
async fn a_checkpointed_verdict_reminder_still_settles_its_review_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let (secrets, redactor) = no_secrets();
    let unfinished = || {
        let (_, mut result) = answered(0);
        result.output = "Looks fine so far.".into();
        result
    };
    let local = rig.run_with("review", "cursor", "composer-2.5", false, 1_000).await?;
    let remote = rig.run_with("review", "cursor", "composer-2.5", true, 1_000).await?;
    let quests = [rig.on_review(&local).await?, rig.on_review(&remote).await?];
    rig.locally(&local, Vec::new(), unfinished(), &redactor).await?;
    rig.remotely(&remote, Vec::new(), unfinished(), &secrets).await?;

    let failure = || anyhow::anyhow!("Cursor exited with code 1: unexpected end of stream");
    let mut nudges = Vec::new();
    for (path, run) in [("local", &local), ("remote", &remote)] {
        let nudge: Uuid = sqlx::query_scalar("SELECT id FROM session_runs WHERE session_id = $1 AND status = 'queued' AND nudge = 'verdict'")
            .bind(run.session)
            .fetch_one(rig.ctx.db.pool())
            .await
            .unwrap_or_else(|error| panic!("{path}: a review without a verdict gets one reminder: {error}"));
        rig.overflow_session(run, 190_000).await?;
        if path == "local" {
            sqlx::query("UPDATE session_runs SET status = 'running', started_at = NOW() WHERE id = $1").bind(nudge).execute(rig.ctx.db.pool()).await?;
            let mut record = rig.ctx.store.get_run(nudge).await?.expect("nudge");
            let mut session = rig.session(run).await?;
            assert!(matches!(super::prepare_request(&rig.ctx, &mut record, &mut session).await?, Prepared::Checkpoint { .. }));
            let (tx, activity) = spawn_activity(rig.ctx.store.clone(), record.clone(), redactor.clone());
            for event in steps() {
                tx.send(event)?;
            }
            drop(tx);
            activity.await?;
            fail_with(&rig.ctx, &record, &session, &failure(), &redactor).await?;
        } else {
            let claim = rig.runners.claim_kinds(run.runner, &format!("mcp-{}", Uuid::new_v4()), &["review".to_string()]).await?.expect("claimed");
            assert_eq!(claim.run_id, nudge);
            let lease_id = claim.lease_id;
            let job = rig.coordinator.build_job(claim, "token".into()).await?;
            assert!(job.prompt.starts_with("[context checkpoint]"), "{}", job.prompt);
            let step = Run { lease: Some(lease_id), run: nudge, claimed: None, ..rig_copy(run) };
            rig.show(&step, steps(), &secrets).await?;
            rig.deliver(&step, errored_completion(&failure(), None, run.workspace.clone(), false)).await?;
        }
        nudges.push(nudge);
    }
    for (nudge, quest) in nudges.iter().zip(quests) {
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM session_runs WHERE id = $1").bind(nudge).fetch_one(rig.ctx.db.pool()).await?;
        assert!(!prompt.starts_with(loka_shared::world::VERDICT_NUDGE_MARK), "the checkpoint rewrote the reminder's prompt");
        let retried: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND retry_cause IS NOT NULL").bind(quest).fetch_one(rig.ctx.db.pool()).await?;
        assert_eq!(retried, 0, "the failure is not one Loka retries, so the reminder rule decides");
        let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1").bind(quest).fetch_one(rig.ctx.db.pool()).await?;
        let error: Option<String> = sqlx::query_scalar("SELECT error_message FROM agent_tasks WHERE id = $1").bind(quest).fetch_one(rig.ctx.db.pool()).await?;
        assert_ne!(status, "failed", "a failed verdict reminder settles the review instead of failing the quest: {error:?}");
    }
    let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = ANY($1) ORDER BY id = $2 DESC")
        .bind(quests.to_vec())
        .bind(quests[0])
        .fetch_all(rig.ctx.db.pool())
        .await?;
    assert_eq!(statuses[0], statuses[1], "both workers settle it the same way");
    Ok(())
}

fn rig_copy(run: &Run) -> Run {
    Run {
        agent: run.agent.clone(),
        session: run.session,
        run: run.run,
        workspace: run.workspace.clone(),
        runner: run.runner,
        lease: run.lease,
        claimed: None,
    }
}

fn with_output(output: &str) -> (Vec<StreamEvent>, CliResult) {
    let (events, mut result) = scripted();
    result.output = output.to_string();
    (events, result)
}

impl Rig {
    async fn take_prework_run(&self, source: &Run, run_id: Uuid, remote: bool) -> anyhow::Result<(Run, String)> {
        if remote {
            let claim = self
                .runners
                .claim_kinds(source.runner, &format!("mcp-{}", Uuid::new_v4()), &["task".to_string(), "followup".to_string()])
                .await?
                .expect("the preparation run is claimed");
            assert_eq!(claim.run_id, run_id);
            let lease = claim.lease_id;
            let job = self.coordinator.build_job(claim, "token".into()).await?;
            return Ok((Run { run: run_id, lease: Some(lease), ..rig_copy(source) }, job.prompt));
        }
        let changed = sqlx::query("UPDATE session_runs SET status = 'running', claimed_by = 'contract', started_at = NOW() WHERE id = $1 AND status = 'queued'")
            .bind(run_id)
            .execute(self.ctx.db.pool())
            .await?
            .rows_affected();
        assert_eq!(changed, 1, "the local preparation run is queued exactly once");
        let mut record = self.ctx.store.get_run(run_id).await?.expect("preparation run");
        let mut session = self.session(source).await?;
        super::prepare_request(&self.ctx, &mut record, &mut session).await?;
        super::super::quests::append_single_prework_policy(&self.ctx.store, session.id, record.id, &mut record.prompt).await?;
        Ok((Run { run: run_id, lease: None, ..rig_copy(source) }, record.prompt))
    }

    async fn complete_prework_run(&self, run: &Run, output: &str, remote: bool, secrets: &Redactor) -> anyhow::Result<()> {
        self.complete_prework_run_with_work(run, output, remote, secrets, None).await
    }

    async fn complete_prework_run_with_work(
        &self,
        run: &Run,
        output: &str,
        remote: bool,
        secrets: &Redactor,
        work_summary: Option<&str>,
    ) -> anyhow::Result<()> {
        let (events, result) = with_output(output);
        if remote {
            self.show(run, events, secrets).await?;
            let session_id = result.session_id.clone();
            let outcome = ExecutionOutcome {
                result,
                pr_url: None,
                pr_number: None,
                publish_pr_state: None,
                checked_pr_url: None,
                publish_pr_checked_at: None,
                work_summary: work_summary.map(str::to_string),
                context_sent: None,
                published_head: None,
            };
            self.deliver(run, success_completion(outcome, session_id, run.workspace.clone(), Vec::new())).await
        } else {
            self.locally(run, events, result, secrets).await
        }
    }
}

#[tokio::test]
async fn queued_followups_inherit_unfinished_prework_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let secrets = Redactor::default();
    let clarification = "<!-- clarify\n{\"text\":\"Which account?\",\"choices\":[\"Team\"]}\nclarify -->";
    let plan = "<!-- plan {\"summary\":\"Use team\",\"steps\":[],\"checks\":[],\"risks\":[]} plan -->";
    for (remote, path) in [(false, "local"), (true, "runner")] {
        for (mode, automatic, first_output) in [
            ("ask_when_unclear", false, clarification),
            ("always_show_plan", true, plan),
        ] {
            let first = rig.run_of("task", "claude", "sonnet", remote).await?;
            let task = rig.on_quest(&first, &format!("{path} {mode} preparation")).await?;
            sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2 WHERE id = $1")
                .bind(task)
                .bind(first.agent.id)
                .execute(rig.ctx.db.pool())
                .await?;
            rig.ctx
                .store
                .set_session_asked_by(
                    first.session,
                    json!({
                        "before_big_work": mode,
                        "before_big_work_phase": "pending",
                        "before_big_work_automatic": automatic,
                    }),
                )
                .await?;
            sqlx::query("UPDATE agent_sessions SET review = FALSE WHERE id = $1")
                .bind(first.session)
                .execute(rig.ctx.db.pool())
                .await?;
            let queued = rig
                .ctx
                .store
                .start_followup(&rig.session(&first).await?, "Keep the public API stable", "Ana", false)
                .await?;

            rig.complete_prework_run(&first, first_output, remote, &secrets).await?;
            let phase = loka_shared::quests::single_prework_phase(rig.session(&first).await?.asked_by.as_ref()).map(str::to_string);
            assert_eq!(phase.as_deref(), Some("pending"), "{path} {mode}: stale output cannot finish preparation");
            let stale_cards: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_messages WHERE run_id = $1 AND (meta ? 'clarification' OR meta ? 'plan')",
            )
            .bind(first.run)
            .fetch_one(rig.ctx.db.pool())
            .await?;
            assert_eq!(stale_cards, 0, "{path} {mode}: stale output cannot ask or plan");

            let (effective, prompt) = rig.take_prework_run(&first, queued.id, remote).await?;
            let policy = if automatic { "[before big work: always show a plan]" } else { "[before big work: ask when unclear]" };
            assert!(prompt.contains(policy), "{path} {mode}: {prompt}");
            if automatic {
                rig.complete_prework_run(&effective, REPORT, remote, &secrets).await?;
                assert_eq!(
                    loka_shared::quests::single_prework_phase(rig.session(&first).await?.asked_by.as_ref()),
                    Some("planning"),
                    "{path}: reporting done cannot bypass a mandatory plan"
                );
                let plan_run: Uuid = sqlx::query_scalar(
                    "SELECT id FROM session_runs WHERE session_id = $1 AND status = 'queued' AND kind = 'followup' ORDER BY created_at DESC, id DESC LIMIT 1",
                )
                .bind(first.session)
                .fetch_one(rig.ctx.db.pool())
                .await?;
                let (planning, planning_prompt) = rig.take_prework_run(&first, plan_run, remote).await?;
                assert!(planning_prompt.contains(loka_shared::quests::PLAN_FIRST), "{path}: {planning_prompt}");
                rig.complete_prework_run(&planning, plan, remote, &secrets).await?;
                let card: Uuid = sqlx::query_scalar("SELECT id FROM agent_messages WHERE run_id = $1 AND meta ? 'plan'")
                    .bind(plan_run)
                    .fetch_one(rig.ctx.db.pool())
                    .await?;
                let loka_shared::quests::PlanAnswer::Approved { run_id, .. } = rig.ctx.store.answer_plan(first.agent.id, card, "Approve", "Ana").await?
                else {
                    panic!("{path}: the effective plan is approved")
                };
                assert_eq!(rig.ctx.store.answer_plan(first.agent.id, card, "Approve", "Ana").await?, loka_shared::quests::PlanAnswer::Stale);
                let (implementation, implementation_prompt) = rig.take_prework_run(&first, run_id, remote).await?;
                assert!(implementation_prompt.contains("[plan approved by Ana]"), "{path}: {implementation_prompt}");
                rig.complete_prework_run(&implementation, REPORT, remote, &secrets).await?;
                let approvals: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM world_events WHERE task_id = $1 AND type = 'task.plan_approved'",
                )
                .bind(task)
                .fetch_one(rig.ctx.db.pool())
                .await?;
                assert_eq!(approvals, 1, "{path}: plan approval advances once");
            } else {
                rig.complete_prework_run(&effective, clarification, remote, &secrets).await?;
                let card: Uuid = sqlx::query_scalar("SELECT id FROM agent_messages WHERE run_id = $1 AND meta ? 'clarification'")
                    .bind(effective.run)
                    .fetch_one(rig.ctx.db.pool())
                    .await?;
                let owner = first.agent.owner_user_id.expect("owner");
                let reply = || loka_shared::quests::ClarificationReply {
                    user_id: owner,
                    author: "Ana",
                    text: "Team",
                    meta: json!({ "answers": card, "task_id": task }),
                    reply_to: None,
                    event_payload: json!({ "author": "Ana", "author_kind": "user", "body": "Team", "answer_card": card }),
                };
                let (answer, message) = rig.ctx.store.answer_clarification(first.agent.id, card, reply()).await?;
                let loka_shared::quests::ClarificationAnswer::Answered { run_id, .. } = answer else {
                    panic!("{path}: the effective question is answered")
                };
                assert!(message.is_some());
                assert_eq!(
                    rig.ctx.store.answer_clarification(first.agent.id, card, reply()).await?.0,
                    loka_shared::quests::ClarificationAnswer::Stale
                );
                let (implementation, implementation_prompt) = rig.take_prework_run(&first, run_id, remote).await?;
                assert!(implementation_prompt.contains("[questions answered by Ana]"), "{path}: {implementation_prompt}");
                rig.complete_prework_run(&implementation, REPORT, remote, &secrets).await?;
                let cards: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM agent_messages WHERE agent_id = $1 AND meta ? 'clarification' AND meta->'clarification'->>'task_id' = $2",
                )
                .bind(first.agent.id)
                .bind(task.to_string())
                .fetch_one(rig.ctx.db.pool())
                .await?;
                assert_eq!(cards, 1, "{path}: the effective run asks once");
            }
            assert_eq!(loka_shared::quests::single_prework_phase(rig.session(&first).await?.asked_by.as_ref()), None, "{path} {mode}");
            let status: String = sqlx::query_scalar("SELECT status FROM agent_tasks WHERE id = $1")
                .bind(task)
                .fetch_one(rig.ctx.db.pool())
                .await?;
            assert_eq!(status, "done", "{path} {mode}: normal continuation completes after preparation");
        }
    }
    Ok(())
}

#[tokio::test]
async fn exhausted_prework_fails_without_reviewing_or_publishing_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let secrets = Redactor::default();
    let invalid = "I inspected the request, but did not return the required plan.";
    for (remote, path) in [(false, "local"), (true, "runner")] {
        let first = rig.run_of("task", "claude", "sonnet", remote).await?;
        let task = rig.on_quest(&first, &format!("{path} exhausted preparation")).await?;
        sqlx::query("UPDATE agent_tasks SET assigned_agent_id = $2, repo = $3 WHERE id = $1")
            .bind(task)
            .bind(first.agent.id)
            .bind(remote.then_some("acme/app"))
            .execute(rig.ctx.db.pool())
            .await?;
        rig.ctx
            .store
            .set_session_asked_by(
                first.session,
                json!({
                    "before_big_work": "always_show_plan",
                    "before_big_work_phase": "pending",
                    "before_big_work_automatic": true,
                }),
            )
            .await?;
        sqlx::query("UPDATE agent_sessions SET review = TRUE WHERE id = $1")
            .bind(first.session)
            .execute(rig.ctx.db.pool())
            .await?;

        rig.complete_prework_run(&first, REPORT, remote, &secrets).await?;
        assert_eq!(
            loka_shared::quests::single_prework_phase(rig.session(&first).await?.asked_by.as_ref()),
            Some("planning"),
            "{path}: a done report still requires the plan"
        );
        let mut exhausted_run = None;
        for attempt in 0..3 {
            let run_id: Uuid = sqlx::query_scalar(
                "SELECT id FROM session_runs WHERE session_id = $1 AND status = 'queued' AND kind = 'followup' ORDER BY created_at, id LIMIT 1",
            )
            .bind(first.session)
            .fetch_one(rig.ctx.db.pool())
            .await?;
            let (run, prompt) = rig.take_prework_run(&first, run_id, remote).await?;
            assert!(prompt.contains(loka_shared::quests::PLAN_FIRST), "{path} retry {attempt}: {prompt}");
            let work = (remote && attempt == 2).then_some("Changed src/lib.rs before preparation completed.");
            rig.complete_prework_run_with_work(&run, invalid, remote, &secrets, work).await?;
            if attempt == 2 {
                exhausted_run = Some(run_id);
            }
            let reminders: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM session_runs WHERE session_id = $1 AND nudge = 'report'",
            )
            .bind(first.session)
            .fetch_one(rig.ctx.db.pool())
            .await?;
            assert_eq!(reminders, i64::from((attempt + 1).min(2)), "{path} retry {attempt}");
        }

        let (status, error): (String, Option<String>) = sqlx::query_as("SELECT status, error_message FROM agent_tasks WHERE id = $1")
            .bind(task)
            .fetch_one(rig.ctx.db.pool())
            .await?;
        assert_eq!(status, "failed", "{path}: exhausted preparation cannot count as completed: {error:?}");
        assert!(error.is_some(), "{path}: exhausted preparation explains the failure");
        let exhausted_status: String = sqlx::query_scalar("SELECT status FROM session_runs WHERE id = $1")
            .bind(exhausted_run.expect("the final preparation run"))
            .fetch_one(rig.ctx.db.pool())
            .await?;
        assert_eq!(exhausted_status, "failed", "{path}: the terminal preparation run records the failure");
        let terminal: Vec<(String, Value)> = sqlx::query_as(
            "SELECT type, payload FROM world_events WHERE run_id = $1 AND type IN ('run.completed', 'run.failed', 'task.finished') ORDER BY seq",
        )
        .bind(exhausted_run.expect("the final preparation run"))
        .fetch_all(rig.ctx.db.pool())
        .await?;
        assert_eq!(terminal.iter().map(|event| event.0.as_str()).collect::<Vec<_>>(), ["run.failed", "task.finished"], "{path}");
        assert_eq!(terminal[0].1.get("error").and_then(Value::as_str), error.as_deref(), "{path}: the run failure is visible");
        assert_eq!(terminal[1].1.get("error").and_then(Value::as_str), error.as_deref(), "{path}: the quest failure matches it");
        let closed = rig.session(&first).await?;
        assert_eq!(closed.status, "archived", "{path}: the exhausted preparation session is closed");
        assert_eq!(loka_shared::quests::single_prework_phase(closed.asked_by.as_ref()), Some("planning"), "{path}");
        let escaped: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE task_id = $1 AND kind IN ('review', 'publish')",
        )
        .bind(task)
        .fetch_one(rig.ctx.db.pool())
        .await?;
        assert_eq!(escaped, 0, "{path}: unapproved work is neither reviewed nor published");
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_runs WHERE session_id = $1 AND kind IN ('task', 'followup') AND status IN ('queued', 'running')",
        )
        .bind(first.session)
        .fetch_one(rig.ctx.db.pool())
        .await?;
        assert_eq!(active, 0, "{path}: exhausted preparation leaves no retry loop");
    }
    Ok(())
}

#[tokio::test]
async fn the_context_a_run_delivered_is_saved_only_when_the_run_completes_live_on_both_workers() -> anyhow::Result<()> {
    let Some(rig) = rig().await? else {
        return Ok(());
    };
    let redactor = Redactor::default();
    let mut sent = SentContext::default();
    sent.offer(SKILLS_BLOCK, "[skills]\n- notes.md");
    sent.offer(INSTRUCTIONS_BLOCK, "[rules updated]\nSign every reply.");
    let stored = |session: AgentSession| SentContext::from_value(session.context_sent.as_ref().unwrap_or(&json!({})));

    let local = rig.run("claude", "sonnet", false).await?;
    let (events, result) = scripted();
    rig.locally_sending(&local, events, result, Some(sent.clone()), &redactor).await?;
    assert_eq!(stored(rig.session(&local).await?), sent, "local: a completed run records what it delivered");

    let remote = rig.run("claude", "sonnet", true).await?;
    let (events, result) = scripted();
    rig.show(&remote, events, &redactor).await?;
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: Some(sent.clone()), published_head: None };
    rig.deliver(&remote, success_completion(outcome, Some("cs-1".into()), remote.workspace.clone(), Vec::new())).await?;
    assert_eq!(stored(rig.session(&remote).await?).hash(SKILLS_BLOCK), sent.hash(SKILLS_BLOCK), "remote: a completed run records what it delivered");

    let closed = rig.run("claude", "sonnet", false).await?;
    sqlx::query("UPDATE session_runs SET status = 'failed', error = 'Stopped', finished_at = NOW() WHERE id = $1")
        .bind(closed.run)
        .execute(rig.ctx.db.pool())
        .await?;
    let (events, result) = scripted();
    rig.locally_sending(&closed, without_session_start(events), result, Some(sent.clone()), &redactor).await?;
    assert_eq!(stored(rig.session(&closed).await?), SentContext::default(), "local: a closed run leaves the session's record alone");

    let stopped = rig.run("claude", "sonnet", true).await?;
    assert!(rig.coordinator.stop_run(stopped.run, "Tara").await?);
    let (_, result) = scripted();
    let outcome = ExecutionOutcome { result, pr_url: None, pr_number: None, publish_pr_state: None, checked_pr_url: None, publish_pr_checked_at: None, work_summary: None, context_sent: Some(sent.clone()), published_head: None };
    assert!(rig.deliver(&stopped, success_completion(outcome, Some("cs-1".into()), stopped.workspace.clone(), Vec::new())).await.is_err());
    assert_eq!(stored(rig.session(&stopped).await?), SentContext::default(), "remote: a stopped run leaves the session's record alone");
    Ok(())
}
