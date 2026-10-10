pub mod mux;
mod proxy;
pub mod shim;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::cli::{self, JobUid};
use mux::{ClientId, Mux, Out};

pub type Pids = Arc<Mutex<Vec<(u32, Option<u64>)>>>;

pub const TOKEN_HEADER: &str = "x-world-mcp-token";
const MAX_LINE: u64 = 4 * 1024 * 1024;
const OUTSIDE_RUN: i64 = -32001;
const SPAWN_FAILED: i64 = -32002;
const STATELESS: &[&str] = &[
    "@modelcontextprotocol/server-github",
    "@modelcontextprotocol/server-gitlab",
    "@modelcontextprotocol/server-slack",
    "@modelcontextprotocol/server-brave-search",
    "@modelcontextprotocol/server-fetch",
    "@modelcontextprotocol/server-postgres",
    "@modelcontextprotocol/server-memory",
    "@modelcontextprotocol/server-everything",
    "@upstash/context7-mcp",
    "@notionhq/notion-mcp-server",
    "@sentry/mcp-server",
    "@stripe/mcp",
    "@supabase/mcp-server-supabase",
    "@cloudflare/mcp-server-cloudflare",
    "exa-mcp-server",
    "tavily-mcp",
    "firecrawl-mcp",
    "mcp-server-fetch",
    "mcp-grafana",
    "mcp-remote",
    "slack_mcp.py",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Transport {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Http { url: String, headers: BTreeMap<String, String>, sse: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spec {
    pub name: String,
    pub shared: bool,
    pub transport: Transport,
}

impl Spec {
    pub fn from_definition(name: &str, definition: &Value) -> Result<Option<Self>> {
        let object = definition.as_object().ok_or_else(|| anyhow!("MCP server {name} definition must be an object"))?;
        let strings = |field: &str| -> Result<BTreeMap<String, String>> {
            object
                .get(field)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(key, value)| {
                    let value = value.as_str().ok_or_else(|| anyhow!("MCP server {name} {field} values must be strings"))?;
                    Ok((key.clone(), value.to_string()))
                })
                .collect()
        };
        let explicit = object.get("shared").and_then(Value::as_bool);
        if let Some(url) = object.get("url").and_then(Value::as_str) {
            let headers = strings("headers")?;
            if headers.is_empty() {
                return Ok(None);
            }
            let sse = object.get("type").and_then(Value::as_str) == Some("sse") || object.get("transport").and_then(Value::as_str) == Some("sse");
            return Ok(Some(Self {
                name: name.to_string(),
                shared: explicit.unwrap_or(true),
                transport: Transport::Http { url: url.to_string(), headers, sse },
            }));
        }
        let Some(command) = object.get("command").and_then(Value::as_str) else {
            return Ok(None);
        };
        let args = object
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|arg| arg.as_str().map(String::from).ok_or_else(|| anyhow!("MCP server {name} args must be strings")))
            .collect::<Result<Vec<_>>>()?;
        let env = strings("env")?;
        for key in env.keys() {
            if !cli::valid_environment_name(key) || key == "WORLD_SECRETS_KEY" {
                bail!("invalid MCP environment key {key}");
            }
        }
        let shared = explicit.unwrap_or_else(|| known_stateless(command, &args));
        Ok(Some(Self { name: name.to_string(), shared, transport: Transport::Stdio { command: command.to_string(), args, env } }))
    }

    fn keyed(&self) -> bool {
        match &self.transport {
            Transport::Stdio { env, .. } => !env.is_empty(),
            Transport::Http { headers, .. } => !headers.is_empty(),
        }
    }

    fn secrets(&self) -> Vec<String> {
        let values: Vec<&String> = match &self.transport {
            Transport::Stdio { env, .. } => env.values().collect(),
            Transport::Http { headers, .. } => headers.values().collect(),
        };
        let mut out = Vec::new();
        for value in values {
            if value.len() >= 4 {
                out.push(value.clone());
            }
            out.extend(value.split_whitespace().filter(|part| part.len() >= 8 && part.len() < value.len()).map(String::from));
        }
        out
    }

    fn canonical(&self) -> String {
        serde_json::to_string(self).expect("spec serializes")
    }
}

fn without_version(token: &str) -> &str {
    match token.rfind('@') {
        Some(at) if at > 0 => &token[..at],
        _ => token,
    }
}

pub fn known_stateless(command: &str, args: &[String]) -> bool {
    std::iter::once(command).chain(args.iter().map(String::as_str)).any(|token| {
        let bare = without_version(token);
        let base = without_version(token.rsplit('/').next().unwrap_or(token));
        STATELESS.iter().any(|known| *known == bare || *known == base)
    })
}

pub fn instance_key(isolation: &str, spec: &Spec, client: Option<ClientId>) -> String {
    match client.filter(|_| !spec.shared) {
        Some(client) => format!("{isolation}\u{0}conn:{client}\u{0}{}", spec.canonical()),
        None => format!("{isolation}\u{0}shared\u{0}{}", spec.canonical()),
    }
}

pub fn isolation_for(owner: Option<Uuid>, agent: Uuid) -> String {
    match owner {
        Some(owner) => format!("owner:{owner}"),
        None => format!("agent:{agent}"),
    }
}

struct Reg {
    isolation: String,
    agent: Uuid,
    pids: Pids,
    cwd: PathBuf,
    uid: Option<JobUid>,
    specs: HashMap<String, Arc<Spec>>,
    revoked: bool,
}

impl Reg {
    fn spec(&self, name: &str) -> Option<Arc<Spec>> {
        self.specs.get(name).filter(|spec| !(self.revoked && spec.keyed())).cloned()
    }
}

struct Instance {
    key: String,
    spec: Arc<Spec>,
    mux: Mutex<Mux>,
    to_server: mpsc::UnboundedSender<Value>,
    clients: Mutex<HashMap<ClientId, mpsc::UnboundedSender<Value>>>,
    pid: Option<u32>,
    alive: AtomicBool,
    regs: Mutex<HashSet<u64>>,
    unreferenced: Mutex<Option<Instant>>,
    _uid: Option<JobUid>,
}

impl Instance {
    fn dispatch(&self, outs: Vec<Out>) {
        let clients = self.clients.lock().unwrap();
        for out in outs {
            match out {
                Out::Server(message) => {
                    let _ = self.to_server.send(message);
                }
                Out::Client(client, message) => {
                    if let Some(sender) = clients.get(&client) {
                        let _ = sender.send(message);
                    }
                }
            }
        }
    }

    fn attach(&self, client: ClientId, sender: mpsc::UnboundedSender<Value>, init: &Value) {
        self.clients.lock().unwrap().insert(client, sender);
        let outs = self.mux.lock().unwrap().attach(client, init);
        self.dispatch(outs);
    }

    fn detach(&self, client: ClientId) {
        let outs = self.mux.lock().unwrap().detach(client);
        self.dispatch(outs);
        self.clients.lock().unwrap().remove(&client);
    }

    fn send(&self, client: ClientId, message: Value) {
        let outs = self.mux.lock().unwrap().on_client(client, message);
        self.dispatch(outs);
    }

    async fn stop(&self, reason: &str) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        let outs = self.mux.lock().unwrap().fail_all(reason);
        self.dispatch(outs);
        if let Some(pid) = self.pid {
            cli::kill_process_trees(vec![pid]).await;
        }
        info!(server = %self.spec.name, shared = self.spec.shared, reason, "Stopped MCP server");
    }
}

