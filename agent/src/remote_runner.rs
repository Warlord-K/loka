use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use loka_shared::runners::{
    LeaseBeat, LeaseSteers, RunnerCompletion, RunnerEvent, RunnerHeartbeat, RunnerJob, RunnerPoll, RunnerPolled,
    RunnerPublish, RunnerUsage, RunnerWorkspace, RUNNER_LEASE_SECONDS,
};
use loka_shared::git_names::{validate_git_ref, validate_repo};
use loka_shared::shell::{ShellFinish, ShellJob, OUTPUT_CAP};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Notify, Semaphore};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{self, CliOptions, JobUid, StreamEvent};
use crate::world::warm::{self, Pool};

mod github_feed;
mod health;
mod media;
mod outbox;
mod storage;
mod vault_refs;
mod workspaces;

use outbox::{Batch, Deliver, EventSink, Outbox, Sent};
use storage::RunnerStorage;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const UPDATE_DRAIN: Duration = Duration::from_secs(20);
const IDLE_WAIT: Duration = Duration::from_secs(45);
const STREAM_SILENCE: Duration = Duration::from_secs(45);
const WORKSPACE_CLEANUP_INTERVAL: Duration = Duration::from_secs(600);
const FATAL_DRAIN: Duration = Duration::from_secs(30);
const TRANSCRIPT_GONE: &str = "[memory handoff] The saved transcript of your earlier work in this session was cleaned up, so this is a fresh start. Rebuild what you need from the workspace (git status, git log, git diff, any open PR) and the message below, then continue.";
const MAX_EVENT_TEXT: usize = 4_000;
const FOLDERS_FILE: &str = "folders.json";
const SHELL_CANCEL_CHECK: Duration = Duration::from_secs(2);
const CHAT_KINDS: [&str; 4] = ["turn", "aside", "direct", "quick"];
const WORK_KINDS: [&str; 4] = ["task", "followup", "review", "publish"];
const MCP_SOCKET: &str = "leases/mcp.sock";
const MCP_HUB_SOCKET: &str = "leases/mcp-hub.sock";

type SpawnedPids = Arc<Mutex<Vec<(u32, Option<u64>)>>>;

struct McpLease {
    token: String,
    pids: SpawnedPids,
    roots: Vec<PathBuf>,
}

fn same_process(pid: u32, started: Option<u64>) -> bool {
    started.is_some() && cli::process_start(pid) == started
}

fn live_pids(pids: &Mutex<Vec<(u32, Option<u64>)>>) -> Vec<u32> {
    pids.lock().unwrap().iter().filter(|(pid, started)| same_process(*pid, *started)).map(|(pid, _)| *pid).collect()
}

static MCP_LEASES: Mutex<BTreeMap<Uuid, McpLease>> = Mutex::new(BTreeMap::new());

struct OpenMcpLease(Uuid);

impl OpenMcpLease {
    fn new(job: &RunnerJob, pids: SpawnedPids, roots: Vec<PathBuf>) -> Self {
        MCP_LEASES.lock().unwrap().insert(job.lease_id, McpLease { token: job.mcp_token.clone(), pids, roots });
        Self(job.lease_id)
    }
}

impl Drop for OpenMcpLease {
    fn drop(&mut self) {
        MCP_LEASES.lock().unwrap().remove(&self.0);
    }
}

#[derive(Clone)]
pub(crate) struct RunnerApi {
    client: Client,
    stream_client: Client,
    base_url: String,
    runner_id: Uuid,
    token: String,
}

#[derive(Deserialize)]
struct RunnerStatus {
    status: String,
}

#[derive(Deserialize)]
struct ClosedTasks {
    #[serde(default)]
    closed: Vec<Uuid>,
    #[serde(default)]
    entries: Vec<loka_shared::runners::ClosedTask>,
}

#[derive(Debug)]
struct StreamPaused;

impl std::fmt::Display for StreamPaused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("runner is paused")
    }
}

impl std::error::Error for StreamPaused {}

struct Claims {
    poll: bool,
    shell: bool,
    terminal: bool,
}

#[derive(Default)]
struct Stages {
    prepare_ms: Option<u128>,
    run_ms: Option<u128>,
    checkpoint_ms: Option<u128>,
}

enum CompletionState {
    Accepted,
    Stale,
    Blocked(StatusCode),
}

struct ExecutionOutcome {
    result: crate::cli::CliResult,
    pr_url: Option<String>,
    pr_number: Option<i64>,
    publish_pr_state: Option<String>,
    work_summary: Option<String>,
}


static PLAIN_BODIES: AtomicBool = AtomicBool::new(false);

static UPDATE_PAUSE: AtomicBool = AtomicBool::new(false);

fn update_notify() -> &'static Notify {
    static NOTIFY: std::sync::OnceLock<Notify> = std::sync::OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

fn pausing_for_update() -> bool {
    UPDATE_PAUSE.load(Ordering::Acquire)
}

async fn update_requested() {
    let notified = update_notify().notified();
    if pausing_for_update() {
        return;
    }
    notified.await
}

fn pause_for_update() {
    UPDATE_PAUSE.store(true, Ordering::Release);
    update_notify().notify_waiters();
}

fn spawn_busy_file(root: &Path, running: Arc<Mutex<BTreeMap<Uuid, String>>>) {
    let path = root.join("busy");
    tokio::spawn(async move {
        let mut last = usize::MAX;
        loop {
            let busy = running.lock().unwrap().len();
            if busy != last && tokio::fs::write(&path, busy.to_string()).await.is_ok() {
                last = busy;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

fn gzip(raw: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

async fn send_json<T: serde::Serialize + ?Sized>(make: impl Fn() -> reqwest::RequestBuilder, body: &T) -> Result<reqwest::Response> {
    let raw = serde_json::to_vec(body)?;
    let plain = |raw: Vec<u8>| make().header(reqwest::header::CONTENT_TYPE, "application/json").body(raw);
    if PLAIN_BODIES.load(Ordering::Relaxed) {
        return Ok(plain(raw).send().await?);
    }
    let response = make()
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body(gzip(&raw)?)
        .send()
        .await?;
    if !matches!(response.status(), StatusCode::BAD_REQUEST | StatusCode::UNSUPPORTED_MEDIA_TYPE) {
        return Ok(response);
    }
    let retry = plain(raw).send().await?;
    if retry.status().is_success() {
        PLAIN_BODIES.store(true, Ordering::Relaxed);
        warn!("Loka API does not accept gzip request bodies, sending plain JSON");
    }
    Ok(retry)
}

const API_RESTART_GRACE: Duration = Duration::from_secs(60);

fn refused(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| error.is_connect())
}

async fn send_json_through_restart<T: serde::Serialize + ?Sized>(make: impl Fn() -> reqwest::RequestBuilder, body: &T) -> Result<reqwest::Response> {
    let started = Instant::now();
    let mut delay = Duration::from_millis(500);
    loop {
        match send_json(&make, body).await {
            Err(error) if refused(&error) && started.elapsed() < API_RESTART_GRACE => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            outcome => return outcome,
        }
    }
}

impl RunnerApi {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn runner_url(&self, suffix: &str) -> String {
        self.url(&format!("/api/runner/{}{suffix}", self.runner_id))
    }

    fn lease_url(&self, lease_id: Uuid, suffix: &str) -> String {
        self.url(&format!("/api/runner/leases/{lease_id}{suffix}"))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("Authorization", format!("Runner {}", self.token))
    }

    async fn heartbeat_runner(&self, leases: &[Uuid]) -> Result<RunnerStatus> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/heartbeat"))), &json!({ "capabilities": capabilities(), "leases": leases }))
            .await
            .context("runner heartbeat failed")?;
        ensure_success(response.status(), "runner heartbeat")?;
        Ok(response.json().await?)
    }

    async fn closed_tasks(&self, tasks: &[Uuid], pressure: bool) -> Result<Vec<loka_shared::runners::ClosedTask>> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/workspaces"))), &json!({ "tasks": tasks, "pressure": pressure }))
            .await
            .context("runner workspaces request failed")?;
        ensure_success(response.status(), "runner workspaces")?;
        let body = response.json::<ClosedTasks>().await?;
        if !body.entries.is_empty() {
            return Ok(body.entries);
        }
        Ok(body
            .closed
            .into_iter()
            .map(|id| loka_shared::runners::ClosedTask { id, status: "closed".into(), idle_since: None })
            .collect())
    }

    async fn disk_low(&self, notice: &loka_shared::runners::DiskNotice) -> Result<()> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/disk"))), notice)
            .await
            .context("runner disk notice failed")?;
        ensure_success(response.status(), "runner disk notice")
    }

    async fn poll(&self, poll: &RunnerPoll) -> Result<Option<RunnerPolled>> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/poll"))), poll)
            .await
            .context("runner poll failed")?;
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) {
            return Ok(None);
        }
        ensure_success(response.status(), "runner poll")?;
        let polled: RunnerPolled = response.json().await.context("invalid runner poll")?;
        if polled.job.as_ref().is_some_and(|job| job.runner_id != self.runner_id) {
            bail!("claimed job belongs to another runner");
        }
        Ok(Some(polled))
    }

    async fn poll_separately(&self, poll: &RunnerPoll, claims: &mut Claims) -> Result<RunnerPolled> {
        let status = match &poll.heartbeat {
            Some(heartbeat) => self.heartbeat_runner(&heartbeat.leases).await?.status,
            None => "online".to_string(),
        };
        let mut polled = RunnerPolled { status, ..RunnerPolled::default() };
        if polled.status == "paused" {
            return Ok(polled);
        }
        let kinds: Vec<&str> = poll.kinds.iter().map(String::as_str).collect();
        let (shell, terminal, job) = tokio::join!(
            async { if poll.shell { Some(self.shell_claim().await) } else { None } },
            async { if poll.terminal { Some(self.terminal_claim().await) } else { None } },
            async { if kinds.is_empty() { None } else { Some(self.claim(&kinds).await) } },
        );
        match shell {
            Some(Ok(shell)) => polled.shell = shell,
            Some(Err(error)) if format!("{error:#}").contains("unsupported by this API") => {
                info!("Loka API does not support shell commands yet, shell claiming disabled until restart");
                claims.shell = false;
            }
            Some(Err(error)) => warn!(error = %format!("{error:#}"), "Shell claim failed"),
            None => {}
        }
        match terminal {
            Some(Ok(terminal)) => polled.terminal = terminal,
            Some(Err(error)) if format!("{error:#}").contains("unsupported by this API") => {
                info!("Loka API does not support terminals yet, terminal claiming disabled until restart");
                claims.terminal = false;
            }
            Some(Err(error)) => warn!(error = %format!("{error:#}"), "Terminal claim failed"),
            None => {}
        }
        match job {
            Some(Ok(job)) => polled.job = job,
            Some(Err(error)) => {
                warn!(error = %format!("{error:#}"), "Runner claim failed");
                polled.retry = true;
            }
            None => {}
        }
        Ok(polled)
    }

    async fn claim(&self, kinds: &[&str]) -> Result<Option<RunnerJob>> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/claim"))), &json!({ "kinds": kinds }))
            .await
            .context("runner claim failed")?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        ensure_success(response.status(), "runner claim")?;
        let job: RunnerJob = response.json().await.context("invalid runner job")?;
        if job.runner_id != self.runner_id {
            bail!("claimed job belongs to another runner");
        }
        Ok(Some(job))
    }

    async fn listen_for_wakes(&self, wake: &Notify, nudge: &Notify) -> Result<()> {
        let response = self
            .authorized(self.stream_client.get(self.runner_url("/stream")))
            .send()
            .await
            .context("runner stream failed")?;
        if response.status() == StatusCode::CONFLICT {
            return Err(anyhow!(StreamPaused));
        }
        ensure_success(response.status(), "runner stream")?;
        let mut stream = response.bytes_stream();
        let mut buffered = String::new();
        loop {
            let chunk = match tokio::time::timeout(STREAM_SILENCE, stream.next()).await {
                Err(_) => bail!("runner stream went silent"),
                Ok(None) => return Ok(()),
                Ok(Some(chunk)) => chunk?,
            };
            buffered.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(index) = buffered.find('\n') {
                let line = buffered[..index].trim_end_matches('\r').to_string();
                buffered.drain(..=index);
                if line == "event: wake" {
                    wake.notify_one();
                    nudge.notify_waiters();
                }
            }
        }
    }

    async fn heartbeat_lease(&self, lease_id: Uuid, beat: Option<&LeaseBeat>) -> Result<Option<LeaseSteers>> {
        let make = || self.authorized(self.client.post(self.lease_url(lease_id, "/heartbeat")));
        let response = match beat {
            Some(beat) => send_json(make, beat).await?,
            None => make().send().await?,
        };
        match response.status() {
            StatusCode::OK if beat.is_some() => {
                Ok(Some(response.json::<LeaseSteers>().await.unwrap_or_default()))
            }
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(Some(LeaseSteers::default())),
            StatusCode::CONFLICT | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND => Ok(None),
            status => Err(anyhow!("lease heartbeat returned {status}")),
        }
    }

    async fn events(&self, lease_id: Uuid, events: &[RunnerEvent]) -> Result<StatusCode> {
        let response = send_json_through_restart(|| self.authorized(self.client.post(self.lease_url(lease_id, "/events"))), &json!({ "events": events })).await?;
        Ok(response.status())
    }

    async fn complete(
        &self,
        lease_id: Uuid,
        completion: &RunnerCompletion,
    ) -> Result<CompletionState> {
        let response = send_json(|| self.authorized(self.client.post(self.lease_url(lease_id, "/complete"))), completion).await?;
        match response.status() {
            status if status.is_success() => Ok(CompletionState::Accepted),
            StatusCode::CONFLICT | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND => {
                Ok(CompletionState::Stale)
            }
            StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST => Ok(CompletionState::Blocked(response.status())),
            status => Err(anyhow!("runner completion returned {status}")),
        }
    }

    async fn shell_claim(&self) -> Result<Option<ShellJob>> {
        let response = self
            .authorized(self.client.post(self.runner_url("/shell/claim")))
            .send()
            .await
            .context("shell claim failed")?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if response.status() == StatusCode::NOT_FOUND {
            bail!("shell claims unsupported by this API");
        }
        ensure_success(response.status(), "shell claim")?;
        Ok(Some(response.json().await.context("invalid shell job")?))
    }

    async fn shell_output(&self, run_id: Uuid, chunk: &str) -> Result<Option<bool>> {
        let response = send_json(
            || self.authorized(self.client.post(self.runner_url(&format!("/shell/{run_id}/output")))),
            &json!({ "chunk": chunk }),
        )
        .await
        .context("shell output failed")?;
        match response.status() {
            status if status.is_success() => Ok(Some(
                response
                    .json::<Value>()
                    .await?
                    .get("cancel")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )),
            StatusCode::CONFLICT => Ok(None),
            status => Err(anyhow!("shell output returned {status}")),
        }
    }

    async fn shell_finish(&self, run_id: Uuid, finish: &ShellFinish) -> Result<()> {
        let response = send_json(
            || self.authorized(self.client.post(self.runner_url(&format!("/shell/{run_id}/finish")))),
            finish,
        )
        .await
        .context("shell finish failed")?;
        match response.status() {
            status if status.is_success() || status == StatusCode::CONFLICT => Ok(()),
            status => Err(anyhow!("shell finish returned {status}")),
        }
    }

    async fn terminal_claim(&self) -> Result<Option<loka_shared::terminal::TerminalJob>> {
        let response = self
            .authorized(self.client.post(self.runner_url("/terminal/claim")))
            .send()
            .await
            .context("terminal claim failed")?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if response.status() == StatusCode::NOT_FOUND {
            bail!("terminal claims unsupported by this API");
        }
        ensure_success(response.status(), "terminal claim")?;
        Ok(Some(response.json().await.context("invalid terminal session")?))
    }

    pub(crate) fn terminal_socket_url(&self, session: Uuid) -> String {
        self.runner_url(&format!("/terminal/{session}/socket")).replacen("http", "ws", 1)
    }

    pub(crate) fn runner_token(&self) -> &str {
        &self.token
    }
}

