use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{info, warn};
use uuid::Uuid;

use crate::claude::{self, TempMcpConfig, TurnReader};
use crate::cli::{stderr_tail, CliOptions, CliResult, JobUid, Timing};
use crate::codex::app_server::AppServer;

pub struct Pool {
    capacity: usize,
    idle: Duration,
    procs: Mutex<HashMap<Uuid, Warm>>,
}

struct Warm {
    key: Fingerprint,
    last_run: Uuid,
    last_used: Instant,
    turns: u32,
    uid: Option<JobUid>,
    proc: Proc,
}

impl Warm {
    async fn shutdown(self) {
        let Warm { proc, uid, .. } = self;
        proc.shutdown().await;
        drop(uid);
    }
}

enum Proc {
    Claude(ClaudeProc),
    Codex(AppServer),
    Omp(crate::omp::WarmOmp),
    Acp(crate::acp::AcpProc, &'static str),
}

struct ClaudeProc {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    replays: bool,
    expect: Option<&'static str>,
    _mcp: Option<TempMcpConfig>,
}

pub fn supports(backend: &str) -> bool {
    matches!(backend, "claude" | "codex" | "omp" | "cursor" | "opencode")
}

pub fn reports_usage(backend: &str) -> bool {
    backend != "cursor"
}

pub fn supports_model(backend: &str, model: &str) -> bool {
    supports(backend) && (!matches!(backend, "cursor" | "opencode") || crate::acp::offers(model))
}

impl Pool {
    pub fn new(capacity: usize) -> Arc<Self> {
        let idle = std::env::var("WORLD_WARM_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(1800);
        Arc::new(Self {
            capacity,
            idle: Duration::from_secs(idle),
            procs: Mutex::new(HashMap::new()),
        })
    }

    pub fn enabled(&self) -> bool {
        self.capacity > 0
    }

    pub async fn evict(&self, session_id: Uuid) {
        let taken = self.procs.lock().unwrap().remove(&session_id);
        if let Some(warm) = taken {
            warm.shutdown().await;
        }
    }

    pub fn clear(&self) {
        let all: Vec<Warm> = self.procs.lock().unwrap().drain().map(|(_, w)| w).collect();
        for warm in all {
            tokio::spawn(warm.shutdown());
        }
    }

    pub async fn reap_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let stale: Vec<Warm> = {
                let mut procs = self.procs.lock().unwrap();
                let ids: Vec<Uuid> = procs
                    .iter_mut()
                    .filter_map(|(id, w)| (w.last_used.elapsed() > self.idle || !w.proc.alive()).then_some(*id))
                    .collect();
                ids.into_iter().filter_map(|id| procs.remove(&id)).collect()
            };
            for warm in stale {
                info!(turns = warm.turns, "Retiring idle warm session");
                warm.shutdown().await;
            }
        }
    }