#[derive(Default)]
struct State {
    regs: HashMap<u64, Reg>,
    instances: HashMap<String, Arc<Instance>>,
    tokens: HashMap<String, (String, Uuid, String)>,
    token_of: HashMap<(String, Uuid, String), String>,
    last_keys: HashMap<(String, Uuid, String), String>,
}

pub struct Hub {
    state: tokio::sync::Mutex<State>,
    socket: PathBuf,
    http: std::net::SocketAddr,
    exe: String,
    next: AtomicU64,
    idle: Duration,
    neutral_dir: PathBuf,
}

static HUB: OnceLock<&'static Hub> = OnceLock::new();

pub fn get() -> Option<&'static Hub> {
    HUB.get().copied()
}

impl Hub {
    pub(crate) fn socket(&self) -> &std::path::Path {
        &self.socket
    }
}

pub fn enabled() -> bool {
    !matches!(std::env::var("WORLD_MCP_HUB").map(|v| v.trim().to_ascii_lowercase()).as_deref(), Ok("off" | "0" | "false" | "no"))
}

pub async fn start(socket: PathBuf) -> Result<&'static Hub> {
    if let Some(hub) = get() {
        return Ok(hub);
    }
    let _ = tokio::fs::remove_file(&socket).await;
    let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("could not open the MCP hub socket {}", socket.display()))?;
    cli::share_with_jobs(&socket, 0o666)?;
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.context("could not open the MCP hub proxy")?;
    let idle = std::env::var("WORLD_MCP_HUB_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60u64);
    let hub: &'static Hub = Box::leak(Box::new(Hub {
        state: tokio::sync::Mutex::new(State::default()),
        socket,
        http: tcp.local_addr()?,
        exe: std::env::current_exe()?.to_string_lossy().to_string(),
        next: AtomicU64::new(1),
        idle: Duration::from_secs(idle),
        neutral_dir: std::env::temp_dir(),
    }));
    if HUB.set(hub).is_err() {
        return Ok(get().expect("hub set"));
    }
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(async move {
                        if let Err(error) = hub.serve_shim(stream).await {
                            debug!(%error, "MCP hub client ended");
                        }
                    });
                }
                Err(error) => {
                    warn!(%error, "MCP hub accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    tokio::spawn(proxy::serve(hub, tcp));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            hub.reap().await;
        }
    });
    let swept = cli::sweep_stale_run_files(Duration::from_secs(24 * 3600));
    info!(socket = %hub.socket.display(), proxy = %hub.http, swept, "MCP hub started");
    Ok(hub)
}