impl Deliver for RunnerApi {
    fn deliver<'a>(&'a self, lease_id: Uuid, batch: &'a Batch) -> futures_util::future::BoxFuture<'a, Sent> {
        Box::pin(async move {
            match batch {
                Batch::Events(events) => match self.events(lease_id, events).await {
                    Ok(status) if status.is_success() => Sent::Delivered,
                    Ok(StatusCode::CONFLICT | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND) => Sent::Stale,
                    Ok(status) if status.is_client_error() => Sent::Refused(format!("runner events returned {status}")),
                    Ok(status) => Sent::Retry(anyhow!("runner events returned {status}")),
                    Err(error) => Sent::Retry(error),
                },
                Batch::Completion(completion) => match self.complete(lease_id, completion).await {
                    Ok(CompletionState::Accepted) => Sent::Delivered,
                    Ok(CompletionState::Stale) => Sent::Stale,
                    Ok(CompletionState::Blocked(status)) => Sent::Refused(format!("runner completion returned {status}")),
                    Err(error) => Sent::Retry(error),
                },
            }
        })
    }
}

fn job_metrics(kind: &str, cli: &str, outcome: &str, claim_ms: u128, stages: &Stages, execute_ms: u128, complete_ms: u128) {
    use loka_shared::telemetry;
    telemetry::count("runner_jobs_total", &[("kind", kind), ("cli", cli), ("outcome", outcome)], 1.0);
    let seconds = |ms: u128| ms as f64 / 1000.0;
    let timed = [
        ("claim", Some(claim_ms)),
        ("prepare", stages.prepare_ms),
        ("run", stages.run_ms),
        ("checkpoint", stages.checkpoint_ms),
        ("execute", Some(execute_ms)),
        ("complete", Some(complete_ms)),
    ];
    for (stage, ms) in timed {
        if let Some(ms) = ms {
            telemetry::observe("runner_job_stage_seconds", &[("kind", kind), ("cli", cli), ("stage", stage)], seconds(ms));
        }
    }
}

pub async fn run() -> Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut update = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
        let update_stop = stop_tx.clone();
        tokio::spawn(async move {
            sigterm.recv().await;
            let _ = stop_tx.send(()).await;
        });
        tokio::spawn(async move {
            update.recv().await;
            info!("Pausing running jobs so Loka can restart for an update");
            pause_for_update();
            let _ = update_stop.send(()).await;
        });
    }
    #[cfg(not(unix))]
    drop(stop_tx);
    let base_url = required_env("WORLD_RUNNER_URL")?
        .trim_end_matches('/')
        .to_string();
    let runner_id = required_env("WORLD_RUNNER_ID")?
        .parse()
        .context("invalid WORLD_RUNNER_ID")?;
    let token = required_env("WORLD_RUNNER_TOKEN")?;
    let root = PathBuf::from(required_env("WORLD_RUNNER_ROOT")?);
    if std::env::var_os("WORLD_RUNNER_JOB_UID").is_some() && cli::job_user().is_none() {
        bail!("WORLD_RUNNER_JOB_UID and WORLD_RUNNER_JOB_GID must be numeric ids");
    }
    if cli::job_user().is_some() {
        JobUid::fill(cli::job_uids().context("WORLD_RUNNER_JOB_UIDS must be the number of job uids")?);
        protect_user_settings()?;
    }
    prepare_root(&root).await?;
    let storage = RunnerStorage::from_env(&root, runner_id).await?;
    for name in [
        "WORLD_RUNNER_TOKEN",
        "WORLD_RUNNER_STORAGE_ENDPOINT",
        "WORLD_RUNNER_STORAGE_REGION",
        "WORLD_RUNNER_STORAGE_BUCKET",
        "WORLD_RUNNER_STORAGE_ACCESS_KEY_ID",
        "WORLD_RUNNER_STORAGE_SECRET_ACCESS_KEY",
        "DATABASE_URL",
        "REDIS_URL",
        "AUTH_SECRET",
        "NEXTAUTH_SECRET",
        "GH_TOKEN",
        "GITHUB_TOKEN",
    ] {
        std::env::remove_var(name);
    }
    let ready = Arc::new(AtomicBool::new(false));
    let budget = crate::resources::Budget::detect();
    let running: Arc<Mutex<BTreeMap<Uuid, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let health = health::start_from_env(health::RunnerState {
        ready: ready.clone(),
        running: running.clone(),
        slots: budget.chat_slots + budget.work_slots,
    })
    .await?;
    if let Some(storage) = &storage {
        storage.restore().await?;
    }
    ready.store(true, Ordering::Release);
    health::spawn_self_ping()?;
    crate::model_catalog::spawn_refresh();
    crate::tools_check::spawn_refresh();
    let api = RunnerApi {
        client: Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(70))
            .build()?,
        stream_client: Client::builder().connect_timeout(Duration::from_secs(10)).build()?,
        base_url,
        runner_id,
        token,
    };
    let outbox = Outbox::open(&root, Arc::new(api.clone()))?;
    listen_for_mcp(api.clone(), &root).await?;
    if crate::mcp_hub::enabled() {
        crate::mcp_hub::start(cli::socket_path(&root.join(MCP_HUB_SOCKET))?).await?;
    }
    if crate::key_proxy::enabled() {
        crate::key_proxy::start().await?;
    }
    if crate::cmd_hub::enabled() {
        crate::cmd_hub::start(root.join("cmd-hub").join("hub.sock"), root.join("cmd-hub")).await?;
    }

    info!(runner_id = %runner_id, root = %root.display(), "Local runner starting");
    spawn_busy_file(&root, running.clone());
    watch_disk(root.clone());
    let runner = runner_loop(api, root, storage.map(Arc::new), budget, running, outbox, stop_rx);
    if let Some(health) = health {
        tokio::select! {
            result = runner => result,
            result = health => result.context("health server stopped")?,
        }
    } else {
        runner.await
    }
}

async fn wake_stream(api: RunnerApi, wake: Arc<Notify>, nudge: Arc<Notify>) {
    let mut outage = Outage::new("wake stream");
    loop {
        let connected = Instant::now();
        match api.listen_for_wakes(&wake, &nudge).await {
            Err(error) if error.is::<StreamPaused>() => tokio::time::sleep(Duration::from_secs(15)).await,
            Ok(()) => {
                outage.recovered();
                reconnect_delay().await;
            }
            Err(error) if connected.elapsed() >= HEALTHY_STREAM => {
                outage.recovered();
                tracing::debug!(%error, "Runner wake stream dropped, reconnecting");
                reconnect_delay().await;
            }
            Err(error) => tokio::time::sleep(outage.failed(&error)).await,
        }
    }
}

const LOW_DISK_BYTES: f64 = 5.0 * 1024.0 * 1024.0 * 1024.0;

fn disk_space(path: &Path) -> Option<(f64, f64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as f64;
    Some((stats.f_bavail as f64 * block, stats.f_blocks as f64 * block))
}