    pub async fn turn(
        &self,
        backend: &str,
        session_id: Uuid,
        latest_run: Option<Uuid>,
        run_id: Uuid,
        opts: &CliOptions<'_>,
    ) -> Result<(CliResult, Timing)> {
        crate::cli::check_model(opts.model)?;
        let started = Instant::now();
        let key = fingerprint(backend, opts);
        let taken = self.procs.lock().unwrap().remove(&session_id);
        let reusable = match taken {
            Some(mut warm) => {
                let alive = warm.proc.alive();
                if alive && warm.key == key && Some(warm.last_run) == latest_run {
                    Some(warm)
                } else {
                    let changed = changed(&warm.key, &key);
                    let behind = Some(warm.last_run) != latest_run;
                    info!(session = %session_id, alive, same_key = changed.is_empty(), %changed, behind, "Replacing warm session");
                    loka_shared::telemetry::count("warm_replaced_total", &[("backend", backend), ("alive", if alive { "yes" } else { "no" }), ("changed", if changed.is_empty() { "none" } else { changed.as_str() }), ("behind", if behind { "yes" } else { "no" })], 1.0);
                    if matches!(warm.proc, Proc::Acp(..)) {
                        warm.shutdown().await;
                    } else {
                        tokio::spawn(warm.shutdown());
                    }
                    None
                }
            }
            None => None,
        };
        let spawned = reusable.is_none();
        let mut warm = match reusable {
            Some(warm) => {
                crate::cli::note_spawn(opts.event_tx.as_ref(), warm.proc.pid());
                warm
            }
            None => Warm {
                key,
                last_run: run_id,
                last_used: Instant::now(),
                turns: 0,
                uid: JobUid::current(),
                proc: Proc::spawn(backend, opts).await.inspect_err(|e| {
                    warn!(backend, error = %crate::remote_runner::redact(&format!("{e:#}"), &opts.secret_values()), "Could not start warm session")
                })?,
            },
        };
        let spawn_ms = started.elapsed().as_millis() as u64;

        match warm.proc.ask(opts, started).await {
            Ok((result, mut timing)) => {
                timing.warm = true;
                timing.spawned = spawned;
                timing.spawn_ms = spawn_ms;
                warm.last_run = run_id;
                warm.last_used = Instant::now();
                warm.turns += 1;
                self.put(session_id, warm);
                Ok((result, timing))
            }
            Err(e) => {
                let tail = warm.proc.stderr_tail();
                warm.shutdown().await;
                warn!(backend, stderr = %crate::remote_runner::redact(&tail, &opts.secret_values()), "Warm session turn failed");
                if tail.is_empty() || opts.sensitive || opts.mcp_has_secrets {
                    Err(e)
                } else {
                    Err(anyhow!("{e:#}. CLI output: {tail}"))
                }
            }
        }
    }

    fn put(&self, session_id: Uuid, warm: Warm) {
        let evicted = {
            let mut procs = self.procs.lock().unwrap();
            procs.insert(session_id, warm);
            let mut evicted = Vec::new();
            while procs.len() > self.capacity {
                let Some(oldest) = procs.iter().min_by_key(|(_, w)| w.last_used).map(|(id, _)| *id) else {
                    break;
                };
                evicted.extend(procs.remove(&oldest));
            }
            evicted
        };
        for warm in evicted {
            info!(turns = warm.turns, "Evicting least recently used warm session");
            tokio::spawn(warm.shutdown());
        }
    }
}

impl Proc {
    async fn spawn(backend: &str, opts: &CliOptions<'_>) -> Result<Self> {
        match backend {
            "codex" => Ok(Self::Codex(AppServer::spawn(opts).await?)),
            "omp" => Ok(Self::Omp(crate::omp::WarmOmp::spawn(opts).await?)),
            "cursor" => Ok(Self::Acp(crate::acp::AcpProc::spawn(crate::cursor::acp_setup(opts).await?, opts).await?, "cursor")),
            "opencode" => Ok(Self::Acp(crate::acp::AcpProc::spawn(crate::opencode::acp_setup(opts).await?, opts).await?, "opencode")),
            _ => Ok(Self::Claude(ClaudeProc::spawn(opts).await?)),
        }
    }

    fn pid(&self) -> Option<u32> {
        match self {
            Self::Claude(p) => p.child.id(),
            Self::Codex(p) => p.pid(),
            Self::Omp(p) => p.pid(),
            Self::Acp(p, _) => p.pid(),
        }
    }

    fn alive(&mut self) -> bool {
        match self {
            Self::Claude(p) => matches!(p.child.try_wait(), Ok(None)),
            Self::Codex(p) => p.alive(),
            Self::Omp(p) => p.alive(),
            Self::Acp(p, _) => p.alive(),
        }
    }

    fn stderr_tail(&self) -> String {
        match self {
            Self::Claude(p) => p.stderr.lock().unwrap().trim().to_string(),
            Self::Codex(p) => p.stderr_tail(),
            Self::Omp(p) => p.stderr_tail(),
            Self::Acp(p, _) => p.stderr_tail(),
        }
    }

    async fn ask(&mut self, opts: &CliOptions<'_>, started: Instant) -> Result<(CliResult, Timing)> {
        match self {
            Self::Claude(p) => p.ask(opts, started).await,
            Self::Codex(p) => p.ask(opts, started).await,
            Self::Omp(p) => p.ask(opts, started).await,
            Self::Acp(p, backend) => {
                let (mut result, timing) = p.ask(opts, started).await?;
                if *backend == "cursor" {
                    result.checkpoint = crate::cursor::after_acp_turn(p.session(), opts.working_dir).await;
                }
                Ok((result, timing))
            }
        }
    }