pub struct Job<'a> {
    pub isolation: String,
    pub agent: Uuid,
    pub cwd: &'a Path,
    pub pids: Pids,
    pub passthrough: &'a [&'a str],
}

pub struct Registration {
    hub: &'static Hub,
    id: u64,
    secrets: Vec<String>,
}

impl Registration {
    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }

    pub fn revoker(&self) -> Revoker {
        Revoker { hub: self.hub, id: self.id }
    }
}

pub struct Revoker {
    hub: &'static Hub,
    id: u64,
}

impl Revoker {
    pub async fn revoke(&self) {
        self.hub.revoke(self.id).await;
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let hub = self.hub;
        let id = self.id;
        if let Ok(mut state) = hub.state.try_lock() {
            Hub::forget(&mut state, id);
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                Hub::forget(&mut *hub.state.lock().await, id);
            });
        }
    }
}

pub struct Prepared {
    pub servers: Map<String, Value>,
    pub registration: Option<Registration>,
}

fn carried(definition: &Value, drop: &[&str]) -> Map<String, Value> {
    definition
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !drop.contains(&key.as_str()) && key.as_str() != "shared")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub fn strip_shared(servers: &mut Map<String, Value>) {
    for definition in servers.values_mut() {
        if let Some(object) = definition.as_object_mut() {
            object.remove("shared");
        }
    }
}