fn watch_disk(root: PathBuf) {
    tokio::spawn(async move {
        let mut warned: Option<Instant> = None;
        loop {
            if let Some((free, total)) = disk_space(&root) {
                loka_shared::telemetry::gauge("runner_disk_free_bytes", &[], free);
                loka_shared::telemetry::gauge("runner_disk_total_bytes", &[], total);
                let low = free < LOW_DISK_BYTES || (total > 0.0 && free / total < 0.05);
                if low && warned.is_none_or(|at| at.elapsed() >= Duration::from_secs(3600)) {
                    warned = Some(Instant::now());
                    warn!(free_gb = (free / 1e9 * 10.0).round() / 10.0, total_gb = (total / 1e9).round(), root = %root.display(), "Runner disk is almost full, jobs may fail to write");
                } else if !low {
                    warned = None;
                }
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
}

const HEALTHY_STREAM: Duration = Duration::from_secs(60);
const OUTBOX_FLUSH: Duration = Duration::from_secs(15);
const OUTAGE_REMINDER: Duration = Duration::from_secs(300);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

struct Outage {
    call: &'static str,
    failures: u32,
    since: Option<Instant>,
    reminded: Option<Instant>,
}

impl Outage {
    fn new(call: &'static str) -> Self {
        Outage { call, failures: 0, since: None, reminded: None }
    }

    fn failed(&mut self, error: &anyhow::Error) -> Duration {
        self.failures += 1;
        let since = *self.since.get_or_insert_with(Instant::now);
        if self.reminded.is_none_or(|at| at.elapsed() >= OUTAGE_REMINDER) {
            self.reminded = Some(Instant::now());
            warn!(call = self.call, error = %format!("{error:#}"), failures = self.failures, down_secs = since.elapsed().as_secs(), "Runner cannot reach Loka, retrying");
        }
        let backoff = Duration::from_secs(2).saturating_mul(1 << self.failures.saturating_sub(1).min(5)).min(MAX_RETRY_DELAY);
        let jitter = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_millis() as u64 % 1_000;
        backoff + Duration::from_millis(jitter)
    }

    fn recovered(&mut self) {
        if let Some(since) = self.since.take() {
            info!(call = self.call, failures = self.failures, down_secs = since.elapsed().as_secs(), "Runner reached Loka again");
        }
        self.failures = 0;
        self.reminded = None;
    }
}

async fn runner_loop(
    api: RunnerApi,
    root: PathBuf,
    storage: Option<Arc<RunnerStorage>>,
    budget: crate::resources::Budget,
    running: Arc<Mutex<BTreeMap<Uuid, String>>>,
    outbox: Outbox,
    mut stop_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let chat = Arc::new(Semaphore::new(budget.chat_slots));
    let work = Arc::new(Semaphore::new(budget.work_slots));
    let shells = Arc::new(Semaphore::new(2));
    let terminals = Arc::new(Semaphore::new(5));
    let mut claims = Claims { poll: true, shell: true, terminal: true };
    let warm = Pool::new(budget.warm_sessions);
    tokio::spawn(warm.clone().reap_loop());
    let freed = Arc::new(Notify::new());
    let wake = Arc::new(Notify::new());
    let nudge = Arc::new(Notify::new());
    let listener = tokio::spawn(wake_stream(api.clone(), wake.clone(), nudge.clone()));
    let _listener = AbortOnDrop(listener);
    let github_switch = Arc::new(std::sync::atomic::AtomicU8::new(github_feed::UNKNOWN));
    github_feed::spawn(api.clone(), root.clone(), github_switch.clone());
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel::<anyhow::Error>();
    let mut cleaned_at = Instant::now();
    let rescan = Arc::new(Notify::new());
    workspaces::spawn_usage(root.clone(), rescan.clone());
    let mut clean_requested = false;
    let mut beat_at: Option<Instant> = None;
    let mut poll_outage = Outage::new("poll");
    loop {
        if let Ok(error) = fatal_rx.try_recv() {
            return stop_after_fatal(&api, &warm, &running, &outbox, &freed, error).await;
        }
        if stop_rx.try_recv().is_ok() {
            return stop_after_sigterm(&api, &warm, &running, &outbox, &freed).await;
        }
        let cleanup_due = cleaned_at.elapsed() >= WORKSPACE_CLEANUP_INTERVAL
            || (cleaned_at.elapsed() >= workspaces::LOW_DISK_RECHECK && workspaces::low_disk(&root));
        if clean_requested || cleanup_due {
            cleaned_at = Instant::now();
            let mode = workspaces::mode_for(&root, clean_requested);
            clean_requested = false;
            if let Err(error) = workspaces::sweep(&api, &root, mode, &rescan).await {
                warn!(error = %format!("{error:#}"), "Failed to remove closed task workspaces");
            }
        }
        let mut uids = JobUid::free();
        let mut want = |wanted: bool| {
            let take = wanted && uids > 0;
            uids -= usize::from(take);
            take
        };
        let shell = want(claims.shell && shells.available_permits() > 0);
        let terminal = want(claims.terminal && terminals.available_permits() > 0);
        let mut kinds = Vec::new();
        if want(chat.available_permits() > 0 || work.available_permits() > 0) {
            if chat.available_permits() > 0 {
                kinds.extend(CHAT_KINDS.map(String::from));
            }
            if work.available_permits() > 0 {
                kinds.extend(WORK_KINDS.map(String::from));
            }
        }
        let beat = beat_at.is_none_or(|at| at.elapsed() >= HEARTBEAT_INTERVAL);
        let poll = RunnerPoll {
            heartbeat: beat.then(|| RunnerHeartbeat {
                capabilities: capabilities(),
                leases: held_leases(&running, &outbox),
            }),
            kinds,
            shell,
            terminal,
        };
        if poll_outage.failures > 0 && !outbox.finishing().is_empty() {
            outbox.flush(OUTBOX_FLUSH).await;
        }
        let polled_at = Instant::now();
        let polled = if claims.poll {
            match api.poll(&poll).await {
                Ok(Some(polled)) => Ok(polled),
                Ok(None) => {
                    info!("Loka API has no combined runner poll yet, claiming with separate requests until restart");
                    claims.poll = false;
                    api.poll_separately(&poll, &mut claims).await
                }
                Err(error) => Err(error),
            }
        } else {
            api.poll_separately(&poll, &mut claims).await
        };
        let claim_ms = polled_at.elapsed().as_millis();
        let polled = match polled {
            Ok(polled) if polled.status == "paused" => {
                beat_at = None;
                tokio::time::sleep(Duration::from_secs(15)).await;
                continue;
            }
            Ok(polled) => {
                poll_outage.recovered();
                if beat {
                    beat_at = Some(Instant::now());
                }
                if let Some(on) = polled.github_feed {
                    github_switch.store(if on { github_feed::ON } else { github_feed::OFF }, Ordering::Relaxed);
                }
                if let Some(on) = polled.screen {
                    crate::screen::set_allowed(&root, on);
                }
                if let Some(on) = polled.apps {
                    crate::apps::set_allowed(on);
                }
                clean_requested |= polled.clean_workspaces == Some(true);
                polled
            }
            Err(error) => {
                beat_at = None;
                tokio::time::sleep(poll_outage.failed(&error)).await;
                continue;
            }
        };
        let claimed = polled.job.is_some() || polled.shell.is_some() || polled.terminal.is_some();

        if let Some(job) = polled.shell {
            let permit = shells.clone().acquire_owned().await?;
            let uid = JobUid::take().await.context("job uid setup failed");
            let (api, root) = (api.clone(), root.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let finished = match uid {
                    Ok(uid) => JobUid::scope(uid, execute_shell(&api, &root, job)).await,
                    Err(error) => api.shell_finish(job.id, &failed_shell(format!("{error:#}"))).await,
                };
                if let Err(error) = finished {
                    warn!(error = %format!("{error:#}"), "Shell command job failed");
                }
            });
        }

        if let Some(job) = polled.terminal {
            let permit = terminals.clone().acquire_owned().await?;
            let uid = JobUid::take().await.context("job uid setup failed");
            let (api, root) = (api.clone(), root.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let finished = match uid {
                    Ok(uid) => JobUid::scope(uid, crate::terminal::run_session(&api, &root, job)).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = finished {
                    warn!(error = %format!("{error:#}"), "Terminal session failed");
                }
            });
        }

        if let Some(job) = polled.job {
            let lane = if CHAT_KINDS.contains(&job.kind.as_str()) { chat.clone() } else { work.clone() };
            let permit = lane.acquire_owned().await?;
            let uid = JobUid::take().await.context("job uid setup failed");
            let lane = outbox.lane(job.lease_id);
            let (api, root, storage, warm, freed, fatal, running, nudge) = (
                api.clone(),
                root.clone(),
                storage.clone(),
                warm.clone(),
                freed.clone(),
                fatal_tx.clone(),
                running.clone(),
                nudge.clone(),
            );
            let lease_id = job.lease_id;
            let (job_kind, job_cli) = (job.kind.clone(), job.cli.clone());
            let span = tracing::info_span!("job", run = %job.run_id, agent = %job.agent_id, kind = %job.kind, lease = %lease_id);
            running.lock().unwrap().insert(lease_id, job.workspace.key.clone());
            tokio::spawn(tracing::Instrument::instrument(async move {
                let workspace_key = job.workspace.key.clone();
                let stages = Arc::new(Mutex::new(Stages::default()));
                loka_shared::telemetry::add("runner_jobs_running", &[("kind", &job_kind)], 1.0);
                let started = Instant::now();
                let finished = match uid {
                    Ok(uid) => JobUid::scope(uid, execute_job(&api, &root, job, storage.as_deref(), &warm, &nudge, lane.sink(), stages.clone())).await,
                    Err(error) => {
                        warn!(error = %format!("{error:#}"), "Job uid setup failed");
                        Ok(failed_completion(workspace_key.clone(), format!("{error:#}")))
                    }
                };
                let execute_ms = started.elapsed().as_millis();
                let completed_at = Instant::now();
                let outcome = match &finished {
                    Ok(completion) if completion.error.is_none() && completion.exit_code == 0 => "ok",
                    Ok(_) => "failed",
                    Err(_) => "crashed",
                };
                match finished {
                    Ok(completion) => lane.complete(completion).await,
                    Err(error) => {
                        let failure = failed_completion(workspace_key, format!("{error:#}"));
                        let _ = fatal.send(error);
                        let _ = tokio::time::timeout(FATAL_DRAIN, lane.complete(failure)).await;
                    }
                }
                let stages = std::mem::take(&mut *stages.lock().unwrap());
                info!(
                    claim_ms,
                    prepare_ms = ?stages.prepare_ms,
                    run_ms = ?stages.run_ms,
                    checkpoint_ms = ?stages.checkpoint_ms,
                    execute_ms,
                    complete_ms = completed_at.elapsed().as_millis(),
                    "Job timing"
                );
                job_metrics(&job_kind, &job_cli, outcome, claim_ms, &stages, execute_ms, completed_at.elapsed().as_millis());
                loka_shared::telemetry::add("runner_jobs_running", &[("kind", &job_kind)], -1.0);
                drop(permit);
                running.lock().unwrap().remove(&lease_id);
                freed.notify_one();
                freed.notify_waiters();
            }, span));
        }

        if claimed {
            continue;
        }
        if polled.retry {
            reconnect_delay().await;
            continue;
        }
        let idle = if poll.kinds.is_empty() { HEARTBEAT_INTERVAL } else { IDLE_WAIT };
        tokio::select! {
            _ = wake.notified() => {}
            _ = freed.notified() => {}
            _ = tokio::time::sleep(idle) => {}
            Some(error) = fatal_rx.recv() => {
                return stop_after_fatal(&api, &warm, &running, &outbox, &freed, error).await;
            }
            Some(()) = stop_rx.recv() => {
                return stop_after_sigterm(&api, &warm, &running, &outbox, &freed).await;
            }
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn flush_shell(
    api: &RunnerApi,
    run_id: Uuid,
    pending: &mut String,
    live_sent: &mut usize,
    stop: &mut Option<&'static str>,
    pid: u32,
) {
    let mut chunk = std::mem::take(pending);
    if *live_sent >= OUTPUT_CAP {
        chunk.clear();
    } else if *live_sent + chunk.len() > OUTPUT_CAP {
        let mut keep = OUTPUT_CAP - *live_sent;
        while !chunk.is_char_boundary(keep) {
            keep -= 1;
        }
        chunk.truncate(keep);
    }
    *live_sent += chunk.len();
    match api.shell_output(run_id, &chunk).await {
        Ok(Some(true)) | Ok(None) if stop.is_none() => {
            *stop = Some("cancelled");
            cli::kill_process_trees(vec![pid]).await;
        }
        Err(error) => warn!(error = %format!("{error:#}"), "shell output upload failed"),
        _ => {}
    }
}

async fn execute_shell(api: &RunnerApi, root: &Path, job: ShellJob) -> Result<()> {
    info!(run_id = %job.id, "Running a shell command from chat");
    let workspace = prepare_workspace(
        root,
        &RunnerWorkspace {
            key: job.workspace_key.clone(),
            repo: None,
            branch: None,
            base_branch: None,
            create: true,
        },
    )
    .await?;
    let mut process = match crate::shell::spawn(&workspace, &job.command) {
        Ok(process) => process,
        Err(error) => {
            api.shell_finish(job.id, &failed_shell(format!("{error:#}"))).await?;
            return Ok(());
        }
    };
    let pid = process.pid;
    let start = Instant::now();
    let mut deadline_at = Some(tokio::time::Instant::now() + Duration::from_secs(job.timeout_secs));
    let mut tail = crate::shell::Tail::new(OUTPUT_CAP);
    let mut pending = String::new();
    let mut live_sent = 0usize;
    let mut stop: Option<&'static str> = None;
    let mut uploaded_at = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(400));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            chunk = process.recv() => {
                match chunk {
                    Some(bytes) => {
                        tail.push(&bytes);
                        pending.push_str(&String::from_utf8_lossy(&bytes));
                        if pending.len() > 32 * 1024 {
                            flush_shell(api, job.id, &mut pending, &mut live_sent, &mut stop, pid).await;
                            uploaded_at = Instant::now();
                        }
                    }
                    None => break,
                }
            }
            _ = tick.tick() => {
                if !pending.is_empty() || uploaded_at.elapsed() >= SHELL_CANCEL_CHECK {
                    flush_shell(api, job.id, &mut pending, &mut live_sent, &mut stop, pid).await;
                    uploaded_at = Instant::now();
                }
            }
            _ = async { match deadline_at { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                deadline_at = None;
                if stop.is_none() {
                    stop = Some("timed out");
                    cli::kill_process_trees(vec![pid]).await;
                }
            }
        }
    }
    if !pending.is_empty() {
        flush_shell(api, job.id, &mut pending, &mut live_sent, &mut stop, pid).await;
    }
    let code = process.wait().await;
    let (status, error) = match stop {
        Some("cancelled") => ("cancelled", None),
        Some("timed out") => ("failed", Some(format!("timed out after {}s", job.timeout_secs))),
        _ => match code {
            Some(_) => ("done", None),
            None => ("failed", Some("the command did not exit cleanly".to_string())),
        },
    };
    api.shell_finish(
        job.id,
        &ShellFinish {
            status: status.to_string(),
            exit_code: if status == "done" { code } else { None },
            duration_ms: start.elapsed().as_millis() as i64,
            output: tail.text(),
            truncated: tail.truncated(),
            total_bytes: tail.total as i64,
            error,
        },
    )
    .await?;
    Ok(())
}

async fn stop_after_fatal(
    api: &RunnerApi,
    warm: &Pool,
    running: &Mutex<BTreeMap<Uuid, String>>,
    outbox: &Outbox,
    freed: &Notify,
    error: anyhow::Error,
) -> Result<()> {
    warn!(error = %format!("{error:#}"), "Runner job failed fatally, letting other jobs finish before restarting");
    let reason = format!("runner restarted after another job failed: {error:#}");
    wind_down(api, warm, running, outbox, freed, FATAL_DRAIN, reason).await;
    Err(error)
}

async fn stop_after_sigterm(
    api: &RunnerApi,
    warm: &Pool,
    running: &Mutex<BTreeMap<Uuid, String>>,
    outbox: &Outbox,
    freed: &Notify,
) -> Result<()> {
    if pausing_for_update() {
        wind_down(api, warm, running, outbox, freed, UPDATE_DRAIN, loka_shared::run_causes::UPDATE_PAUSE.to_string()).await;
        return Ok(());
    }
    let drain = Duration::from_secs(cli::env_secs("WORLD_RUNNER_DRAIN_SECS", FATAL_DRAIN.as_secs()));
    info!(drain_secs = drain.as_secs(), "Runner received SIGTERM, letting running jobs finish before stopping");
    wind_down(api, warm, running, outbox, freed, drain, "runner stopped before the job finished".to_string()).await;
    Ok(())
}

async fn wind_down(
    api: &RunnerApi,
    warm: &Pool,
    running: &Mutex<BTreeMap<Uuid, String>>,
    outbox: &Outbox,
    freed: &Notify,
    drain: Duration,
    reason: String,
) {
    let deadline = tokio::time::Instant::now() + drain;
    while !running.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
        let beat_deadline = deadline.min(tokio::time::Instant::now() + HEARTBEAT_INTERVAL);
        let held = held_leases(running, outbox);
        match tokio::time::timeout_at(beat_deadline, api.heartbeat_runner(&held)).await {
            Ok(Err(error)) => warn!(error = %format!("{error:#}"), "Runner heartbeat failed"),
            Err(_) => warn!("Runner heartbeat timed out"),
            Ok(Ok(_)) => {}
        }
        let _ = tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + HEARTBEAT_INTERVAL), freed.notified()).await;
    }
    let unfinished = std::mem::take(&mut *running.lock().unwrap());
    for (lease_id, workspace_key) in unfinished {
        let failure = failed_completion(workspace_key, reason.clone());
        if tokio::time::timeout(Duration::from_secs(10), outbox.complete(lease_id, failure)).await.is_err() {
            warn!(%lease_id, "Loka did not take an unfinished job's report before stopping, it stays queued for the next start");
        }
    }
    warm.clear();
}

fn failed_shell(error: String) -> ShellFinish {
    ShellFinish {
        status: "failed".to_string(),
        exit_code: None,
        duration_ms: 0,
        output: String::new(),
        truncated: false,
        total_bytes: 0,
        error: Some(error),
    }
}

fn failed_completion(workspace_key: String, error: String) -> RunnerCompletion {
    RunnerCompletion {
        output: String::new(),
        session_id: None,
        cost_usd: 0.0,
        exit_code: 1,
        usage: RunnerUsage::default(),
        error: Some(error),
        workspace_key,
        pr_url: None,
        pr_number: None,
        publish_pr_state: None,
        checkpoint: None,
        steered: Vec::new(),
        retryable: true,
        work_summary: None,
    }
}

fn runner_platform() -> &'static str {
    std::env::consts::OS
}