    async fn shutdown(self) {
        match self {
            Self::Claude(p) => p.shutdown().await,
            Self::Codex(p) => p.shutdown().await,
            Self::Omp(p) => p.shutdown().await,
            Self::Acp(p, _) => p.shutdown().await,
        }
    }
}

impl ClaudeProc {
    async fn spawn(opts: &CliOptions<'_>) -> Result<Self> {
        let launch = claude::detect::prepare(opts).await?;
        let mcp = match opts.mcp_config_json.as_deref() {
            Some(json) => Some(claude::write_mcp_config_temp_file(json).await?),
            None => None,
        };
        let mut cmd = claude::command(opts, mcp.as_ref().map(|m| m.as_ref()), &launch)?;
        let expect = claude::expected_mode(opts, &launch);
        cmd.arg("--input-format").arg("stream-json");
        let replays = opts.steer.is_set();
        if replays {
            cmd.arg("--replay-user-messages");
        }
        info!(dir = %opts.working_dir.display(), model = opts.model, replays, "Starting warm Claude Code session");
        let mut child = claude::spawn(&mut cmd)?;
        crate::cli::announce_spawn(opts.event_tx.as_ref(), &child);
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("claude stdin missing"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("claude stdout missing"))?).lines();
        let stderr = stderr_tail(child.stderr.take());
        Ok(Self { child, stdin, stdout, stderr, replays, expect, _mcp: mcp })
    }

    async fn ask(&mut self, opts: &CliOptions<'_>, started: Instant) -> Result<(CliResult, Timing)> {
        let line = claude::user_line(opts.prompt, &opts.image_paths).await?;
        self.stdin.write_all(line.as_bytes()).await.context("warm session stdin closed")?;
        self.stdin.flush().await?;
        let mut reader = TurnReader::new(started).expecting(self.expect);
        let mut steer = if self.replays { opts.steer.take() } else { None };
        let steering = steer.is_some();
        let mut state = claude::SteerState::new();
        let mut outputs: Vec<String> = Vec::new();
        let interrupt_after = crate::cli::steer_interrupt_after();
        let hang_after = crate::cli::tool_hang_after();
        let mut watchdog = tokio::time::interval(Duration::from_secs(15));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let finished: Result<()> = loop {
            tokio::select! {
                line = self.stdout.next_line() => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) => break Err(anyhow!("warm Claude Code session exited mid-turn")),
                        Err(error) => break Err(error.into()),
                    };
                    let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    state.track(&json);
                    let done = reader.feed(&json, opts.event_tx.as_ref());
                    if let Some(error) = claude::refusal(&reader, opts) {
                        break Err(error);
                    }
                    if !done {
                        continue;
                    }
                    if reader.is_error_response || !steering {
                        break Ok(());
                    }
                    if !reader.result_text.trim().is_empty() {
                        outputs.push(reader.result_text.clone());
                    }
                    if let Some(rx) = steer.as_mut() {
                        while let Ok(next) = rx.try_recv() {
                            let written = state.write(&mut self.stdin, &next.text, false).await;
                            let ok = written.is_ok();
                            let _ = next.ack.send(ok);
                            if !ok {
                                break;
                            }
                        }
                    }
                    if !state.pending() {
                        break Ok(());
                    }
                }
                Some(next) = async {
                    match steer.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let interrupt = next.interrupt || state.stuck_longer_than(interrupt_after);
                    let written = state.write(&mut self.stdin, &next.text, interrupt).await;
                    let _ = next.ack.send(written.is_ok());
                    if let Err(error) = written {
                        break Err(error);
                    }
                    info!(interrupt, "Steered warm Claude session");
                }
                _ = watchdog.tick(), if steering => {
                    if let (Some(limit), Some(age)) = (hang_after, state.oldest_tool()) {
                        if age >= limit {
                            warn!(secs = age.as_secs(), "Claude tool call looks stuck, interrupting");
                            if let Err(error) = state.write(&mut self.stdin, &crate::cli::hang_notice(age), true).await {
                                break Err(error);
                            }
                        }
                    }
                }
            }
        };
        if let Some(rx) = steer.as_mut() {
            crate::cli::close_steers(rx);
        }
        finished?;
        if reader.is_error_response {
            claude::note_error(&reader, opts);
            return Err(anyhow!("Claude Code returned error: {}", reader.result_text));
        }
        if steering && !outputs.is_empty() {
            reader.result_text = outputs.join("\n\n");
        }
        let timing = reader.timing;
        Ok((reader.into_result(0), timing))
    }

    async fn shutdown(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_err() {
            if let Err(e) = self.child.kill().await {
                warn!(error = %e, "Could not kill warm Claude Code session");
            }
        }
    }
}