impl Hub {
    pub async fn prepare(&'static self, job: Job<'_>, servers: &Map<String, Value>) -> Result<Prepared> {
        let mut out = Map::new();
        let mut specs = HashMap::new();
        let mut state = self.state.lock().await;
        for (name, definition) in servers {
            let spec = if job.passthrough.contains(&name.as_str()) { None } else { Spec::from_definition(name, definition)? };
            let Some(spec) = spec else {
                let mut kept = definition.clone();
                if let Some(object) = kept.as_object_mut() {
                    object.remove("shared");
                }
                out.insert(name.clone(), kept);
                continue;
            };
            let rewritten = match &spec.transport {
                Transport::Stdio { .. } => {
                    let mut entry = carried(definition, &["command", "args", "env", "cwd", "type"]);
                    entry.insert("command".into(), json!(self.exe));
                    entry.insert("args".into(), json!(["mcp-shim", "--hub", self.socket.to_string_lossy(), name]));
                    Value::Object(entry)
                }
                Transport::Http { .. } => {
                    let token = state
                        .token_of
                        .entry((job.isolation.clone(), job.agent, name.clone()))
                        .or_insert_with(|| format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
                        .clone();
                    state.tokens.insert(token.clone(), (job.isolation.clone(), job.agent, name.clone()));
                    let mut entry = carried(definition, &["url", "headers"]);
                    entry.insert("url".into(), json!(format!("http://{}/mcp/{}", self.http, urlencode(name))));
                    entry.insert("headers".into(), json!({ TOKEN_HEADER: token }));
                    Value::Object(entry)
                }
            };
            out.insert(name.clone(), rewritten);
            specs.insert(name.clone(), Arc::new(spec));
        }
        if specs.is_empty() {
            return Ok(Prepared { servers: out, registration: None });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let secrets = specs.values().flat_map(|spec| spec.secrets()).collect();
        let mut stale = Vec::new();
        for (name, spec) in &specs {
            if !spec.shared {
                continue;
            }
            let key = instance_key(&job.isolation, spec, None);
            if let Some(old) = state.last_keys.insert((job.isolation.clone(), job.agent, name.clone()), key.clone()) {
                if old != key {
                    stale.push(old);
                }
            }
            if let Some(instance) = state.instances.get(&key) {
                instance.regs.lock().unwrap().insert(id);
                *instance.unreferenced.lock().unwrap() = None;
            }
        }
        state.regs.insert(
            id,
            Reg {
                isolation: job.isolation,
                agent: job.agent,
                pids: job.pids,
                cwd: job.cwd.to_path_buf(),
                uid: JobUid::current(),
                specs,
                revoked: false,
            },
        );
        let mut doomed = Vec::new();
        for key in stale {
            if !Self::referenced(&state, &key) {
                if let Some(instance) = state.instances.remove(&key) {
                    doomed.push(instance);
                }
            }
        }
        drop(state);
        for instance in doomed {
            instance.stop("its key changed").await;
        }
        Ok(Prepared { servers: out, registration: Some(Registration { hub: self, id, secrets }) })
    }

    fn referenced(state: &State, key: &str) -> bool {
        state.regs.values().any(|reg| {
            reg.specs
                .values()
                .any(|spec| spec.shared && !(reg.revoked && spec.keyed()) && instance_key(&reg.isolation, spec, None) == key)
        }) || state
            .instances
            .get(key)
            .is_some_and(|instance| instance.regs.lock().unwrap().iter().any(|id| state.regs.get(id).is_some_and(|reg| !(reg.revoked && instance.spec.keyed()))))
    }

    fn forget(state: &mut State, id: u64) {
        state.regs.remove(&id);
        let now = Instant::now();
        let keys: Vec<String> = state.instances.keys().cloned().collect();
        for key in keys {
            if !Self::referenced(state, &key) {
                let instance = &state.instances[&key];
                instance.unreferenced.lock().unwrap().get_or_insert(now);
            }
        }
    }

    async fn revoke(&self, id: u64) {
        let mut state = self.state.lock().await;
        let Some(reg) = state.regs.get_mut(&id) else {
            return;
        };
        if reg.revoked {
            return;
        }
        reg.revoked = true;
        let keys: Vec<String> = state.instances.keys().cloned().collect();
        let mut doomed = Vec::new();
        for key in keys {
            let keyed = state.instances[&key].spec.keyed();
            if keyed && !Self::referenced(&state, &key) {
                doomed.push(state.instances.remove(&key).expect("listed"));
            }
        }
        drop(state);
        for instance in doomed {
            instance.stop("a key it used was revoked").await;
        }
    }

    async fn reap(&self) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let mut doomed = Vec::new();
        let keys: Vec<String> = state.instances.keys().cloned().collect();
        for key in keys {
            let instance = state.instances[&key].clone();
            if !instance.alive.load(Ordering::SeqCst) {
                state.instances.remove(&key);
                continue;
            }
            if Self::referenced(&state, &key) {
                *instance.unreferenced.lock().unwrap() = None;
                continue;
            }
            let since = *instance.unreferenced.lock().unwrap().get_or_insert(now);
            if now.duration_since(since) >= self.idle {
                state.instances.remove(&key);
                doomed.push(instance);
            }
        }
        drop(state);
        for instance in doomed {
            instance.stop("idle").await;
        }
    }

    async fn reg_for_pid(&self, pid: u32) -> Option<u64> {
        let state = self.state.lock().await;
        let mut current = pid;
        for _ in 0..64 {
            let found = state.regs.iter().find(|(_, reg)| {
                reg.pids
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(candidate, started)| *candidate == current && started.is_some() && cli::process_start(current) == *started)
            });
            if let Some((id, _)) = found {
                return Some(*id);
            }
            current = cli::parent_pid(current).filter(|parent| *parent > 1)?;
        }
        None
    }

