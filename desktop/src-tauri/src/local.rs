use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};

const LOG_ROTATE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const STABLE_AFTER: Duration = Duration::from_secs(60);
pub const DATABASE_USER: &str = "loka";
pub const DATABASE_NAME: &str = "postgres";
pub const API_PORT: u16 = 47811;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Ports {
    pub database: u16,
    pub api: u16,
    pub web: u16,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Secrets {
    pub database_password: String,
    pub auth_secret: String,
    pub secrets_key: String,
    #[serde(default)]
    pub runner_token: Option<String>,
    #[serde(default)]
    pub ports: Ports,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Starting(&'static str),
    Ready,
    Failed { message: String, log: PathBuf },
}

pub struct Supervised {
    pub name: &'static str,
    log: PathBuf,
    pid_file: PathBuf,
    make: Box<dyn Fn() -> Result<Command, String> + Send + Sync>,
    stop_signal: libc::c_int,
    group: bool,
    grace: Duration,
    child: Mutex<Option<Child>>,
    wanted: AtomicBool,
    generation: AtomicU64,
    exits: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Supervised {
    pub fn new(
        name: &'static str,
        log: PathBuf,
        pid_dir: &Path,
        stop_signal: libc::c_int,
        group: bool,
        grace: Duration,
        make: impl Fn() -> Result<Command, String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            log,
            pid_file: pid_dir.join(format!("{name}.pid")),
            make: Box::new(make),
            stop_signal,
            group,
            grace,
            child: Mutex::new(None),
            wanted: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            exits: AtomicU64::new(0),
            last_error: Mutex::new(None),
        })
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|error| error.clone())
    }

    pub fn exits(&self) -> u64 {
        self.exits.load(Ordering::SeqCst)
    }

    pub fn running(&self) -> bool {
        self.child
            .lock()
            .map(|mut slot| slot.as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(None))))
            .unwrap_or(false)
    }

    fn note(&self, message: String) {
        if let Ok(mut file) = append_log(&self.log) {
            let _ = writeln!(file, "[loka] {message}");
        }
        if let Ok(mut error) = self.last_error.lock() {
            *error = Some(message);
        }
    }

    fn spawn(&self) -> Result<Child, String> {
        let mut command = (self.make)()?;
        rotate_log(&self.log);
        let out = append_log(&self.log).map_err(|error| format!("Could not open {}: {error}", self.log.display()))?;
        let err = out.try_clone().map_err(|error| error.to_string())?;
        command.stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("Could not start {}: {error}", self.name))?;
        let _ = write_private(&self.pid_file, child.id().to_string().as_bytes());
        Ok(child)
    }

    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        let mut slot = self.child.lock().map_err(|_| "Service state is unavailable.".to_string())?;
        if let Some(child) = slot.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return Ok(());
            }
        }
        *slot = None;
        self.wanted.store(true, Ordering::SeqCst);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        match self.spawn() {
            Ok(child) => {
                *slot = Some(child);
                if let Ok(mut error) = self.last_error.lock() {
                    *error = None;
                }
            }
            Err(error) => {
                drop(slot);
                self.note(error.clone());
                let me = self.clone();
                std::thread::spawn(move || me.watch(generation));
                return Err(error);
            }
        }
        drop(slot);
        let me = self.clone();
        std::thread::spawn(move || me.watch(generation));
        Ok(())
    }

    fn current(&self, generation: u64) -> bool {
        self.wanted.load(Ordering::SeqCst) && self.generation.load(Ordering::SeqCst) == generation
    }

    fn watch(self: Arc<Self>, generation: u64) {
        let mut backoff = Duration::from_secs(1);
        let mut started = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            if !self.current(generation) {
                return;
            }
            let exited = {
                let Ok(mut slot) = self.child.lock() else { return };
                match slot.as_mut() {
                    None => Some("it could not start".to_string()),
                    Some(child) => match child.try_wait() {
                        Ok(None) => None,
                        Ok(Some(status)) => {
                            slot.take();
                            Some(status.to_string())
                        }
                        Err(error) => {
                            slot.take();
                            Some(error.to_string())
                        }
                    },
                }
            };
            let Some(why) = exited else { continue };
            if !self.current(generation) {
                return;
            }
            if started.elapsed() > STABLE_AFTER {
                backoff = Duration::from_secs(1);
            }
            self.note(format!("{} stopped ({why}). Restarting in {} s.", self.name, backoff.as_secs()));
            self.exits.fetch_add(1, Ordering::SeqCst);
            let until = Instant::now() + backoff;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(250));
                if !self.current(generation) {
                    return;
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            let Ok(mut slot) = self.child.lock() else { return };
            if !self.current(generation) || slot.is_some() {
                return;
            }
            match self.spawn() {
                Ok(child) => {
                    *slot = Some(child);
                    started = Instant::now();
                }
                Err(error) => {
                    drop(slot);
                    self.note(error);
                }
            }
        }
    }

    pub fn hand_off(&self, signal: libc::c_int, wait: Duration) {
        self.wanted.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let child = self.child.lock().ok().and_then(|mut slot| slot.take());
        let Some(mut child) = child else {
            return;
        };
        #[cfg(unix)]
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            unsafe {
                libc::kill(pid, signal);
            }
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        terminate(&mut child, self.stop_signal, self.group, self.grace);
        let _ = std::fs::remove_file(&self.pid_file);
    }

    pub fn stop(&self) {
        self.wanted.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let child = self.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child {
            terminate(&mut child, self.stop_signal, self.group, self.grace);
        }
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

pub fn terminate(child: &mut Child, signal: libc::c_int, group: bool, grace: Duration) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        let target = if group { -pid } else { pid };
        unsafe {
            libc::kill(target, signal);
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (signal, group, grace);
        let _ = child.kill();
    }
    let _ = child.wait();
}