type Fingerprint = [(&'static str, u64); 14];

fn part(value: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

fn fingerprint(backend: &str, opts: &CliOptions<'_>) -> Fingerprint {
    [
        ("backend", part(backend)),
        ("working_dir", part(opts.working_dir)),
        ("system_prompt", part(opts.system_prompt)),
        ("max_turns", part(opts.max_turns)),
        ("model", part(opts.model)),
        ("mcp_config", part(&opts.mcp_config_json)),
        ("effort", part(opts.effort)),
        ("fast", part(opts.fast)),
        ("environment", part(&opts.environment)),
        ("provider_keys", part(&opts.provider_keys)),
        ("omp_models", part(&opts.omp_models)),
        ("readable_files", part(&opts.readable_files)),
        ("steer", part(opts.steer.is_set())),
        ("approval", part(format!("{:?}", opts.approval))),
    ]
}

fn changed(before: &Fingerprint, after: &Fingerprint) -> String {
    before.iter().zip(after).filter(|(old, new)| old.1 != new.1).map(|(old, _)| old.0).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::Pool;
    use crate::cli::CliOptions;
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    fn options<'a>(dir: &'a std::path::Path, prompt: &'a str, model: &'a str, sid: Option<&'a str>, resume: bool) -> CliOptions<'a> {
        CliOptions {
            working_dir: dir,
            prompt,
            system_prompt: Some("Answer in as few words as possible."),
            allowed_tools: None,
            max_turns: 2,
            model,
            mcp_config_json: None,
            session_id: sid,
            resume,
            event_tx: None,
            image_paths: Vec::new(),
            fork: false,
            lean: true,
            effort: Some("low"),
            fast: false,
            environment: Default::default(),
            mcp_has_secrets: false,
            sensitive: false,
            rewind_to: None,
            approval: Default::default(),
            steer: Default::default(),
            omp_models: None,
            provider_keys: Default::default(),
            readable_files: Vec::new(),
        }
    }

    fn pool() -> Arc<Pool> {
        Arc::new(Pool { capacity: 1, idle: Duration::from_secs(60), procs: Default::default() })
    }

    #[tokio::test]
    #[ignore]
    async fn keeps_session_warm_between_turns() {
        let pool = pool();
        let dir = std::env::temp_dir();
        let session = Uuid::new_v4();
        let sid = Uuid::new_v4().to_string();
        let first_run = Uuid::new_v4();
        let (first, t1) = pool
            .turn("claude", session, None, first_run, &options(&dir, "Remember the word marmalade. Reply ok.", "haiku", Some(&sid), false))
            .await
            .unwrap();
        assert!(t1.spawned);
        assert_eq!(first.session_id.as_deref(), Some(sid.as_str()));
        let second_run = Uuid::new_v4();
        let (second, t2) = pool
            .turn("claude", session, Some(first_run), second_run, &options(&dir, "What word did I ask you to remember?", "haiku", Some(&sid), true))
            .await
            .unwrap();
        assert!(!t2.spawned);
        assert!(second.output.to_lowercase().contains("marmalade"), "{}", second.output);
        assert!(second.cost_usd >= first.cost_usd);
        let (_, t3) = pool
            .turn("claude", session, Some(Uuid::new_v4()), Uuid::new_v4(), &options(&dir, "Reply ok.", "haiku", Some(&sid), true))
            .await
            .unwrap();
        assert!(t3.spawned);
        eprintln!("cold {t1:?}\nwarm {t2:?}\nstale {t3:?}");
        pool.clear();
    }

    #[tokio::test]
    #[ignore]
    async fn keeps_codex_thread_warm_between_turns() {
        let model = std::env::var("CODEX_TEST_MODEL").expect("set CODEX_TEST_MODEL to a model your codex login accepts");
        let pool = pool();
        let dir = std::env::temp_dir();
        let session = Uuid::new_v4();
        let first_run = Uuid::new_v4();
        let (first, t1) = pool
            .turn("codex", session, None, first_run, &options(&dir, "Remember the word heliotrope. Reply ok.", &model, None, false))
            .await
            .unwrap();
        assert!(t1.spawned);
        let thread = first.session_id.clone().expect("codex thread id");
        let second_run = Uuid::new_v4();
        let (second, t2) = pool
            .turn("codex", session, Some(first_run), second_run, &options(&dir, "What word did I ask you to remember?", &model, Some(&thread), true))
            .await
            .unwrap();
        assert!(!t2.spawned);
        assert_eq!(second.session_id.as_deref(), Some(thread.as_str()));
        assert!(second.output.to_lowercase().contains("heliotrope"), "{}", second.output);
        assert!(second.usage.input_tokens + second.usage.cached_input_tokens > first.usage.input_tokens + first.usage.cached_input_tokens);
        let (third, t3) = pool
            .turn("codex", session, Some(Uuid::new_v4()), Uuid::new_v4(), &options(&dir, "Say the word again.", &model, Some(&thread), true))
            .await
            .unwrap();
        assert!(t3.spawned);
        assert!(third.output.to_lowercase().contains("heliotrope"), "{}", third.output);
        eprintln!("cold {t1:?}\nwarm {t2:?}\nresumed {t3:?}");
        pool.clear();
    }

    #[tokio::test]
    #[ignore]
    async fn keeps_omp_session_warm_between_turns() {
        let model = std::env::var("OMP_TEST_MODEL").expect("set OMP_TEST_MODEL to a model omp can reach");
        let pool = pool();
        let dir = std::env::temp_dir();
        let session = Uuid::new_v4();
        let first_run = Uuid::new_v4();
        let (first, t1) = pool
            .turn("omp", session, None, first_run, &options(&dir, "Remember the word marmalade. Reply ok.", &model, None, false))
            .await
            .unwrap();
        assert!(t1.spawned);
        let sid = first.session_id.clone().expect("omp session id");
        assert!(first.usage.output_tokens > 0);
        let second_run = Uuid::new_v4();
        let (second, t2) = pool
            .turn("omp", session, Some(first_run), second_run, &options(&dir, "What word did I ask you to remember?", &model, Some(&sid), true))
            .await
            .unwrap();
        assert!(!t2.spawned);
        assert_eq!(second.session_id.as_deref(), Some(sid.as_str()));
        assert!(second.output.to_lowercase().contains("marmalade"), "{}", second.output);
        assert!(second.usage.output_tokens > 0);
        assert!(second.usage.input_tokens + second.usage.cached_input_tokens > 0);
        let third_run = Uuid::new_v4();
        let (third, t3) = pool
            .turn("omp", session, Some(Uuid::new_v4()), third_run, &options(&dir, "Say the word again.", &model, Some(&sid), true))
            .await
            .unwrap();
        assert!(t3.spawned);
        assert_eq!(third.session_id.as_deref(), Some(sid.as_str()));
        assert!(third.output.to_lowercase().contains("marmalade"), "{}", third.output);
        if let Ok(image) = std::env::var("OMP_TEST_IMAGE") {
            let mut opts = options(&dir, "What colour fills this image? One word.", &model, Some(&sid), true);
            opts.image_paths = vec![image.into()];
            let (seen, t4) = pool.turn("omp", session, Some(third_run), Uuid::new_v4(), &opts).await.unwrap();
            assert!(!t4.spawned);
            assert!(seen.output.to_lowercase().contains("red"), "{}", seen.output);
        }
        eprintln!("cold {t1:?}\nwarm {t2:?}\nresumed {t3:?}\nusage {:?} {:?}", first.usage, second.usage);
        pool.clear();
    }
}