    pub(crate) async fn http_target(&self, token: &str, name: &str) -> Option<(Arc<Spec>, bool)> {
        let state = self.state.lock().await;
        let (isolation, agent, owned) = state.tokens.get(token)?;
        if owned != name {
            return None;
        }
        let guest = isolation.starts_with("guest:");
        state
            .regs
            .iter()
            .filter(|(_, reg)| reg.isolation == *isolation && reg.agent == *agent)
            .max_by_key(|(id, _)| **id)
            .and_then(|(_, reg)| reg.spec(name))
            .map(|spec| (spec, guest))
    }

    async fn instance_for(&'static self, reg_id: u64, name: &str, client: ClientId) -> Result<Option<Arc<Instance>>> {
        let mut state = self.state.lock().await;
        let Some(reg) = state.regs.get(&reg_id) else {
            return Ok(None);
        };
        let Some(spec) = reg.spec(name) else {
            return Ok(None);
        };
        if !matches!(spec.transport, Transport::Stdio { .. }) {
            return Ok(None);
        }
        let mut shared = spec.shared;
        let key = instance_key(&reg.isolation, &spec, Some(client));
        if let Some(instance) = state.instances.get(&key).filter(|i| i.alive.load(Ordering::SeqCst)) {
            instance.regs.lock().unwrap().insert(reg_id);
            *instance.unreferenced.lock().unwrap() = None;
            return Ok(Some(instance.clone()));
        }
        let (cwd, reg_uid) = (reg.cwd.clone(), reg.uid.clone());
        let uid = if shared && cli::job_user().is_some() {
            match JobUid::take().await? {
                Some(uid) => Some(uid),
                None => {
                    shared = false;
                    reg_uid.clone()
                }
            }
        } else if shared {
            None
        } else {
            reg_uid.clone()
        };
        let spec = if shared == spec.shared { spec } else { Arc::new(Spec { shared, ..(*spec).clone() }) };
        let key = instance_key(&state.regs[&reg_id].isolation, &spec, Some(client));
        let dir = if shared { self.neutral_dir.clone() } else { cwd };
        let instance = spawn(key.clone(), spec, &dir, uid).await?;
        instance.regs.lock().unwrap().insert(reg_id);
        state.instances.insert(key, instance.clone());
        Ok(Some(instance))
    }

    async fn serve_shim(&'static self, stream: tokio::net::UnixStream) -> Result<()> {
        let pid = stream.peer_cred()?.pid().context("MCP hub peer has no pid")? as u32;
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader);
        let mut hello = String::new();
        (&mut lines).take(64 * 1024).read_line(&mut hello).await?;
        let hello: Value = serde_json::from_str(&hello).context("bad MCP hub hello")?;
        let name = hello.get("server").and_then(Value::as_str).context("MCP hub hello has no server")?.to_string();
        let client = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let writer_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let mut line = message.to_string();
                line.push('\n');
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let mut init = json!({});
        let mut bound: Option<Arc<Instance>> = None;
        let mut buffer = String::new();
        let result = loop {
            buffer.clear();
            let read = (&mut lines).take(MAX_LINE).read_line(&mut buffer).await;
            match read {
                Ok(0) => break Ok(()),
                Ok(_) if !buffer.ends_with('\n') && buffer.len() as u64 >= MAX_LINE => break Err(anyhow!("MCP message larger than {MAX_LINE} bytes")),
                Ok(_) => {}
                Err(error) => break Err(error.into()),
            }
            let Ok(message) = serde_json::from_str::<Value>(buffer.trim()) else {
                continue;
            };
            if message.get("method").and_then(Value::as_str) == Some("initialize") {
                init = message.get("params").cloned().unwrap_or_default();
            }
            let request_id = message.get("method").and(message.get("id")).filter(|id| !id.is_null()).cloned();
            let instance = match self.reg_for_pid(pid).await {
                Some(reg) => self.instance_for(reg, &name, client).await,
                None => Ok(None),
            };
            let instance = match instance {
                Ok(Some(instance)) => instance,
                Ok(None) => {
                    if let Some(id) = request_id {
                        let _ = tx.send(mux::error(id, OUTSIDE_RUN, "This MCP server is only available while the agent is working on a run"));
                    }
                    continue;
                }
                Err(error) => {
                    warn!(server = %name, %error, "Could not start MCP server");
                    if let Some(id) = request_id {
                        let _ = tx.send(mux::error(id, SPAWN_FAILED, &format!("Could not start MCP server {name}")));
                    }
                    continue;
                }
            };
            if !bound.as_ref().is_some_and(|current| Arc::ptr_eq(current, &instance)) {
                if let Some(old) = bound.take() {
                    old.detach(client);
                    if !old.spec.shared {
                        self.retire(&old).await;
                    }
                }
                instance.attach(client, tx.clone(), &init);
                bound = Some(instance.clone());
            }
            instance.send(client, message);
        };
        if let Some(old) = bound {
            old.detach(client);
            if !old.spec.shared {
                self.retire(&old).await;
            }
        }
        writer_task.abort();
        result
    }

    async fn retire(&self, instance: &Arc<Instance>) {
        let mut state = self.state.lock().await;
        if state.instances.get(&instance.key).is_some_and(|current| Arc::ptr_eq(current, instance)) {
            state.instances.remove(&instance.key);
        }
        drop(state);
        instance.stop("its session ended").await;
    }

    #[cfg(test)]
    async fn instance_pids(&self) -> Vec<u32> {
        self.state.lock().await.instances.values().filter(|i| i.alive.load(Ordering::SeqCst)).filter_map(|i| i.pid).collect()
    }

    #[cfg(test)]
    async fn instance_count(&self) -> usize {
        self.state.lock().await.instances.values().filter(|i| i.alive.load(Ordering::SeqCst)).count()
    }
}