pub struct Stack {
    pub root: PathBuf,
    pub logs: PathBuf,
    runtime: PathBuf,
    secrets_path: PathBuf,
    secrets: Mutex<Secrets>,
    pub ports: Ports,
    database: Arc<Supervised>,
    api: Arc<Supervised>,
    web: Arc<Supervised>,
    api_binary: Arc<Mutex<PathBuf>>,
    web_dir: Arc<Mutex<PathBuf>>,
    bundled_api: PathBuf,
    bundled_web: PathBuf,
    phase: Mutex<Phase>,
    busy: AtomicBool,
}

impl Stack {
    pub fn new(root: PathBuf, logs: PathBuf, runtime: PathBuf, sidecars: PathBuf) -> Result<Self, String> {
        ensure_private_dir(&root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&logs).map_err(|error| error.to_string())?;
        let pid_dir = root.join("pids");
        ensure_private_dir(&pid_dir).map_err(|error| error.to_string())?;
        reap_orphans(&pid_dir);
        let secrets_path = root.join("local.json");
        let mut secrets = load_secrets(&secrets_path)?;
        secrets.ports = choose_ports(secrets.ports);
        save_secrets(&secrets_path, &secrets).map_err(|error| error.to_string())?;
        let ports = secrets.ports;
        let data = root.join("postgres");
        let database = {
            let bin = runtime.join("postgres").join("bin");
            let data = data.clone();
            Supervised::new("postgres", logs.join("postgres.log"), &pid_dir, libc::SIGINT, false, Duration::from_secs(20), move || {
                let mut command = Command::new(bin.join("postgres"));
                command
                    .arg("-D")
                    .arg(&data)
                    .args(["-p", &ports.database.to_string(), "-h", "127.0.0.1", "-k", "", "-c", "jit=off", "-c", "max_connections=60"])
                    .env_remove("PGDATA")
                    .env("LC_ALL", "C");
                Ok(command)
            })
        };
        let database_url = format!(
            "postgres://{DATABASE_USER}:{}@127.0.0.1:{}/{DATABASE_NAME}",
            secrets.database_password, ports.database
        );
        let bundled_api = [sidecars.join(sidecar_name("loka-api")), sidecars.join("loka-api")]
            .into_iter()
            .find(|path| path.is_file())
            .unwrap_or_else(|| sidecars.join("loka-api"));
        let bundled_web = runtime.join("web");
        let api_binary = Arc::new(Mutex::new(bundled_api.clone()));
        let web_dir = Arc::new(Mutex::new(bundled_web.clone()));
        let api = {
            let binary = api_binary.clone();
            let secrets = secrets.clone();
            let config = root.join("config.yaml");
            Supervised::new("api", logs.join("api.log"), &pid_dir, libc::SIGTERM, true, Duration::from_secs(5), move || {
                let binary = binary.lock().map_err(|_| "The Loka API path is unavailable.".to_string())?.clone();
                if !binary.is_file() {
                    return Err("The bundled Loka API is missing from this app.".into());
                }
                let mut command = Command::new(&binary);
                command
                    .env("DATABASE_URL", &database_url)
                    .env("PORT", ports.api.to_string())
                    .env("API_BIND_HOST", "127.0.0.1")
                    .env("AUTH_DISABLED", "true")
                    .env("AUTH_SECRET", &secrets.auth_secret)
                    .env("WORLD_SECRETS_KEY", &secrets.secrets_key)
                    .env("WORLD_PUBLIC_API_URL", format!("http://127.0.0.1:{}", ports.api))
                    .env("WORLD_APP_URL", format!("http://127.0.0.1:{}", ports.web))
                    .env("CONFIG_PATH", &config)
                    .env("DATABASE_MAX_CONNECTIONS", "10")
                    .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
                    .env_remove("REDIS_URL")
                    .env_remove("WORLD_SECRETS_KEY_PREVIOUS")
                    .env_remove("GH_TOKEN")
                    .env_remove("GITHUB_TOKEN");
                Ok(command)
            })
        };
        let web = {
            let node = runtime.join("node").join("bin").join("node");
            let dir = web_dir.clone();
            let secrets = secrets.clone();
            Supervised::new("web", logs.join("web.log"), &pid_dir, libc::SIGTERM, true, Duration::from_secs(5), move || {
                let app = dir.lock().map_err(|_| "The Loka web app path is unavailable.".to_string())?.clone();
                if !node.is_file() || !app.join("server.js").is_file() {
                    return Err("The bundled Loka web app is missing from this app.".into());
                }
                let mut command = Command::new(&node);
                command
                    .arg(app.join("server.js"))
                    .current_dir(&app)
                    .env("PORT", ports.web.to_string())
                    .env("HOSTNAME", "127.0.0.1")
                    .env("NODE_ENV", "production")
                    .env("NEXT_TELEMETRY_DISABLED", "1")
                    .env("API_URL", format!("http://127.0.0.1:{}", ports.api))
                    .env("WORLD_PUBLIC_API_URL", format!("http://127.0.0.1:{}", ports.api))
                    .env("AUTH_SECRET", &secrets.auth_secret)
                    .env("AUTH_TRUST_HOST", "true")
                    .env_remove("NODE_OPTIONS")
                    .env_remove("DATABASE_URL")
                    .env_remove("REDIS_URL");
                Ok(command)
            })
        };
        Ok(Self {
            root,
            logs,
            runtime,
            secrets_path,
            secrets: Mutex::new(secrets),
            ports,
            database,
            api,
            web,
            api_binary,
            web_dir,
            bundled_api,
            bundled_web,
            phase: Mutex::new(Phase::Starting("Starting Loka")),
            busy: AtomicBool::new(false),
        })
    }