fn held_leases(running: &Mutex<BTreeMap<Uuid, String>>, outbox: &Outbox) -> Vec<Uuid> {
    let mut held: Vec<Uuid> = running.lock().unwrap().keys().copied().collect();
    for lease in outbox.finishing() {
        if !held.contains(&lease) {
            held.push(lease);
        }
    }
    held
}


#[allow(clippy::too_many_arguments)]
async fn execute_job(
    api: &RunnerApi,
    root: &Path,
    job: RunnerJob,
    storage: Option<&RunnerStorage>,
    warm: &Pool,
    nudge: &Notify,
    events: EventSink,
    stages: Arc<Mutex<Stages>>,
) -> Result<RunnerCompletion> {
    let job = start_fresh_if_transcript_missing(job, storage);
    let workspace_key = job.workspace.key.clone();
    let state_may_change = Arc::new(AtomicBool::new(false));
    let checkpointed = Arc::new(AtomicBool::new(false));
    let started_session_id = Arc::new(Mutex::new(None));
    let pids = Arc::new(Mutex::new(Vec::new()));
    let keys_changed = Arc::new(Notify::new());
    let steering = (matches!(job.kind.as_str(), "task" | "followup" | "direct") && !job.lean && cli::steerable(&job.cli))
        .then(tokio::sync::mpsc::unbounded_channel);
    let (steer_sender, steer_receiver) = match steering {
        Some((sender, receiver)) => (Some(sender), Some(receiver)),
        None => (None, None),
    };
    let delivered: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    let mut inflight: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut acked: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut execution = Box::pin(execute_job_inner(
        root,
        &job,
        storage,
        warm,
        JobState {
            state_may_change: state_may_change.clone(),
            checkpointed: checkpointed.clone(),
            started_session_id: started_session_id.clone(),
            pids: pids.clone(),
            steer: steer_receiver,
            stages,
            keys_changed: keys_changed.clone(),
            events,
        },
    ));
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    let started_at = tokio::time::Instant::now();
    let mut limit_secs = job.timeout_secs;
    let timeout = tokio::time::sleep(Duration::from_secs(job.timeout_secs.max(1)));
    tokio::pin!(timeout);
    let lease_ttl = Duration::from_secs(RUNNER_LEASE_SECONDS as u64);
    let outage_limit = Duration::from_secs(loka_shared::runners::away_grace_secs(runner_platform()) as u64);
    let mut renewed_at = Instant::now();
    let mut renewed_wall = SystemTime::now();
    let started_wall = renewed_wall;
    let deadline = Duration::from_secs(job.timeout_secs.max(1));
    let result = loop {
        tokio::select! {
            result = &mut execution => break result,
            _ = async {
                tokio::select! {
                    _ = heartbeat.tick() => {}
                    _ = nudge.notified(), if steer_sender.is_some() => {}
                }
            } => {
                let beat = LeaseBeat {
                    steer: steer_sender.is_some(),
                    delivered: delivered.lock().unwrap().iter().filter(|id| !acked.contains(id)).copied().collect(),
                };
                let renewal = tokio::time::timeout(HEARTBEAT_INTERVAL, api.heartbeat_lease(job.lease_id, Some(&beat)))
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("lease heartbeat timed out")));
                match renewal {
                    Ok(Some(LeaseSteers { steers, timeout_secs, keys, apps })) => {
                        acked.extend(beat.delivered.iter().copied());
                        if let Some(on) = apps {
                            crate::apps::set_granted(root, job.lease_id, on);
                        }
                        renewed_at = Instant::now();
                        renewed_wall = SystemTime::now();
                        if keys.is_some() && job.keys_version.is_some() && keys != job.keys_version {
                            info!(lease_id = %job.lease_id, "A key this run uses changed or was revoked, so its MCP servers stop");
                            keys_changed.notify_one();
                        }
                        if let Some(secs) = timeout_secs.filter(|secs| *secs != limit_secs) {
                            limit_secs = secs;
                            timeout.as_mut().reset(started_at + Duration::from_secs(secs.max(1)));
                            info!(lease_id = %job.lease_id, secs, "Job time limit changed");
                        }
                        if let Some(sender) = &steer_sender {
                            for steer in steers {
                                if !inflight.insert(steer.id) {
                                    continue;
                                }
                                let sender = sender.clone();
                                let delivered = delivered.clone();
                                tokio::spawn(async move {
                                    if cli::deliver(&sender, steer.text, steer.interrupt).await {
                                        info!(followup = %steer.id, interrupt = steer.interrupt, "Steered follow-up into running job");
                                        delivered.lock().unwrap().push(steer.id);
                                    }
                                });
                            }
                        }
                    }
                    Ok(None) => {
                        terminate_processes(&pids).await;
                        let slept = renewed_wall.elapsed().unwrap_or_default() > renewed_at.elapsed() + lease_ttl;
                        let past_deadline = started_wall.elapsed().unwrap_or_default() >= deadline;
                        break Err(anyhow!(match (slept, past_deadline) {
                            (true, true) => "run reached its time limit while this device was asleep",
                            (true, false) => "runner lease ended while this device was asleep, so the run was stopped",
                            (false, _) => "runner lease was cancelled or revoked",
                        }));
                    }
                    Err(error) => {
                        let unrenewed = renewed_at.elapsed();
                        if unrenewed < HEARTBEAT_INTERVAL * 2 || unrenewed.as_secs() % OUTAGE_REMINDER.as_secs() < HEARTBEAT_INTERVAL.as_secs() {
                            warn!(error = %format!("{error:#}"), lease_id = %job.lease_id, unrenewed_secs = unrenewed.as_secs(), "Loka is unreachable, the job keeps running and reports when it is back");
                        }
                        if unrenewed >= outage_limit {
                            terminate_processes(&pids).await;
                            break Err(anyhow!("runner could not reach Loka for {} minutes, so the run was stopped", outage_limit.as_secs() / 60));
                        }
                    }
                }
            }
            _ = &mut timeout => {
                terminate_processes(&pids).await;
                break Err(anyhow!("runner job timed out"));
            }
            _ = update_requested(), if job.kind != "publish" => {
                terminate_processes(&pids).await;
                break Err(anyhow!(loka_shared::run_causes::UPDATE_PAUSE));
            }
        }
    };
    drop(execution);
    crate::apps::set_granted(root, job.lease_id, false);
    if storage.is_some()
        && state_may_change.load(Ordering::Acquire)
        && !checkpointed.load(Ordering::Acquire)
    {
        return Err(anyhow!(
            "runner stopped with uncheckpointed Claude session state: {}",
            result
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_default()
        ));
    }
    let started_session_id = started_session_id.lock().unwrap().clone();
    Ok(match result {
        Ok(outcome) => {
            let session_id = select_session_id(
                outcome.result.session_id.or(started_session_id),
                job.cli_session_id.as_deref(),
                job.resume,
                job.fork || job.rewind_to.is_some(),
            );
            RunnerCompletion {
                output: outcome.result.output,
                session_id,
                cost_usd: outcome.result.cost_usd,
                exit_code: outcome.result.exit_code,
                usage: RunnerUsage {
                    input_tokens: outcome.result.usage.input_tokens,
                    cached_input_tokens: outcome.result.usage.cached_input_tokens,
                    output_tokens: outcome.result.usage.output_tokens,
                    context_tokens: outcome.result.usage.context_tokens,
                },
                error: None,
                workspace_key,
                pr_url: outcome.pr_url,
                pr_number: outcome.pr_number,
                publish_pr_state: outcome.publish_pr_state,
                checkpoint: outcome.result.checkpoint,
                steered: delivered.lock().unwrap().clone(),
                retryable: false,
                work_summary: outcome.work_summary,
            }
        }
        Err(error) => RunnerCompletion {
            output: String::new(),
            session_id: select_session_id(
                started_session_id,
                job.cli_session_id.as_deref(),
                job.resume,
                job.fork || job.rewind_to.is_some(),
            ),
            cost_usd: 0.0,
            exit_code: 1,
            usage: RunnerUsage::default(),
            error: Some(format!("{error:#}")),
            workspace_key,
            pr_url: None,
            pr_number: None,
            publish_pr_state: None,
            checkpoint: None,
            steered: Vec::new(),
            retryable: pausing_for_update(),
            work_summary: None,
        },
    })
}

fn start_fresh_if_transcript_missing(mut job: RunnerJob, storage: Option<&RunnerStorage>) -> RunnerJob {
    let (true, Some(session_id)) = (job.resume, job.cli_session_id.as_deref()) else {
        return job;
    };
    let present = match storage {
        Some(storage) => storage.has_transcript(&job.cli, session_id),
        None => true,
    };
    if present {
        return job;
    }
    warn!(run_id = %job.run_id, %session_id, "CLI transcript is gone, starting a fresh session");
    job.cli_session_id = (job.cli == "claude").then(|| Uuid::new_v4().to_string());
    job.resume = false;
    job.fork = false;
    job.rewind_to = None;
    job.previous_run_id = None;
    job.prompt = format!("{TRANSCRIPT_GONE}\n\n{}", job.prompt);
    job
}

async fn terminate_processes(pids: &Mutex<Vec<(u32, Option<u64>)>>) {
    let roots = live_pids(pids);
    cli::kill_process_trees(roots).await;
}

struct JobState {
    state_may_change: Arc<AtomicBool>,
    checkpointed: Arc<AtomicBool>,
    started_session_id: Arc<Mutex<Option<String>>>,
    pids: SpawnedPids,
    steer: Option<cli::SteerReceiver>,
    stages: Arc<Mutex<Stages>>,
    keys_changed: Arc<Notify>,
    events: EventSink,
}