fn redacted(text: &str, spec: &Spec) -> String {
    spec.secrets().iter().fold(text.to_string(), |text, secret| text.replace(secret.as_str(), "[REDACTED]"))
}

fn urlencode(name: &str) -> String {
    name.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

async fn spawn(key: String, spec: Arc<Spec>, dir: &Path, uid: Option<JobUid>) -> Result<Arc<Instance>> {
    let Transport::Stdio { command, args, env } = &spec.transport else {
        bail!("only command servers run in the hub");
    };
    let secrets = spec.secrets();
    let (command, args, env, dir) = (command.clone(), args.clone(), env.clone(), dir.to_path_buf());
    let mut child = JobUid::scope(uid.clone(), async move {
        let mut cmd = cli::job_command(&command);
        cmd.args(&args);
        cli::configure_child_environment(&mut cmd, &BTreeMap::new())?;
        cmd.env_remove("WORLD_SECRETS_KEY");
        for (name, value) in &env {
            cmd.env(name, value);
        }
        let dir = if dir.is_dir() { dir } else { cli::temp_dir() };
        cmd.current_dir(dir);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.spawn().with_context(|| format!("could not start {command}"))
    })
    .await?;
    let pid = child.id();
    let mut stdin = child.stdin.take().context("MCP server has no stdin")?;
    let stdout = child.stdout.take().context("MCP server has no stdout")?;
    let stderr = child.stderr.take();
    let (to_server, mut from_hub) = mpsc::unbounded_channel::<Value>();
    let instance = Arc::new(Instance {
        key,
        spec: spec.clone(),
        mux: Mutex::new(Mux::new(spec.shared)),
        to_server,
        clients: Mutex::new(HashMap::new()),
        pid,
        alive: AtomicBool::new(true),
        regs: Mutex::new(HashSet::new()),
        unreferenced: Mutex::new(None),
        _uid: uid,
    });
    info!(server = %spec.name, shared = spec.shared, pid, "Started MCP server");
    tokio::spawn(async move {
        while let Some(message) = from_hub.recv().await {
            let mut line = message.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    if let Some(stderr) = stderr {
        let name = spec.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = secrets.iter().fold(line, |line, secret| line.replace(secret.as_str(), "[REDACTED]"));
                debug!(server = %name, line = %crate::cli::truncate_for_log(&line, 500), "MCP server stderr");
            }
        });
    }
    let reader = instance.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout);
        let mut buffer = String::new();
        loop {
            buffer.clear();
            match (&mut lines).take(MAX_LINE).read_line(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(message) = serde_json::from_str::<Value>(buffer.trim()) else {
                continue;
            };
            let outs = reader.mux.lock().unwrap().on_server(message);
            reader.dispatch(outs);
        }
        let _ = child.wait().await;
        if reader.alive.swap(false, Ordering::SeqCst) {
            warn!(server = %reader.spec.name, "MCP server exited");
            let outs = reader.mux.lock().unwrap().fail_all("The MCP server restarted; try the call again");
            reader.dispatch(outs);
        }
    });
    Ok(instance)
}

#[cfg(test)]
mod tests;