    pub fn api_origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.ports.api)
    }

    pub fn web_origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.ports.web)
    }

    pub fn phase(&self) -> Phase {
        self.phase.lock().map(|phase| phase.clone()).unwrap_or(Phase::Starting("Starting Loka"))
    }

    fn set_phase(&self, phase: Phase) {
        if let Ok(mut current) = self.phase.lock() {
            *current = phase;
        }
    }

    pub fn runner_token(&self) -> Option<String> {
        self.secrets.lock().ok().and_then(|secrets| secrets.runner_token.clone())
    }

    pub fn set_runner_token(&self, token: Option<String>) -> Result<(), String> {
        let mut secrets = self.secrets.lock().map_err(|_| "Local secrets are unavailable.".to_string())?;
        secrets.runner_token = token;
        save_secrets(&self.secrets_path, &secrets).map_err(|error| error.to_string())
    }

    pub fn log_for(&self, name: &str) -> PathBuf {
        self.logs.join(format!("{name}.log"))
    }

    pub fn begin(&self) -> bool {
        !self.busy.swap(true, Ordering::SeqCst)
    }

    pub fn end(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }

    pub fn fail(&self, service: &str, message: String) -> Phase {
        let phase = Phase::Failed { message, log: self.log_for(service) };
        self.set_phase(phase.clone());
        phase
    }

    pub async fn start_database(&self) -> Result<(), (String, &'static str)> {
        self.set_phase(Phase::Starting("Preparing the database"));
        let bin = self.runtime.join("postgres").join("bin");
        if !bin.join("postgres").is_file() {
            return Err(("The bundled database is missing from this app.".into(), "postgres"));
        }
        let data = self.root.join("postgres");
        let password = self.secrets.lock().map(|s| s.database_password.clone()).unwrap_or_default();
        let log = self.log_for("postgres");
        let root = self.root.clone();
        let init_bin = bin.clone();
        tokio::task::spawn_blocking(move || {
            stop_stale_postmaster(&data);
            ensure_cluster(&init_bin, &root, &data, &password, &log)
        })
        .await
        .map_err(|error| (error.to_string(), "postgres"))?
        .map_err(|error| (error, "postgres"))?;
        self.set_phase(Phase::Starting("Starting the database"));
        self.database.start().map_err(|error| (error, "postgres"))?;
        let ready = bin.join("pg_isready");
        let port = self.ports.database;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            let accepting = tokio::process::Command::new(&ready)
                .args(["-q", "-h", "127.0.0.1", "-p", &port.to_string(), "-d", DATABASE_NAME, "-U", DATABASE_USER])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|status| status.success())
                .unwrap_or_else(|_| TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500)).is_ok());
            if accepting {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err((self.database.last_error().unwrap_or_else(|| "The database did not start in time.".into()), "postgres"))
    }

    pub async fn start_api(&self) -> Result<(), (String, &'static str)> {
        self.set_phase(Phase::Starting("Starting the Loka server"));
        if !self.api.running() && !port_free(self.ports.api) {
            return Err((
                format!("Another program on this Mac is using port {}, which Loka needs for its local server. Quit that program, then choose Reload from the View menu.", self.ports.api),
                "api",
            ));
        }
        self.api.start().map_err(|error| (error, "api"))?;
        let url = format!("{}/ready", self.api_origin());
        wait_http(&url, Duration::from_secs(240))
            .await
            .map_err(|_| (self.api.last_error().unwrap_or_else(|| "The Loka server did not become ready in time.".into()), "api"))
    }

    pub async fn start_web(&self) -> Result<(), (String, &'static str)> {
        self.set_phase(Phase::Starting("Opening Loka"));
        self.web.start().map_err(|error| (error, "web"))?;
        let url = format!("{}/", self.web_origin());
        wait_http(&url, Duration::from_secs(120))
            .await
            .map_err(|_| (self.web.last_error().unwrap_or_else(|| "The Loka web app did not start in time.".into()), "web"))
    }

    pub fn ready(&self) {
        self.set_phase(Phase::Ready);
    }

    pub fn api_binary(&self) -> PathBuf {
        self.api_binary.lock().map(|path| path.clone()).unwrap_or_else(|_| self.bundled_api.clone())
    }

    pub fn web_dir(&self) -> PathBuf {
        self.web_dir.lock().map(|path| path.clone()).unwrap_or_else(|_| self.bundled_web.clone())
    }

    pub fn set_api_binary(&self, path: PathBuf) {
        if let Ok(mut current) = self.api_binary.lock() {
            *current = path;
        }
    }

    pub fn set_web_dir(&self, path: PathBuf) {
        if let Ok(mut current) = self.web_dir.lock() {
            *current = path;
        }
    }

    pub async fn restart_api(&self, limit: Duration) -> Result<(), String> {
        restart(&self.api, &format!("{}/ready", self.api_origin()), limit).await
    }

    pub async fn restart_web(&self, limit: Duration) -> Result<(), String> {
        restart(&self.web, &format!("{}/", self.web_origin()), limit).await
    }

    pub fn stop_all(&self) {
        self.web.stop();
        self.api.stop();
        self.database.stop();
    }

    pub fn pid_dir(&self) -> PathBuf {
        self.root.join("pids")
    }
}