async fn execute_job_inner(
    root: &Path,
    job: &RunnerJob,
    storage: Option<&RunnerStorage>,
    warm: &Pool,
    state: JobState,
) -> Result<ExecutionOutcome> {
    let JobState { state_may_change, checkpointed, started_session_id, pids, steer, stages, keys_changed, events } = state;
    let claimed_at = SystemTime::now();
    let prepare_started = Instant::now();
    if CHAT_KINDS.contains(&job.kind.as_str()) {
        tracing::debug!(run_id = %job.run_id, agent_id = %job.agent_id, kind = %job.kind, "Executing runner job");
    } else {
        info!(run_id = %job.run_id, agent_id = %job.agent_id, kind = %job.kind, cli = %job.cli, "Executing runner job");
    }
    if job.kind == "publish" {
        let published = publish_job(root, job).await;
        stages.lock().unwrap().run_ms = Some(prepare_started.elapsed().as_millis());
        return published;
    }
    let work_root = root.to_path_buf();
    let workspace = if job.kind == "direct" {
        shared_folder(root, &job.workspace.key)?
    } else {
        prepare_workspace(&work_root, &job.workspace).await?
    };
    if !loka_shared::backends::is_agent_cli(&job.cli) {
        bail!("local runners support Claude, Codex, Cursor, OpenCode and omp jobs");
    }
    if storage.is_some() && !matches!(job.cli.as_str(), "claude" | "omp") {
        bail!("checkpointed runners support Claude and omp jobs only");
    }
    if job.cli == "cursor" && cli::job_user().is_some() && !cli::cursor_homes_set() {
        bail!("runners with a separate job user run Cursor jobs only when CURSOR_CONFIG_DIR and CURSOR_DATA_DIR point at folders the job group shares");
    }
    let mut granted = job.environment.clone();
    let mut vault_values = Vec::new();
    if let Some(name) = job.vault_refs.keys().next() {
        if storage.is_some() || !cfg!(target_os = "macos") {
            return Err(vault_refs::failure(name, "vault references are read only on their owner's own Mac."));
        }
        let resolved = vault_refs::Resolver::new(vault_refs::find_op()).resolve(&job.vault_refs).await?;
        vault_values.extend(redactable(resolved.values()));
        granted.extend(resolved);
    }
    let mut skills = materialize_skills(&work_root, job).await?;
    let browser = storage.is_none() && crate::browser::wanted(&job.kind) && crate::browser::available();
    if browser {
        skills.push(materialize_browser(root, job).await?);
    }
    let apps_launcher = (storage.is_none() && crate::apps::wanted(&job.kind) && crate::apps::allowed())
        .then(|| crate::apps::launcher(job.agent_id))
        .flatten();
    if let Some(launcher) = &apps_launcher {
        let dir = secure_create_dir(&root.join("skills"), Path::new(&job.session_id.to_string()), false).await?;
        let path = dir.join("apps.md");
        tokio::fs::write(&path, crate::apps::skill(launcher)).await?;
        skills.push(path);
    }
    let screen = storage.is_none() && crate::screen::wanted(&job.kind) && crate::screen::allowed();
    let screen_launcher = if screen { crate::screen::launcher(job.agent_id) } else { None };
    if let Some(launcher) = &screen_launcher {
        let dir = secure_create_dir(&root.join("skills"), Path::new(&job.session_id.to_string()), false).await?;
        let path = dir.join("screen.md");
        tokio::fs::write(&path, crate::screen::skill(launcher)).await?;
        skills.push(path);
    }
    let images = materialize_images(&work_root, job).await?;
    let prompt = prompt_with_skills(&job.prompt, &skills, storage);
    let prompt = match &job.setup_scan {
        Some(request) => {
            if job.kind != "aside" || storage.is_some() {
                bail!("setup suggestions only run on their owner's own Mac");
            }
            let request = request.clone();
            let skip = root.to_path_buf();
            let digest = tokio::task::spawn_blocking(move || crate::setup_scan::digest(&request, &skip)).await?;
            format!("{prompt}\n\n{digest}")
        }
        None => prompt,
    };
    let prompt = match matches!(job.kind.as_str(), "task" | "followup" | "direct" | "review" | "quick")
        .then(|| {
            let host: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
            crate::key_proxy::prompt_note(
                &job.cli,
                job.provider_keys.keys().chain(granted.keys()).map(String::as_str).chain(host.iter().map(String::as_str)),
            )
        })
        .flatten()
    {
        Some(note) => format!("{prompt}\n\n{note}"),
        None => prompt,
    };
    let prompt = match (!job.lean).then(|| crate::cmd_hub::prompt_note(&job.cli, &granted)).flatten() {
        Some(note) => format!("{prompt}\n\n{note}"),
        None => prompt,
    };
    let prompt = match job.command_rules.as_ref().filter(|policy| !job.lean && !policy.is_empty()).and_then(|_| crate::cmd_hub::rules_note(&job.cli)) {
        Some(note) => format!("{prompt}\n\n{note}"),
        None => prompt,
    };
    let mut environment = filtered_environment(&granted, storage.is_some())?;
    if let Some(policy) = job
        .command_rules
        .as_ref()
        .filter(|policy| !job.lean && !policy.is_empty() && loka_shared::command_rules::enforced_for(&job.cli))
    {
        environment.insert(loka_shared::command_rules::ENV.to_string(), serde_json::to_string(policy)?);
    }
    if !job.lean && !job.approved_actions.is_empty() {
        environment.insert(loka_shared::approvals::ENV.to_string(), serde_json::to_string(&job.approved_actions)?);
    }
    let provided = provided_environment();
    for (name, value) in &provided {
        environment.entry(name.clone()).or_insert_with(|| value.clone());
    }
    add_gate_environment(&mut environment, root, job, browser, screen_launcher, apps_launcher);
    if std::env::var_os("COMPOSE_PROJECT_NAME").is_none() {
        environment
            .entry("COMPOSE_PROJECT_NAME".into())
            .or_insert_with(|| compose_project_name(&job.workspace.key));
    }
    let mut media_roots = vec![workspace.clone()];
    if storage.is_none() && matches!(job.kind.as_str(), "task" | "followup") {
        let dir = secure_create_dir(root, &Path::new(crate::browser::MEDIA_DIR).join(job.agent_id.to_string()), false).await?;
        media::prune(&dir);
        media_roots.push(dir);
    }
    let mcp_lease = exposes_world_tools(job).then(|| OpenMcpLease::new(job, pids.clone(), media_roots));
    let mut hub_registration = None;
    let mut hub_values = Vec::new();
    let mcp_config_json = if job.kind == "aside" && !job.world_tools {
        None
    } else {
        let mut servers = runner_mcp_servers(job, root)?;
        crate::mcp_hub::pin_sign_in_dirs(&mut servers, &sign_in_home(root, job));
        if let Some(hub) = crate::mcp_hub::get() {
            let prepared = hub
                .prepare(
                    crate::mcp_hub::Job {
                        isolation: crate::mcp_hub::isolation_for(job.owner_id, job.agent_id),
                        agent: job.agent_id,
                        cwd: &workspace,
                        pids: pids.clone(),
                        passthrough: &["world"],
                    },
                    &servers,
                )
                .await?;
            servers = prepared.servers;
            collect_json_strings(&Value::Object(servers.clone()), &mut hub_values);
            if let Some(registration) = prepared.registration {
                hub_values.extend(registration.secrets().iter().cloned());
                hub_registration = Some(registration);
            }
        } else {
            crate::mcp_hub::strip_shared(&mut servers);
        }
        Some(json!({ "mcpServers": servers }).to_string())
    };
    let keys_watch = hub_registration.as_ref().map(|registration| {
        let revoke = registration.revoker();
        let keys_changed = keys_changed.clone();
        tokio::spawn(async move {
            keys_changed.notified().await;
            revoke.revoke().await;
        })
    });
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let upload = events.clone();
    let mut secrets = redaction_values(job);
    secrets.extend(hub_values);
    secrets.extend(redactable(provided.values()));
    secrets.extend(vault_values);
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.dedup();
    let event_secrets = secrets.clone();
    let observed_session_id = started_session_id.clone();
    let drain_task = tokio::spawn(async move {
        drain_events(event_rx, upload, event_secrets, observed_session_id, pids).await;
    });

    let opts = CliOptions {
        working_dir: &workspace,
        prompt: &prompt,
        system_prompt: job.system_prompt.as_deref(),
        allowed_tools: None,
        max_turns: job.max_turns,
        model: &job.model,
        mcp_config_json,
        session_id: job.cli_session_id.as_deref(),
        resume: job.resume,
        event_tx: Some(event_tx),
        image_paths: images,
        fork: job.fork,
        lean: job.lean,
        effort: job.effort.as_deref(),
        fast: job.fast,
        environment,
        mcp_has_secrets: job.kind != "aside",
        sensitive: false,
        rewind_to: job.rewind_to.as_deref(),
        approval: cli::Approval::parse(job.approval.as_deref()),
        steer: steer.map(cli::SteerSlot::new).unwrap_or_default(),
        omp_models: job.omp_models.clone(),
        provider_keys: job.provider_keys.clone(),
        readable_files: if job.lean && job.kind == "turn" { skills.clone() } else { Vec::new() },
    };
    state_may_change.store(true, Ordering::Release);
    stages.lock().unwrap().prepare_ms = Some(prepare_started.elapsed().as_millis());
    let run_started = Instant::now();
    let compacting = cli::compaction_run(&job.cli, &opts);
    if compacting {
        warm.evict(job.session_id).await;
    }
    let warm_turn = (job.kind == "turn" && job.lean || job.kind == "direct" && job.cli == "claude")
        && job.rewind_to.is_none()
        && !compacting
        && warm::supports_model(&job.cli, opts.model)
        && (!job.metered || warm::reports_usage(&job.cli))
        && warm.enabled();
    let mut cli = Box::pin(async {
        if warm_turn {
            let (result, timing) = match warm.turn(&job.cli, job.session_id, job.previous_run_id, job.run_id, &opts).await {
                Err(e) if crate::acp::is_unoffered(&e) => return cli::run(&job.cli, opts).await,
                other => other?,
            };
            info!(
                agent_id = %job.agent_id,
                backend = %job.cli,
                spawned = timing.spawned,
                spawn_ms = timing.spawn_ms,
                first_output_ms = ?timing.first_output_ms,
                result_ms = ?timing.result_ms,
                tool_calls = timing.tool_calls,
                "Turn timing"
            );
            let labels = [("backend", job.cli.as_str()), ("start", if timing.spawned { "cold" } else { "warm" })];
            if let Some(ms) = timing.first_output_ms {
                loka_shared::telemetry::observe("turn_first_output_seconds", &labels, ms as f64 / 1000.0);
            }
            if let Some(ms) = timing.result_ms {
                loka_shared::telemetry::observe("turn_result_seconds", &labels, ms as f64 / 1000.0);
            }
            Ok(result)
        } else {
            cli::run(&job.cli, opts).await
        }
    });
    let outcome = (&mut cli).await;
    drop(cli);
    drop(mcp_lease);
    if let Some(watch) = keys_watch {
        watch.abort();
    }
    drop(hub_registration);
    let _ = drain_task.await;
    stages.lock().unwrap().run_ms = Some(run_started.elapsed().as_millis());
    if let Some(storage) = storage {
        let checkpoint_started = Instant::now();
        let session_id = started_session_id.lock().unwrap().clone().filter(|_| storage.keeps_sessions_of(&job.cli));
        storage
            .checkpoint(
                session_id.as_deref(),
                job.live_sessions.as_deref().map(|ids| (ids, claimed_at)),
            )
            .await?;
        stages.lock().unwrap().checkpoint_ms = Some(checkpoint_started.elapsed().as_millis());
    }
    checkpointed.store(true, Ordering::Release);
    let mut result = outcome.map_err(|error| anyhow!(redact(&format!("{error:#}"), &secrets)))?;
    result.output = redact(&result.output, &secrets);
    let work_summary = match (job.kind.as_str(), job.workspace.base_branch.as_deref()) {
        ("task" | "followup", Some(base)) if job.workspace.repo.is_some() && crate::world::prompts::parse_report(&result.output).is_none() => {
            crate::git::workspace::work_summary(&workspace, base).await
        }
        _ => None,
    }
    .map(|summary| redact(&summary, &secrets));
    Ok(ExecutionOutcome {
        result,
        pr_url: None,
        pr_number: None,
        publish_pr_state: None,
        work_summary,
    })
}

async fn publish_job(root: &Path, job: &RunnerJob) -> Result<ExecutionOutcome> {
    let publish = job
        .publish
        .as_ref()
        .ok_or_else(|| anyhow!("publish metadata is required"))?;
    let repo = validate_repo(&publish.repo)?;
    validate_git_ref(&publish.base_branch)?;
    if job.workspace.repo.as_deref() != Some(repo) {
        bail!("publish repository does not match the workspace");
    }
    let workspace = prepare_workspace(root, &job.workspace).await?;
    let quest_branch = job
        .workspace
        .branch
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&publish.branch);
    crate::git::workspace::commit_signed(&workspace, &publish.commit_message).await?;
    let ahead = crate::git::workspace::ahead_of_fresh_base(&workspace, &publish.base_branch).await?;
    if publish.pr_number.is_none() && !ahead {
        return Ok(ExecutionOutcome {
            result: crate::cli::CliResult {
                output: "Nothing to publish".to_string(),
                session_id: job.cli_session_id.clone(),
                cost_usd: 0.0,
                exit_code: 0,
                usage: crate::cli::Usage::default(),
                checkpoint: None,
            },
            pr_url: None,
            pr_number: None,
            publish_pr_state: None,
            work_summary: None,
        });
    }
    if let (Some(number), Some(url)) = (publish.pr_number, publish.pr_url.as_deref()) {
        if let Some(state) = crate::git::github::pr_state(&workspace, repo, number).await? {
            if crate::git::github::is_final_pr_state(&state) {
                let state = state.to_lowercase();
                return Ok(ExecutionOutcome {
                    result: crate::cli::CliResult {
                        output: format!("Pull request #{number} is already {state}, nothing to publish"),
                        session_id: job.cli_session_id.clone(),
                        cost_usd: 0.0,
                        exit_code: 0,
                        usage: crate::cli::Usage::default(),
                        checkpoint: None,
                    },
                    pr_url: Some(url.to_string()),
                    pr_number: Some(number),
                    publish_pr_state: Some(state),
                    work_summary: None,
                });
            }
        }
    }
    if let Some((url, number)) = kept_pr(publish, ahead) {
        return Ok(ExecutionOutcome {
            result: crate::cli::CliResult {
                output: format!("Nothing new to publish, keeping pull request #{number}"),
                session_id: job.cli_session_id.clone(),
                cost_usd: 0.0,
                exit_code: 0,
                usage: crate::cli::Usage::default(),
                checkpoint: None,
            },
            pr_url: Some(url),
            pr_number: Some(number),
            publish_pr_state: None,
            work_summary: None,
        });
    }
    let mut publish_pr_state = None;
    let pr = match crate::git::github::open_pr_containing_head(
        &workspace,
        repo,
        &publish.base_branch,
        quest_branch,
    )
    .await?
    {
        crate::git::github::HeadPr::Own(pr, head) if ahead => {
            crate::git::workspace::ensure_signed_commits(&workspace, &publish.base_branch).await?;
            crate::git::workspace::push_quest_branch(&workspace, &head, quest_branch).await?;
            Some(pr)
        }
        crate::git::github::HeadPr::Elsewhere(pr) => {
            info!(pr = %pr.url, "Work already sits in another branch's pull request, not attaching it");
            publish_pr_state = Some(loka_shared::runners::PUBLISH_ELSEWHERE.to_string());
            None
        }
        crate::git::github::HeadPr::Missing if ahead => 'missing: {
            if !crate::git::workspace::ahead_of_fresh_base(&workspace, &publish.base_branch).await? {
                info!("The base branch caught up with this work while publishing, not opening a pull request");
                break 'missing None;
            }
            let branch =
                crate::git::workspace::publish_branch(&workspace, Some(quest_branch)).await?;
            if branch == publish.base_branch {
                bail!("publish workspace is on the base branch");
            }
            crate::git::workspace::ensure_signed_commits(&workspace, &publish.base_branch).await?;
            crate::git::workspace::push_quest_branch(&workspace, &branch, quest_branch).await?;
            let into = crate::git::github::stack_base(&workspace, repo, &publish.base_branch, quest_branch, publish.stack_onto_pr).await;
            crate::git::github::create_pr(
                &workspace,
                repo,
                &branch,
                &into,
                &publish.pr_title,
                &publish.pr_body,
            )
            .await?
        }
        _ => None,
    };
    if let Err(error) = run_git(
        &workspace,
        &[
            "clean",
            "-fdX",
            "--",
            ":(glob)**/target",
            ":(glob)**/node_modules",
        ],
    )
    .await
    {
        warn!(error = %format!("{error:#}"), "Failed to clear build directories after publish");
    }
    Ok(ExecutionOutcome {
        result: crate::cli::CliResult {
            output: pr.as_ref().map_or_else(
                || "Nothing to publish".to_string(),
                |pr| format!("Published {}", pr.url),
            ),
            session_id: job.cli_session_id.clone(),
            cost_usd: 0.0,
            exit_code: 0,
            usage: crate::cli::Usage::default(),
            checkpoint: None,
        },
        pr_url: pr.as_ref().map(|pr| pr.url.clone()),
        pr_number: pr.map(|pr| i64::from(pr.number)),
        publish_pr_state,
        work_summary: None,
    })
}

fn kept_pr(publish: &RunnerPublish, ahead: bool) -> Option<(String, i64)> {
    if ahead {
        return None;
    }
    let url = publish.pr_url.as_deref().map(str::trim).filter(|url| !url.is_empty())?;
    Some((url.to_string(), publish.pr_number?))
}

const LIVE_USAGE_EVERY: Duration = Duration::from_secs(5);

async fn drain_events(
    mut receiver: mpsc::UnboundedReceiver<StreamEvent>,
    sender: EventSink,
    secrets: Vec<String>,
    started_session_id: Arc<Mutex<Option<String>>>,
    pids: SpawnedPids,
) {
    let mut last_usage: Option<Instant> = None;
    while let Some(event) = receiver.recv().await {
        match &event {
            StreamEvent::SessionStarted { session_id } => {
                *started_session_id.lock().unwrap() = Some(session_id.clone());
            }
            StreamEvent::Spawned { pid } => pids.lock().unwrap().push((*pid, cli::process_start(*pid))),
            StreamEvent::Usage(usage) => {
                if last_usage.is_none_or(|at| at.elapsed() >= LIVE_USAGE_EVERY) {
                    last_usage = Some(Instant::now());
                    if let Ok(text) = serde_json::to_string(usage) {
                        sender.push(RunnerEvent { kind: loka_shared::runners::USAGE_EVENT.to_string(), tool: None, text });
                    }
                }
                continue;
            }
            _ => {}
        }
        if let Some(event) = map_event(event, &secrets) {
            sender.push(event);
        }
    }
}

fn select_session_id(
    started_session_id: Option<String>,
    configured_session_id: Option<&str>,
    resume: bool,
    fork: bool,
) -> Option<String> {
    started_session_id.or_else(|| {
        (resume && !fork)
            .then(|| configured_session_id.map(str::to_string))
            .flatten()
    })
}

fn map_event(event: StreamEvent, secrets: &[String]) -> Option<RunnerEvent> {
    let (kind, tool, text) = match event {
        StreamEvent::Spawned { .. } | StreamEvent::Usage(_) => return None,
        StreamEvent::SessionStarted { session_id } => (
            "text".to_string(),
            None,
            format!("Session started: {session_id}"),
        ),
        StreamEvent::ToolUse {
            tool,
            input_summary,
        } => ("tool".to_string(), Some(tool), input_summary),
        StreamEvent::ToolResult { tool, output } => ("result".to_string(), Some(tool), output),
        StreamEvent::AssistantText(text) => ("text".to_string(), None, text),
        StreamEvent::Error(text) => ("error".to_string(), None, text),
    };
    Some(RunnerEvent {
        kind,
        tool,
        text: redact(&truncate(&text, MAX_EVENT_TEXT), secrets),
    })
}

fn redactable<'a>(values: impl IntoIterator<Item = &'a String> + 'a) -> impl Iterator<Item = String> + 'a {
    values.into_iter().filter(|value| value.len() >= 4).cloned()
}

fn redaction_values(job: &RunnerJob) -> Vec<String> {
    let mut values = redactable(job.environment.values()).collect::<Vec<_>>();
    values.extend(redactable(job.provider_keys.values()));
    values.push(job.mcp_token.clone());
    values.extend(redactable(&job.omp_secrets));
    collect_json_strings(&job.mcp_servers, &mut values);
    values.extend(cli::host_secrets().iter().cloned());
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

pub(crate) fn collect_json_strings(value: &Value, values: &mut Vec<String>) {
    collect_json_secrets(value, false, values);
}

fn collect_json_secrets(value: &Value, sensitive: bool, values: &mut Vec<String>) {
    match value {
        Value::String(value) if sensitive && value.len() >= 8 => {
            values.push(value.clone());
            values.extend(value.split_whitespace().filter(|part| part.len() >= 8 && part.len() < value.len()).map(String::from));
        }
        Value::Array(items) => {
            for item in items {
                collect_json_secrets(item, sensitive, values);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                let child_sensitive = sensitive
                    || matches!(
                        key.as_str(),
                        "env" | "headers" | "token" | "authorization" | "url"
                    )
                    || key.contains("secret")
                    || key.contains("password")
                    || key.contains("api_key")
                    || key.contains("apikey");
                collect_json_secrets(value, child_sensitive, values);
            }
        }
        _ => {}
    }
}

pub(crate) fn redact(text: &str, values: &[String]) -> String {
    values.iter().fold(text.to_string(), |text, value| {
        text.replace(value, "[REDACTED]")
    })
}

async fn listen_for_mcp(api: RunnerApi, root: &Path) -> Result<()> {
    let path = cli::socket_path(&root.join(MCP_SOCKET))?;
    let _ = tokio::fs::remove_file(&path).await;
    let listener = tokio::net::UnixListener::bind(&path).with_context(|| format!("could not open the runner MCP socket {}", path.display()))?;
    cli::share_with_jobs(&path, 0o666)?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let api = api.clone();
                    tokio::spawn(async move {
                        if let Err(error) = relay_mcp(&api, stream).await {
                            warn!(error = %format!("{error:#}"), "Runner MCP request failed");
                        }
                    });
                }
                Err(error) => {
                    warn!(error = %format!("{error:#}"), "Runner MCP socket accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    Ok(())
}

const MAX_MCP_REQUEST_BYTES: u64 = 4 * 1024 * 1024;

async fn relay_mcp(api: &RunnerApi, stream: tokio::net::UnixStream) -> Result<()> {
    let pid = stream.peer_cred()?.pid().context("runner MCP peer has no pid")?;
    let (lease_id, token, roots) = lease_for_process(pid as u32)
        .await
        .context("runner MCP peer is not part of a running job")?;
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader.take(MAX_MCP_REQUEST_BYTES)).read_line(&mut line).await?;
    if !line.ends_with('\n') && line.len() as u64 >= MAX_MCP_REQUEST_BYTES {
        bail!("runner MCP request is larger than {MAX_MCP_REQUEST_BYTES} bytes");
    }
    let mut request: Value = serde_json::from_str(&line)?;
    if media::wants_files(&request) {
        if let Err(message) = media::upload_files(api, lease_id, &token, &roots, &mut request).await {
            writer.write_all(&serde_json::to_vec(&media::refusal(&request, &message))?).await?;
            return Ok(());
        }
    }
    let response = send_json_through_restart(
        || api.client.post(api.lease_url(lease_id, "/mcp")).header("Authorization", format!("RunnerMcp {token}")),
        &request,
    )
    .await;
    let response = match response {
        Ok(response) if response.status().is_server_error() => Err(anyhow!("runner MCP returned {}", response.status())),
        other => other,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!(error = %format!("{error:#}"), %lease_id, "Loka is unreachable, answering the agent's tool call with an offline note");
            if let Some(reply) = offline_reply(&request) {
                writer.write_all(&serde_json::to_vec(&reply)?).await?;
            }
            return Ok(());
        }
    };
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    ensure_success(response.status(), "runner MCP")?;
    writer.write_all(&response.bytes().await?).await?;
    Ok(())
}

const OFFLINE_TOOL_NOTE: &str = "Loka is unreachable right now, so this Loka tool could not run. Your own work and other tools keep going; try this one again in a few minutes, or carry on without it and mention it in your report.";

fn offline_reply(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    if request.get("method").and_then(Value::as_str) == Some("tools/call") {
        return Some(media::refusal(request, OFFLINE_TOOL_NOTE));
    }
    Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": OFFLINE_TOOL_NOTE } }))
}

async fn lease_for_process(pid: u32) -> Option<(Uuid, String, Vec<PathBuf>)> {
    let leases = MCP_LEASES.lock().unwrap();
    let mut current = pid;
    for _ in 0..64 {
        if let Some((lease_id, lease)) =
            leases.iter().find(|(_, lease)| lease.pids.lock().unwrap().iter().any(|(pid, started)| *pid == current && same_process(*pid, *started)))
        {
            return Some((*lease_id, lease.token.clone(), lease.roots.clone()));
        }
        current = cli::parent_pid(current).filter(|parent| *parent > 1)?;
    }
    None
}

fn runner_mcp_servers(job: &RunnerJob, root: &Path) -> Result<serde_json::Map<String, Value>> {
    let executable = std::env::current_exe()?.to_string_lossy().to_string();
    let mut servers = match &job.mcp_servers {
        Value::Object(map) => map.clone(),
        Value::Null => serde_json::Map::new(),
        _ => bail!("invalid MCP server configuration"),
    };
    if exposes_slack(&job.kind) && job.private {
        attach_slack(&mut servers);
    } else {
        servers.retain(|_, server| !is_slack_server(server));
    }
    if exposes_world_tools(job) {
        servers.insert(
            "world".to_string(),
            json!({
                "command": executable,
                "args": ["mcp", "runner"],
                "env": { "WORLD_RUNNER_SOCKET": cli::socket_path(&root.join(MCP_SOCKET))?.to_string_lossy() },
            }),
        );
    }
    Ok(servers)
}

fn sign_in_home(root: &Path, job: &RunnerJob) -> PathBuf {
    match job.owner_id {
        Some(owner) => root.join("mcp-auth").join(owner.to_string()),
        None => root.join("mcp-auth").join(job.agent_id.to_string()),
    }
}

fn exposes_world_tools(job: &RunnerJob) -> bool {
    world_tools_for(&job.kind, job.world_tools)
}

fn world_tools_for(kind: &str, owner_aside: bool) -> bool {
    matches!(kind, "turn" | "task" | "followup" | "quick") || (kind == "aside" && owner_aside)
}

fn exposes_slack(kind: &str) -> bool {
    matches!(kind, "turn" | "task" | "followup" | "quick")
}

fn attach_slack(servers: &mut serde_json::Map<String, Value>) {
    let (Ok(script), Ok(key_file)) = (std::env::var("WORLD_SLACK_MCP_SCRIPT"), std::env::var("WORLD_SLACK_KEY_FILE")) else {
        servers.retain(|_, server| !is_slack_server(server));
        return;
    };
    let Some(key) = std::fs::read_to_string(&key_file)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    else {
        servers.retain(|_, server| !is_slack_server(server));
        return;
    };
    slack_servers(servers, &script, &key);
}

fn is_slack_server(server: &Value) -> bool {
    loka_shared::environment::is_mac_slack_server(server)
}

fn slack_servers(servers: &mut serde_json::Map<String, Value>, script: &str, key: &str) {
    let mut wired = false;
    for server in servers.values_mut() {
        if !is_slack_server(server) {
            continue;
        }
        let mut env = server.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
        env.insert("WORLD_SLACK_CONNECTION_KEY".into(), json!(key));
        server["command"] = json!("/usr/bin/python3");
        server["args"] = json!(["-E", "-s", script]);
        server["env"] = Value::Object(env);
        wired = true;
    }
    if !wired {
        servers.insert(
            "slack".to_string(),
            json!({
                "command": "/usr/bin/python3",
                "args": ["-E", "-s", script],
                "env": { "WORLD_SLACK_CONNECTION_KEY": key },
                "default_tools_approval_mode": "approve",
            }),
        );
    }
}

pub async fn serve_mcp() -> Result<()> {
    let socket = PathBuf::from(required_env("WORLD_RUNNER_SOCKET")?);
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => continue,
        };
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .context("runner MCP socket is gone")?;
        stream.write_all(format!("{request}\n").as_bytes()).await?;
        let mut body = Vec::new();
        stream.read_to_end(&mut body).await?;
        if body.is_empty() {
            if request.get("id").is_some() {
                bail!("runner MCP request failed");
            }
            continue;
        }
        stdout.write_all(&body).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

async fn prepare_root(root: &Path) -> Result<()> {
    tokio::fs::create_dir_all(root).await?;
    let _ = secure_create_dir(root, Path::new("repos"), true).await?;
    let _ = secure_create_dir(root, Path::new("workspaces"), true).await?;
    let _ = secure_create_dir(root, Path::new("leases"), false).await?;
    let _ = secure_create_dir(root, Path::new("omp"), false).await?;
    let _ = secure_create_dir(root, Path::new("omp/sessions"), true).await?;
    let _ = secure_create_dir(root, Path::new("skills"), false).await?;
    let _ = secure_create_dir(root, Path::new("uploads"), false).await?;
    let tmp = secure_create_dir(root, Path::new("tmp"), false).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o711)).await?;
    }
    Ok(())
}