async fn restart(service: &Arc<Supervised>, url: &str, limit: Duration) -> Result<(), String> {
    let stopping = service.clone();
    tokio::task::spawn_blocking(move || stopping.stop()).await.map_err(|error| error.to_string())?;
    let exits = service.exits();
    service.start()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if service.exits() > exits {
            return Err("it stopped right after starting".into());
        }
        if client.get(url).send().await.is_ok_and(|response| response.status().is_success()) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Err(format!("it did not answer within {} s", limit.as_secs()))
}

async fn wait_http(url: &str, limit: Duration) -> Result<(), ()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .map_err(|_| ())?;
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Ok(response) = client.get(url).send().await {
            if response.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Err(())
}

fn ensure_cluster(bin: &Path, root: &Path, data: &Path, password: &str, log: &Path) -> Result<(), String> {
    if data.join("PG_VERSION").is_file() {
        return Ok(());
    }
    let staging = root.join("postgres.init");
    let _ = std::fs::remove_dir_all(&staging);
    let pwfile = root.join("postgres.pw");
    write_private(&pwfile, password.as_bytes()).map_err(|error| error.to_string())?;
    let out = append_log(log).map_err(|error| error.to_string())?;
    let err = out.try_clone().map_err(|error| error.to_string())?;
    let status = Command::new(bin.join("initdb"))
        .arg("-D")
        .arg(&staging)
        .args(["-U", DATABASE_USER, "-A", "scram-sha-256", "-E", "UTF8", "--locale=C", "--no-instructions"])
        .arg(format!("--pwfile={}", pwfile.display()))
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status();
    let _ = std::fs::remove_file(&pwfile);
    let status = status.map_err(|error| format!("Could not prepare the database: {error}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("Preparing the database failed ({status})."));
    }
    let _ = std::fs::remove_dir_all(data);
    std::fs::rename(&staging, data).map_err(|error| format!("Could not move the new database into place: {error}"))
}

fn stop_stale_postmaster(data: &Path) {
    let Ok(text) = std::fs::read_to_string(data.join("postmaster.pid")) else { return };
    let Some(pid) = text.lines().next().and_then(|line| line.trim().parse::<libc::pid_t>().ok()) else { return };
    if pid <= 1 || !process_named(pid, "postgres") {
        return;
    }
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && alive(pid) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn process_named(pid: libc::pid_t, wanted: &str) -> bool {
    Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().rsplit('/').next().unwrap_or_default().starts_with(wanted))
        .unwrap_or(false)
}

fn reap_orphans(pid_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(pid_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default();
        let pid = std::fs::read_to_string(&path).ok().and_then(|text| text.trim().parse::<libc::pid_t>().ok());
        let _ = std::fs::remove_file(&path);
        let Some(pid) = pid.filter(|pid| *pid > 1) else { continue };
        let (binary, signal) = match name.as_str() {
            "postgres" => ("postgres", libc::SIGINT),
            "api" => ("loka-api", libc::SIGTERM),
            "runner" => ("loka-agent", libc::SIGTERM),
            "web" => ("node", libc::SIGTERM),
            _ => continue,
        };
        if !alive(pid) || !process_named(pid, binary) {
            continue;
        }
        let target = if name == "postgres" { pid } else { -pid };
        unsafe {
            libc::kill(target, signal);
        }
        let deadline = Instant::now() + Duration::from_secs(if name == "postgres" { 20 } else { 5 });
        while Instant::now() < deadline && alive(pid) {
            std::thread::sleep(Duration::from_millis(200));
        }
        if alive(pid) && name != "postgres" {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

fn port_free(port: u16) -> bool {
    port != 0 && TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn free_port(taken: &[u16]) -> u16 {
    for _ in 0..32 {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) {
            if let Ok(address) = listener.local_addr() {
                if !taken.contains(&address.port()) {
                    return address.port();
                }
            }
        }
    }
    0
}

pub fn choose_ports(saved: Ports) -> Ports {
    let mut taken = vec![API_PORT];
    let mut pick = |wanted: u16| {
        let port = if port_free(wanted) && !taken.contains(&wanted) { wanted } else { free_port(&taken) };
        taken.push(port);
        port
    };
    let database = pick(saved.database);
    let web = pick(saved.web);
    Ports { database, api: API_PORT, web }
}

fn random_bytes(count: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; count];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("Could not read random bytes: {error}"))?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn load_secrets(path: &Path) -> Result<Secrets, String> {
    let mut secrets = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Secrets>(&bytes).ok())
        .unwrap_or_default();
    if secrets.database_password.is_empty() {
        secrets.database_password = hex(&random_bytes(24)?);
    }
    if secrets.auth_secret.is_empty() {
        secrets.auth_secret = STANDARD.encode(random_bytes(32)?);
    }
    if secrets.secrets_key.is_empty() {
        secrets.secrets_key = STANDARD.encode(random_bytes(32)?);
    }
    Ok(secrets)
}

fn save_secrets(path: &Path, secrets: &Secrets) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(secrets).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    write_private(&temporary, &bytes)?;
    std::fs::rename(temporary, path)
}

pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn append_log(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn rotate_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

pub fn sidecar_name(name: &str) -> String {
    format!("{name}-{}-apple-darwin", std::env::consts::ARCH)
}

pub fn sidecar_dir() -> PathBuf {
    let bundled = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    match bundled {
        Some(dir) if dir.join(sidecar_name("loka-api")).is_file() || dir.join("loka-api").is_file() => dir,
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries"),
    }
}

pub fn sidecar(name: &str) -> Option<PathBuf> {
    let dir = sidecar_dir();
    [dir.join(sidecar_name(name)), dir.join(name)].into_iter().find(|path| path.is_file())
}

pub fn runtime_dir(resources: Option<PathBuf>) -> PathBuf {
    resources
        .map(|dir| dir.join("runtime"))
        .filter(|dir| dir.join("postgres").is_dir())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime"))
}

pub fn logs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Library")
        .join("Logs")
        .join("Loka")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loka-local-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn secrets_are_generated_once_and_private() {
        let dir = scratch();
        let path = dir.join("local.json");
        let first = load_secrets(&path).unwrap();
        save_secrets(&path, &first).unwrap();
        let again = load_secrets(&path).unwrap();
        assert_eq!(first.database_password, again.database_password);
        assert_eq!(first.secrets_key, again.secrets_key);
        assert_eq!(STANDARD.decode(&first.secrets_key).unwrap().len(), 32);
        assert!(first.database_password.bytes().all(|b| b.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ports_are_distinct_and_reuse_free_saved_ones() {
        let saved = choose_ports(Ports::default());
        assert!(saved.database != 0 && saved.web != 0);
        assert_eq!(saved.api, API_PORT);
        assert!(saved.database != API_PORT && saved.web != API_PORT && saved.database != saved.web);
        assert_eq!(choose_ports(saved), saved);
        let busy = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let taken = busy.local_addr().unwrap().port();
        let moved = choose_ports(Ports { web: taken, ..saved });
        assert_ne!(moved.web, taken);
    }

    #[test]
    fn a_crashed_service_is_restarted() {
        let dir = scratch();
        let marker = dir.join("count");
        let script = format!("echo x >> '{}'; exit 3", marker.display());
        let service = Supervised::new("probe", dir.join("probe.log"), &dir, libc::SIGTERM, true, Duration::from_secs(1), move || {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", &script]);
            Ok(command)
        });
        service.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::fs::read_to_string(&marker).map(|text| text.lines().count()).unwrap_or(0) >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        service.stop();
        assert!(std::fs::read_to_string(&marker).unwrap().lines().count() >= 2);
        assert!(std::fs::read_to_string(dir.join("probe.log")).unwrap().contains("Restarting"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restarting_swaps_the_program_and_notices_a_broken_one() {
        let dir = scratch();
        let port = free_port(&[]);
        let program = Arc::new(Mutex::new("good".to_string()));
        let chosen = program.clone();
        let root = dir.clone();
        let service = Supervised::new("swap", dir.join("swap.log"), &dir, libc::SIGTERM, true, Duration::from_secs(2), move || {
            let mut command = Command::new("/usr/bin/python3");
            if chosen.lock().unwrap().as_str() == "good" {
                command.args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1", "--directory"]).arg(&root);
            } else {
                command.args(["-c", "raise SystemExit(1)"]);
            }
            Ok(command)
        });
        let url = format!("http://127.0.0.1:{port}/");
        assert!(restart(&service, &url, Duration::from_secs(20)).await.is_ok());
        *program.lock().unwrap() = "broken".into();
        let started = Instant::now();
        assert_eq!(restart(&service, &url, Duration::from_secs(30)).await.unwrap_err(), "it stopped right after starting");
        assert!(started.elapsed() < Duration::from_secs(10));
        *program.lock().unwrap() = "good".into();
        assert!(restart(&service, &url, Duration::from_secs(20)).await.is_ok());
        service.stop();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stopping_a_service_ends_it_for_good() {
        let dir = scratch();
        let service = Supervised::new("sleeper", dir.join("sleeper.log"), &dir, libc::SIGTERM, true, Duration::from_secs(1), || {
            let mut command = Command::new("/bin/sleep");
            command.arg("30");
            Ok(command)
        });
        service.start().unwrap();
        assert!(service.running());
        assert!(dir.join("sleeper.pid").is_file());
        service.stop();
        assert!(!service.running());
        assert!(!dir.join("sleeper.pid").exists());
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!service.running());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