fn protect_user_settings() -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) else {
            return Ok(());
        };
        let path = Path::new(&dir).join("settings.json");
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return Ok(());
        };
        if metadata.is_file() && metadata.uid() == cli::runner_uid() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(metadata.mode() & 0o755))?;
        } else if !crate::claude::trusted_settings_file(&path) {
            let aside = path.with_file_name(format!("settings.json.untrusted-{}", Uuid::new_v4().simple()));
            std::fs::rename(&path, &aside)?;
            warn!(path = %aside.display(), "Moved a Claude settings.json that a job could have written; recreate it as the runner user");
        }
    }
    Ok(())
}

async fn prepare_workspace(root: &Path, workspace: &RunnerWorkspace) -> Result<PathBuf> {
    let relative = validated_relative_path(&workspace.key)?;
    let workspaces = root.join("workspaces");
    let target = workspaces.join(relative);
    ensure_contained(&workspaces, &target).await?;
    let Some(repo) = workspace.repo.as_deref() else {
        return secure_create_dir(&workspaces, target.strip_prefix(&workspaces)?, true).await;
    };
    let repo = validate_repo(repo)?;
    let repo_path = repository_path(root, repo).await?;
    if !target.exists() {
        let branch = workspace
            .branch
            .as_deref()
            .ok_or_else(|| anyhow!("workspace branch is required"))?;
        validate_git_ref(branch)?;
        let staging = target.with_file_name(format!("worktree-{}", Uuid::new_v4().simple()));
        let target_arg = staging.to_string_lossy().to_string();
        let stems: Vec<&str> = std::iter::once(branch).chain(crate::git::workspace::legacy_branch(branch)).collect();
        let patterns: Vec<String> = stems
            .iter()
            .flat_map(|stem| [format!("refs/heads/{stem}"), format!("refs/heads/{stem}-[0-9]*")])
            .collect();
        let mut args = vec!["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)"];
        args.extend(patterns.iter().map(String::as_str));
        let refs = git_output(&repo_path, &args).await?;
        let checked_out = git_output(&repo_path, &["worktree", "list", "--porcelain"]).await?;
        let in_use: Vec<&str> = checked_out
            .lines()
            .filter_map(|line| line.strip_prefix("branch refs/heads/"))
            .collect();
        let existing = refs.lines().map(str::trim).find(|name| {
            !in_use.contains(name) && stems.iter().any(|stem| crate::git::workspace::is_branch_of(name, stem))
        });
        if let (false, Some(existing)) = (workspace.create, existing) {
            run_git(&repo_path, &["worktree", "add", "--no-checkout", &target_arg, existing]).await?;
        } else {
            let base = workspace.base_branch.as_deref().unwrap_or("main");
            validate_git_ref(base)?;
            if !crate::git::workspace::fetched_recently(&repo_path, base) {
                run_git(&repo_path, &["fetch", "origin", base]).await?;
                crate::git::workspace::mark_fetched(&repo_path, base);
            }
            let base_ref = format!("origin/{base}");
            let name = crate::git::workspace::unused_branch_name(&repo_path, branch).await;
            run_git(
                &repo_path,
                &["worktree", "add", "--no-checkout", "-b", &name, &target_arg, &base_ref],
            )
            .await?;
        }
        run_git(&staging, &["reset", "--hard", "--quiet"]).await?;
        run_git(
            &repo_path,
            &["worktree", "move", &target_arg, &target.to_string_lossy()],
        )
        .await?;
    }
    canonical_contained(&workspaces, &target).await
}

async fn repository_path(root: &Path, repo: &str) -> Result<PathBuf> {
    let mappings = std::env::var("WORLD_RUNNER_REPOSITORIES")
        .ok()
        .and_then(|value| serde_json::from_str::<BTreeMap<String, String>>(&value).ok())
        .unwrap_or_default();
    if let Some(path) = mappings.get(repo) {
        let path = tokio::fs::canonicalize(path).await?;
        ensure_git_repo(&path).await?;
        return Ok(path);
    }
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid repository"))?;
    let repositories = root.join("repos");
    let path = repositories.join(owner).join(name);
    ensure_contained(&repositories, &path).await?;
    if !path.exists() {
        let url = format!("https://github.com/{repo}.git");
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("invalid repository path"))?;
        let path_arg = path.to_string_lossy().to_string();
        run_git(parent, &["clone", &url, &path_arg]).await?;
    }
    ensure_git_repo(&path).await?;
    let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    if let Err(e) = crate::git::workspace::ensure_repo_identity(&path, &token).await {
        warn!(repo, error = %e, "Failed to set repo git identity");
    }
    if let Err(e) = crate::git::workspace::ensure_commit_hygiene(&path).await {
        warn!(repo, error = %e, "Failed to install commit hook");
    }
    Ok(path)
}

async fn ensure_git_repo(path: &Path) -> Result<()> {
    let output = cli::workspace_command("git", path)
        .await
        .args(["rev-parse", "--git-dir"])
        .output()
        .await?;
    if !output.status.success() {
        bail!("selected repository is not a Git repository");
    }
    Ok(())
}

async fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let output = cli::workspace_command("git", dir)
        .await
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = cli::workspace_command("git", dir)
        .await
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

async fn materialize_skills(root: &Path, job: &RunnerJob) -> Result<Vec<PathBuf>> {
    let skills_root = root.join("skills");
    let dir = secure_create_dir(&skills_root, Path::new(&job.session_id.to_string()), false).await?;
    let mut paths = Vec::new();
    for skill in &job.skills {
        let name = validated_file_name(&skill.name)?;
        let path = dir.join(format!("{name}.md"));
        tokio::fs::write(&path, &skill.body).await?;
        paths.push(path);
    }
    Ok(paths)
}

async fn materialize_browser(root: &Path, job: &RunnerJob) -> Result<PathBuf> {
    let profiles = secure_create_dir(root, Path::new("browser"), false).await?;
    secure_create_dir(&profiles, Path::new(&job.agent_id.to_string()), false).await?;
    let dir = secure_create_dir(&root.join("skills"), Path::new(&job.session_id.to_string()), false).await?;
    let path = dir.join("browser.md");
    let launcher = crate::browser::launcher(job.agent_id, &job.kind).ok_or_else(|| anyhow::anyhow!("the browser launcher path is not usable"))?;
    tokio::fs::write(&path, crate::browser::skill(&launcher)).await?;
    Ok(path)
}

async fn materialize_images(root: &Path, job: &RunnerJob) -> Result<Vec<PathBuf>> {
    if job.images.is_empty() {
        return Ok(Vec::new());
    }
    let uploads_root = secure_create_dir(root, Path::new("uploads"), false).await?;
    let dir = secure_create_dir(&uploads_root, Path::new(&job.run_id.to_string()), false).await?;
    let mut paths = Vec::new();
    for image in &job.images {
        if !matches!(
            image.mime_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            bail!("unsupported runner image type");
        }
        let name = validated_file_name(&image.name)?;
        let bytes = STANDARD
            .decode(&image.data_base64)
            .context("invalid image data")?;
        if bytes.len() > 10 * 1024 * 1024 {
            bail!("runner image exceeds 10 MB");
        }
        let path = dir.join(format!("{}-{name}", image.id));
        tokio::fs::write(&path, bytes).await?;
        paths.push(path);
    }
    Ok(paths)
}

fn prompt_with_skills(prompt: &str, skills: &[PathBuf], storage: Option<&RunnerStorage>) -> String {
    if prompt == cli::COMPACT_PROMPT {
        return prompt.to_string();
    }
    let mut sections = vec![prompt.to_string()];
    if !skills.is_empty() {
        let index = skills
            .iter()
            .map(|path| format!("- {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("[skills]\n{index}"));
    }
    if let Some(storage) = storage {
        sections.push(format!(
            "[persistent memory]\nStore durable notes in {}.",
            storage.memory_dir().display()
        ));
    }
    sections.join("\n\n")
}

fn provided_environment() -> BTreeMap<String, String> {
    std::env::var("WORLD_RUNNER_JOB_ENV")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| loka_shared::environment::valid_name(name) && !matches!(*name, "GH_TOKEN" | "GITHUB_TOKEN"))
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect()
}

fn add_gate_environment(
    environment: &mut BTreeMap<String, String>,
    root: &Path,
    job: &RunnerJob,
    browser: bool,
    screen_launcher: Option<String>,
    apps_launcher: Option<String>,
) {
    if browser || screen_launcher.is_some() || apps_launcher.is_some() {
        environment.insert(crate::browser::ROOT_ENV.to_string(), root.display().to_string());
    }
    if browser {
        for (name, value) in crate::browser::environment(root, job.agent_id, &job.kind) {
            environment.insert(name, value);
        }
    }
    if let Some(launcher) = screen_launcher {
        environment.entry(crate::screen::LAUNCHER_ENV.to_string()).or_insert(launcher);
    }
    if let Some(launcher) = apps_launcher {
        environment.entry(crate::apps::LAUNCHER_ENV.to_string()).or_insert(launcher);
        environment.entry(crate::apps::LEASE_ENV.to_string()).or_insert(job.lease_id.to_string());
    }
}

fn filtered_environment(
    environment: &BTreeMap<String, String>,
    persistent: bool,
) -> Result<BTreeMap<String, String>> {
    let blocked = [
        "REDIS_URL",
        "AUTH_SECRET",
        "NEXTAUTH_SECRET",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "WORLD_RUNNER_TOKEN",
        "WORLD_RUNNER_MCP_TOKEN",
    ];
    let mut filtered = BTreeMap::new();
    for (name, value) in environment {
        if persistent
            && matches!(
                name.as_str(),
                "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | "WORLD_MEMORY_DIR"
            )
        {
            bail!("runner job attempted to override durable environment key {name}");
        }
        if blocked.contains(&name.as_str()) || name.starts_with("WORLD_RUNNER_") {
            bail!("runner job attempted to grant reserved environment key {name}");
        }
        if !loka_shared::environment::valid_name(name) {
            bail!("runner job contains invalid environment key {name}");
        }
        filtered.insert(name.clone(), value.clone());
    }
    Ok(filtered)
}

fn validated_relative_path(value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.len() > 240 {
        bail!("invalid workspace key");
    }
    if value
        .split(['/', '\\'])
        .any(|part| matches!(part, "" | "." | ".."))
    {
        bail!("invalid workspace key");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        bail!("workspace key must be relative");
    }
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty()
        || components.len() > 8
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid workspace key");
    }
    Ok(path.to_path_buf())
}

fn validated_file_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 120
        || value.contains('/')
        || value.contains('\\')
        || matches!(value, "." | "..")
    {
        bail!("invalid file name");
    }
    Ok(value.to_string())
}

async fn ensure_contained(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| anyhow!("workspace path escapes runner root"))?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let _ = secure_create_dir(root, parent, true).await?;
    if let Ok(metadata) = tokio::fs::symlink_metadata(target).await {
        if metadata.file_type().is_symlink() {
            bail!("workspace path contains a symlink");
        }
    }
    Ok(())
}

async fn secure_create_dir(root: &Path, relative: &Path, for_job: bool) -> Result<PathBuf> {
    let root = tokio::fs::canonicalize(root).await?;
    let relative = validated_relative_or_empty(relative)?;
    let mut current = root.clone();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("invalid runner path");
        };
        let candidate = current.join(name);
        match tokio::fs::symlink_metadata(&candidate).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("runner path contains a symlink");
            }
            Ok(metadata) if !metadata.is_dir() => bail!("runner path is not a directory"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&candidate).await?;
                if for_job {
                    cli::share_with_jobs(&candidate, 0o2770)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
        current = tokio::fs::canonicalize(&candidate).await?;
        if !current.starts_with(&root) {
            bail!("runner path escapes its root");
        }
    }
    Ok(current)
}

fn validated_relative_or_empty(path: &Path) -> Result<&Path> {
    if path.as_os_str().is_empty() {
        return Ok(path);
    }
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid runner path");
    }
    Ok(path)
}

async fn canonical_contained(root: &Path, target: &Path) -> Result<PathBuf> {
    let root = tokio::fs::canonicalize(root).await?;
    let target = tokio::fs::canonicalize(target).await?;
    if !target.starts_with(root) {
        bail!("workspace path escapes runner root");
    }
    Ok(target)
}

fn capabilities() -> Value {
    json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "backends": {
            "codex": crate::model_catalog::command_exists("codex"),
            "claude": crate::claude::detect::installed(),
            "cursor": crate::model_catalog::command_exists("cursor-agent"),
            "opencode": loka_shared::backends::opencode_enabled() && crate::model_catalog::command_exists("opencode"),
            "omp": crate::model_catalog::command_exists("omp")
        },
        "browser": crate::browser::available(),
        "screen": crate::screen::installed(),
        "apps": crate::apps::installed(),
        "versions": { "claude": crate::claude::detect::version(), "runner": env!("WORLD_BUILD") },
        "terminal": true,
        "setup_scan": cfg!(target_os = "macos"),
        "vault_refs": cfg!(target_os = "macos"),
        "quick": true,
        "github_feed": github_feed::capability(),
        "provider_keys": loka_shared::providers::server_key_names(),
        "folders": shared_folders_view(),
        "slack_capture": slack_capture_view(),
        "models": crate::model_catalog::snapshot(),
        "tools": crate::tools_check::snapshot(),
        "workspaces": workspaces::view()
    })
}

#[derive(Debug, Clone, Deserialize)]
struct SharedFolder {
    id: Uuid,
    path: PathBuf,
    name: String,
}

fn read_shared_folders(root: &Path) -> Vec<SharedFolder> {
    std::fs::read(root.join(FOLDERS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn slack_capture_view() -> Value {
    let Ok(root) = std::env::var("WORLD_RUNNER_ROOT") else {
        return Value::Null;
    };
    slack_capture_from(Path::new(&root))
}

fn slack_capture_from(root: &Path) -> Value {
    let Some(status) = std::fs::read(root.join("slack-capture.json")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) else {
        return Value::Null;
    };
    let text = |v: &Value| v.as_str().map(|s| s.chars().take(300).collect::<String>());
    json!({
        "enabled": status["enabled"].as_bool().unwrap_or(false),
        "running": status["running"].as_bool().unwrap_or(false),
        "workspaces": status["workspaces"].as_array().map(|list| list.iter().filter_map(text).take(10).collect::<Vec<_>>()).unwrap_or_default(),
        "error": text(&status["error"]),
    })
}

fn shared_folders_view() -> Value {
    let Ok(root) = std::env::var("WORLD_RUNNER_ROOT") else {
        return json!([]);
    };
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    let folders: Vec<Value> = read_shared_folders(Path::new(&root))
        .into_iter()
        .map(|f| {
            let full = f.path.to_string_lossy().to_string();
            let shown = match home.as_deref().and_then(|h| full.strip_prefix(h)) {
                Some(rest) => format!("~{rest}"),
                None => full,
            };
            json!({ "id": f.id, "name": f.name, "path": shown })
        })
        .collect();
    json!(folders)
}

fn shared_folder(root: &Path, key: &str) -> Result<PathBuf> {
    let id = key
        .strip_prefix(loka_shared::world::DIRECT_PREFIX)
        .and_then(|rest| rest.parse::<Uuid>().ok())
        .ok_or_else(|| anyhow!("invalid folder session key"))?;
    let folder = read_shared_folders(root)
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| anyhow!("this folder is no longer shared on this Mac; add it again in the Loka app"))?;
    let path = std::fs::canonicalize(&folder.path)
        .map_err(|_| anyhow!("the folder {} is missing on this Mac", folder.path.display()))?;
    if !path.is_dir() || path.parent().is_none() {
        bail!("the shared folder is not a directory");
    }
    let runner = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if path.starts_with(&runner) || runner.starts_with(&path) {
        bail!("the runner's own folder cannot be shared");
    }
    Ok(path)
}

fn compose_project_name(workspace_key: &str) -> String {
    let name: String = workspace_key
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '_' | '-') => c,
            _ => '-',
        })
        .collect();
    format!("world-{}", name.trim_matches('-'))
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}

fn ensure_success(status: StatusCode, operation: &str) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        bail!("{operation} returned {status}")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

async fn reconnect_delay() {
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis() as u64
        % 3_000;
    tokio::time::sleep(Duration::from_millis(2_000 + jitter)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_pr_is_kept_when_nothing_is_ahead() {
        let publish = |url: Option<&str>, number: Option<i64>| RunnerPublish {
            repo: "o/r".into(),
            branch: "fix-x-dcec4a9d".into(),
            base_branch: "main".into(),
            commit_message: "m".into(),
            pr_title: "t".into(),
            pr_body: "b".into(),
            pr_url: url.map(str::to_string),
            pr_number: number,
            stack_onto_pr: None,
        };
        let recorded = publish(Some("https://github.com/o/r/pull/94"), Some(94));
        assert_eq!(kept_pr(&recorded, false), Some(("https://github.com/o/r/pull/94".to_string(), 94)));
        assert_eq!(kept_pr(&recorded, true), None);
        assert_eq!(kept_pr(&publish(None, None), false), None);
        assert_eq!(kept_pr(&publish(None, Some(94)), false), None);
        assert_eq!(kept_pr(&publish(Some(" "), Some(94)), false), None);
    }

    #[test]
    fn slack_capture_status_comes_from_the_mac_app() {
        let root = std::env::temp_dir().join(format!("loka-slack-status-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(slack_capture_from(&root), Value::Null);
        std::fs::write(root.join("slack-capture.json"), r#"{"enabled":true,"running":false,"workspaces":["Acme"],"error":"Open the Slack desktop app on this Mac and sign in."}"#).unwrap();
        let view = slack_capture_from(&root);
        assert_eq!(view["enabled"], true);
        assert_eq!(view["workspaces"], json!(["Acme"]));
        assert_eq!(view["error"], "Open the Slack desktop app on this Mac and sign in.");
        std::fs::write(root.join("slack-capture.json"), "not json").unwrap();
        assert_eq!(slack_capture_from(&root), Value::Null);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slack_sender_is_added_or_refreshed_with_the_current_key() {
        let mut fresh = serde_json::Map::new();
        slack_servers(&mut fresh, "/app/slack_mcp.py", "new-key");
        assert_eq!(fresh["slack"]["env"]["WORLD_SLACK_CONNECTION_KEY"], "new-key");
        assert_eq!(fresh["slack"]["args"], json!(["-E", "-s", "/app/slack_mcp.py"]));

        let mut installed = serde_json::Map::new();
        installed.insert(
            "my-slack".into(),
            json!({"command": "python3", "args": ["/old/slack_mcp.py"], "env": {"WORLD_SLACK_CONNECTION_KEY": "stale", "WORLD_SLACK_TEAM_ID": "T1"}}),
        );
        slack_servers(&mut installed, "/app/slack_mcp.py", "new-key");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed["my-slack"]["env"]["WORLD_SLACK_CONNECTION_KEY"], "new-key");
        assert_eq!(installed["my-slack"]["env"]["WORLD_SLACK_TEAM_ID"], "T1");
        assert_eq!(installed["my-slack"]["args"], json!(["-E", "-s", "/app/slack_mcp.py"]));
    }

    #[test]
    fn offline_tool_calls_get_an_answer_instead_of_a_closed_socket() {
        let call = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "post_message" } });
        let reply = offline_reply(&call).unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["isError"], true);
        let list = offline_reply(&json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" })).unwrap();
        assert_eq!(list["error"]["code"], -32000);
        assert!(offline_reply(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    }

    #[tokio::test]
    async fn requests_wait_for_a_restarting_api() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
        });
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/events");
        let started = Instant::now();
        let response = send_json_through_restart(|| client.post(&url), &json!({ "events": [] })).await.unwrap();
        assert!(response.status().is_success());
        assert!(started.elapsed() >= Duration::from_millis(1000));
        server.await.unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let error = send_json(|| client.post(format!("http://127.0.0.1:{closed}/events")), &json!({})).await.unwrap_err();
        assert!(refused(&error));
    }

    #[tokio::test]
    async fn mcp_leases_only_serve_their_own_process_tree() {
        let child = tokio::process::Command::new("sleep").arg("5").kill_on_drop(true).spawn().unwrap();
        let pid = child.id().unwrap();
        let lease_id = Uuid::new_v4();
        let me = std::process::id();
        let pids = Arc::new(Mutex::new(vec![(me, cli::process_start(me))]));
        MCP_LEASES.lock().unwrap().insert(lease_id, McpLease { token: "lease-token".into(), pids, roots: vec![PathBuf::from("/work")] });
        assert_eq!(lease_for_process(pid).await, Some((lease_id, "lease-token".to_string(), vec![PathBuf::from("/work")])));
        assert_eq!(lease_for_process(1).await, None);
        drop(OpenMcpLease(lease_id));
        assert_eq!(lease_for_process(pid).await, None);
        let reused = Uuid::new_v4();
        let stale = Arc::new(Mutex::new(vec![(me, cli::process_start(me).map(|start| start + 1))]));
        MCP_LEASES.lock().unwrap().insert(reused, McpLease { token: "old-token".into(), pids: stale.clone(), roots: Vec::new() });
        assert_eq!(lease_for_process(pid).await, None, "a recycled pid never inherits an exited job's lease");
        assert!(live_pids(&stale).is_empty());
        drop(OpenMcpLease(reused));
    }

    #[test]
    fn workspace_keys_cannot_escape_the_root() {
        for invalid in [
            "",
            "/tmp/task",
            "../task",
            "tasks/../../outside",
            "tasks/./one",
        ] {
            assert!(validated_relative_path(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            validated_relative_path("agents/6dd88a/tasks/42").unwrap(),
            PathBuf::from("agents/6dd88a/tasks/42")
        );
    }

    #[test]
    fn remote_environment_rejects_control_plane_secrets() {
        for name in [
            "REDIS_URL",
            "AUTH_SECRET",
            "WORLD_RUNNER_TOKEN",
            "GITHUB_TOKEN",
        ] {
            let environment = BTreeMap::from([(name.to_string(), "secret".to_string())]);
            assert!(filtered_environment(&environment, false).is_err(), "{name}");
        }
        let database = BTreeMap::from([("DATABASE_URL".to_string(), "app-db".to_string())]);
        assert_eq!(filtered_environment(&database, false).unwrap(), database);
        let environment = BTreeMap::from([("APP_ENDPOINT".to_string(), "value".to_string())]);
        assert_eq!(
            filtered_environment(&environment, false).unwrap(),
            environment
        );
        let environment =
            BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/tmp/other".to_string())]);
        assert!(filtered_environment(&environment, true).is_err());
    }

    #[test]
    fn github_repository_and_ref_validation_is_narrow() {
        assert!(validate_git_ref("refs/heads/main..bad").is_err());
    }

    #[test]
    fn mcp_credentials_are_redacted_from_activity() {
        let definition = json!({
            "github": {
                "command": "example",
                "env": { "ACCESS_TOKEN": "environment-secret" },
                "headers": { "X-Api-Key": "header-secret" },
                "url": "https://example.test/mcp?token=url-secret"
            }
        });
        let mut secrets = Vec::new();
        collect_json_strings(&definition, &mut secrets);
        let text = redact(
            "environment-secret header-secret https://example.test/mcp?token=url-secret",
            &secrets,
        );
        assert_eq!(text, "[REDACTED] [REDACTED] [REDACTED]");
    }

    #[test]
    fn short_vault_values_are_not_redacted_everywhere() {
        let resolved = BTreeMap::from([
            ("PIN".to_string(), "42".to_string()),
            ("API_KEY".to_string(), "vault-secret".to_string()),
        ]);
        let values = redactable(resolved.values()).collect::<Vec<_>>();
        assert_eq!(values, vec!["vault-secret".to_string()]);
        assert_eq!(redact("build 42 vault-secret", &values), "build 42 [REDACTED]");
    }

    #[test]
    fn turns_tasks_and_owner_asides_receive_world_tools() {
        for kind in ["turn", "task", "followup"] {
            assert!(world_tools_for(kind, false), "{kind}");
        }
        assert!(world_tools_for("aside", true));
        assert!(!world_tools_for("aside", false));
        assert!(world_tools_for("quick", false));
        for kind in ["review", "publish"] {
            assert!(!world_tools_for(kind, true), "{kind}");
        }
    }

    #[test]
    fn failed_runs_only_report_observed_or_existing_session_ids() {
        assert_eq!(
            select_session_id(Some("first".into()), None, false, false).as_deref(),
            Some("first")
        );
        assert_eq!(
            select_session_id(Some("fork".into()), Some("parent"), true, true).as_deref(),
            Some("fork")
        );
        assert_eq!(select_session_id(None, Some("fresh"), false, false), None);
        assert_eq!(
            select_session_id(None, Some("existing"), true, false).as_deref(),
            Some("existing")
        );
        assert_eq!(select_session_id(None, Some("parent"), true, true), None);
    }

    fn quest_job(kind: &str) -> RunnerJob {
        serde_json::from_value(json!({
            "lease_id": Uuid::new_v4(), "run_id": Uuid::new_v4(), "session_id": Uuid::new_v4(),
            "agent_id": Uuid::new_v4(), "runner_id": Uuid::new_v4(), "kind": kind, "cli": "claude",
            "model": "opus", "prompt": "p", "system_prompt": null,
            "workspace": {"key": "tasks/q/world", "repo": null, "branch": null, "base_branch": null, "create": false},
            "cli_session_id": null, "resume": false, "fork": false, "lean": false, "max_turns": 0,
            "timeout_secs": 60, "effort": null, "fast": false, "mcp_token": "t", "publish": null, "omp_models": null,
        }))
        .unwrap()
    }

    #[test]
    fn quest_jobs_with_a_browser_or_screen_launcher_carry_the_gate_root() {
        let root = Path::new("/runner");
        for kind in ["task", "followup"] {
            assert!(crate::browser::wanted(kind) && crate::screen::wanted(kind), "{kind} must get the browser and screen");
            let job = quest_job(kind);
            let mut screen_only = BTreeMap::from([(crate::browser::ROOT_ENV.to_string(), "/spoofed".to_string())]);
            add_gate_environment(&mut screen_only, root, &job, false, Some("/x/loka-agent screen a".into()), None);
            assert_eq!(screen_only[crate::browser::ROOT_ENV], "/runner");
            assert_eq!(screen_only[crate::screen::LAUNCHER_ENV], "/x/loka-agent screen a");
            let mut browser_only = BTreeMap::new();
            add_gate_environment(&mut browser_only, root, &job, true, None, None);
            assert_eq!(browser_only[crate::browser::ROOT_ENV], "/runner");
            let mut apps_only = BTreeMap::new();
            add_gate_environment(&mut apps_only, root, &job, false, None, Some("/x/loka-agent apps a".into()));
            assert_eq!(apps_only[crate::browser::ROOT_ENV], "/runner");
            assert_eq!(apps_only[crate::apps::LEASE_ENV], job.lease_id.to_string());
        }
        let mut none = BTreeMap::new();
        add_gate_environment(&mut none, root, &quest_job("task"), false, None, None);
        assert!(none.is_empty());
        assert!(!crate::screen::wanted("turn") && !crate::browser::wanted("turn"));
    }
}
