use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tracing::warn;

pub const HUB_ENV: &str = "LOKA_CMD_HUB";
pub const REAL_SHELL_ENV: &str = "LOKA_REAL_SHELL";
pub const SHELL_NAMES: &[&str] = &["bash", "zsh"];
const MARK: &str = "loka-secret-";
const MAX_REQUEST: u64 = 64 * 1024;

pub struct CmdHub {
    socket: PathBuf,
    shells: PathBuf,
    key: hmac::Key,
    values: Mutex<HashMap<String, String>>,
}

static HUB: OnceLock<&'static CmdHub> = OnceLock::new();

pub fn get() -> Option<&'static CmdHub> {
    HUB.get().copied()
}

pub fn enabled() -> bool {
    !matches!(
        std::env::var("WORLD_CMD_HUB").map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Ok("off" | "0" | "false" | "no")
    )
}

pub async fn start(socket: PathBuf, shell_dir: PathBuf) -> Result<&'static CmdHub> {
    if let Some(hub) = get() {
        return Ok(hub);
    }
    let spaced = |path: &Path| path.to_string_lossy().chars().any(char::is_whitespace);
    let fallback = std::env::temp_dir().join(format!("loka-cmd-hub-{}", std::process::id()));
    let shell_dir = if spaced(&shell_dir) { fallback.clone() } else { shell_dir };
    let socket = if spaced(&socket) || socket.as_os_str().len() > 100 { fallback.join("hub.sock") } else { socket };
    std::fs::create_dir_all(&shell_dir)?;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = tokio::fs::remove_file(&socket).await;
    let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("could not open the command hub socket {}", socket.display()))?;
    crate::cli::share_with_jobs(&socket, 0o666)?;
    #[cfg(unix)]
    std::fs::set_permissions(&shell_dir, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    let exe = std::env::current_exe()?;
    for name in SHELL_NAMES {
        let shell = shell_dir.join(name);
        if std::fs::read_link(&shell).ok().as_deref() != Some(exe.as_path()) {
            let _ = std::fs::remove_file(&shell);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&exe, &shell).with_context(|| format!("could not link the command shell at {}", shell.display()))?;
        }
    }
    let mut secret = [0u8; 32];
    SystemRandom::new().fill(&mut secret).map_err(|_| anyhow::anyhow!("no system randomness"))?;
    let hub: &'static CmdHub = Box::leak(Box::new(CmdHub {
        socket,
        shells: shell_dir,
        key: hmac::Key::new(hmac::HMAC_SHA256, &secret),
        values: Mutex::new(HashMap::new()),
    }));
    if HUB.set(hub).is_err() {
        return Ok(get().expect("hub set"));
    }
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(async move {
                        if let Err(error) = hub.answer(stream).await {
                            warn!(error = %error, "command hub request failed");
                        }
                    });
                }
                Err(error) => {
                    warn!(error = %error, "command hub stopped accepting");
                    return;
                }
            }
        }
    });
    Ok(hub)
}

fn secret_name(name: &str) -> bool {
    !name.starts_with("WORLD_") && !name.starts_with("LOKA_") && !name.starts_with("KARNA_") && name != "PATH" && name != "HOME"
}

impl CmdHub {
    fn placeholder(&self, name: &str, value: &str) -> String {
        let tag = hmac::sign(&self.key, format!("{name}\0{value}").as_bytes());
        let hex: String = tag.as_ref()[..20].iter().map(|byte| format!("{byte:02x}")).collect();
        let token = format!("{MARK}{hex}");
        self.values.lock().unwrap().insert(token.clone(), value.to_string());
        token
    }

    pub fn shield(&self, cmd: &mut tokio::process::Command, granted: &BTreeMap<String, String>, hook: Hook) -> bool {
        let mut swapped = false;
        for (name, value) in granted {
            if value.len() < 4 || value.contains(MARK) || !secret_name(name) {
                continue;
            }
            let current = cmd.as_std().get_envs().find(|(key, _)| key.to_str() == Some(name.as_str())).and_then(|(_, value)| value);
            if current.and_then(|current| current.to_str()) != Some(value.as_str()) {
                continue;
            }
            cmd.env(name, self.placeholder(name, value));
            swapped = true;
        }
        let ruled = granted.get(loka_shared::command_rules::ENV).is_some_and(|rules| !rules.is_empty());
        if swapped || ruled {
            let configured = cmd
                .as_std()
                .get_envs()
                .find(|(key, _)| key.to_str() == Some("SHELL"))
                .and_then(|(_, value)| value.map(PathBuf::from))
                .or_else(|| std::env::var_os("SHELL").map(PathBuf::from));
            let real = configured
                .filter(|shell| !shell.starts_with(&self.shells) && shell.is_absolute() && shell.file_name().is_some_and(|name| SHELL_NAMES.iter().any(|known| name == *known)))
                .unwrap_or_else(|| PathBuf::from("/bin/bash"));
            let name = real.file_name().and_then(|name| name.to_str()).unwrap_or("bash").to_string();
            cmd.env(HUB_ENV, &self.socket);
            cmd.env(REAL_SHELL_ENV, &real);
            cmd.env(hook.variable(), self.shells.join(name));
        }
        swapped
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn answer(&self, mut stream: tokio::net::UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.split();
        let mut line = String::new();
        tokio::io::BufReader::new(reader.take(MAX_REQUEST)).read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line).context("bad command hub request")?;
        let wanted: Vec<String> = request
            .get("resolve")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).take(256).map(String::from).collect())
            .unwrap_or_default();
        let found: serde_json::Map<String, Value> = {
            let values = self.values.lock().unwrap();
            wanted.into_iter().filter_map(|token| values.get(&token).map(|value| (token, Value::String(value.clone())))).collect()
        };
        writer.write_all(format!("{}\n", json!({ "values": found })).as_bytes()).await?;
        writer.shutdown().await?;
        Ok(())
    }
}

pub const PROMPT_NOTE: &str = "The keys granted to you show up in your environment as placeholders (loka-secret-...). Use the variables normally in your commands: every shell command you run gets the real values filled in, and the real values in its output are shown back as the placeholders. Never try to read, print, decode or copy out the real values, and do not treat the placeholder text as a broken key.";

pub const RULES_NOTE: &str = "Your owner limits which shell commands you may run here. A blocked command fails with a message that starts with Loka blocked this command. When that happens, do not look for another way to run it: say which command you needed and why, and your owner can allow it.";

pub fn rules_note(backend: &str) -> Option<&'static str> {
    get()?;
    loka_shared::command_rules::enforced_for(backend).then_some(RULES_NOTE)
}

pub fn prompt_note(backend: &str, granted: &BTreeMap<String, String>) -> Option<&'static str> {
    get()?;
    (Hook::for_backend(backend).is_some() && granted.iter().any(|(name, value)| value.len() >= 4 && secret_name(name))).then_some(PROMPT_NOTE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    ClaudeShell,
    Shell,
}

impl Hook {
    fn variable(self) -> &'static str {
        match self {
            Hook::ClaudeShell => "CLAUDE_CODE_SHELL",
            Hook::Shell => "SHELL",
        }
    }

    pub fn for_backend(backend: &str) -> Option<Hook> {
        match backend {
            "claude" => Some(Hook::ClaudeShell),
            "cursor" | "opencode" => Some(Hook::Shell),
            _ => None,
        }
    }
}

pub fn shield(cmd: &mut tokio::process::Command, granted: &BTreeMap<String, String>, hook: Hook) -> bool {
    get().is_some_and(|hub| hub.shield(cmd, granted, hook))
}

pub fn is_shell_invocation() -> bool {
    std::env::args_os()
        .next()
        .and_then(|arg| Path::new(&arg).file_name().map(|name| SHELL_NAMES.iter().any(|known| name == *known)))
        .unwrap_or(false)
        && std::env::var_os(HUB_ENV).is_some()
}

fn placeholders_in(text: &str, found: &mut Vec<String>) {
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        let tail = &rest[at..];
        let end = tail[MARK.len()..].find(|c: char| !c.is_ascii_hexdigit()).map_or(tail.len(), |n| n + MARK.len());
        let token = &tail[..end];
        if token.len() > MARK.len() && !found.iter().any(|known| known == token) {
            found.push(token.to_string());
        }
        rest = &tail[end..];
    }
}

fn resolve(socket: &str, wanted: &[String]) -> Option<Vec<(String, String)>> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok()?;
    stream.write_all(format!("{}\n", json!({ "resolve": wanted })).as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let answer: Value = serde_json::from_str(&line).ok()?;
    let values = answer.get("values")?.as_object()?;
    let mut pairs: Vec<(String, String)> =
        values.iter().filter_map(|(token, value)| Some((token.clone(), value.as_str()?.to_string()))).collect();
    pairs.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
    Some(pairs)
}

fn fill(text: &str, pairs: &[(String, String)]) -> String {
    pairs.iter().fold(text.to_string(), |text, (token, value)| text.replace(token, value))
}

struct Redactor {
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
    longest: usize,
    pending: Vec<u8>,
}

impl Redactor {
    fn new(pairs: &[(String, String)]) -> Self {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = pairs.iter().map(|(token, value)| (value.as_bytes().to_vec(), token.as_bytes().to_vec())).collect();
        let longest = pairs.iter().map(|(value, _)| value.len()).max().unwrap_or(0);
        Self { pairs, longest, pending: Vec::new() }
    }

    fn replace_all(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut at = 0;
        'outer: while at < data.len() {
            for (value, token) in &self.pairs {
                if data[at..].starts_with(value) {
                    out.extend_from_slice(token);
                    at += value.len();
                    continue 'outer;
                }
            }
            out.push(data[at]);
            at += 1;
        }
        out
    }

    fn held_back(&self, data: &[u8]) -> usize {
        let max = self.longest.saturating_sub(1).min(data.len());
        (1..=max)
            .rev()
            .find(|&n| {
                let tail = &data[data.len() - n..];
                self.pairs.iter().any(|(value, _)| value.len() > n && value.starts_with(tail))
            })
            .unwrap_or(0)
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let pending = std::mem::take(&mut self.pending);
        let replaced = self.replace_all(&pending);
        let keep = self.held_back(&replaced);
        let ready = replaced.len() - keep;
        self.pending = replaced[ready..].to_vec();
        replaced[..ready].to_vec()
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

fn relay(mut from: impl Read, mut to: impl Write, pairs: Vec<(String, String)>) {
    let mut redactor = Redactor::new(&pairs);
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&redactor.push(&buffer[..n])).and_then(|_| to.flush()).is_err() {
                    return;
                }
            }
        }
    }
    let _ = to.write_all(&redactor.finish());
    let _ = to.flush();
}

static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn forward_signal(signal: libc::c_int) {
    let pid = CHILD.load(std::sync::atomic::Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

fn enforce_rules(args: &[String]) {
    let Some(raw) = std::env::var_os(loka_shared::command_rules::ENV) else {
        return;
    };
    let verdict = match raw.to_str().and_then(|raw| serde_json::from_str::<loka_shared::command_rules::Policy>(raw).ok()) {
        Some(policy) => loka_shared::command_rules::check_shell_args(&policy, args),
        None => Err(loka_shared::command_rules::Blocked { command: args.join(" ").chars().take(80).collect(), reason: "could not be checked because the command rules are unreadable" }),
    };
    if let Err(blocked) = verdict {
        eprintln!("Loka blocked this command: {blocked}. Your owner sets command rules in You > Keys or by asking their main agent.");
        std::process::exit(126);
    }
}

pub fn run_shell() -> ! {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let me = std::env::current_exe().ok().and_then(|exe| std::fs::canonicalize(exe).ok());
    let real = std::env::var_os(REAL_SHELL_ENV)
        .map(PathBuf::from)
        .filter(|shell| std::fs::canonicalize(shell).ok() != me)
        .unwrap_or_else(|| PathBuf::from("/bin/bash"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    enforce_rules(&args);
    let mut wanted = Vec::new();
    for (_, value) in std::env::vars() {
        placeholders_in(&value, &mut wanted);
    }
    for arg in &args {
        placeholders_in(arg, &mut wanted);
    }
    let pairs = match std::env::var(HUB_ENV) {
        Ok(socket) if !wanted.is_empty() => resolve(&socket, &wanted).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut command = std::process::Command::new(&real);
    command.args(args.iter().map(|arg| fill(arg, &pairs)));
    for (name, value) in std::env::vars() {
        if value.contains(MARK) {
            command.env(name, fill(&value, &pairs));
        }
    }
    if pairs.is_empty() {
        let error = command.exec();
        eprintln!("loka shell: could not start {}: {error}", real.display());
        std::process::exit(127);
    }
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("loka shell: could not start {}: {error}", real.display());
            std::process::exit(127);
        }
    };
    CHILD.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        unsafe {
            libc::signal(signal, forward_signal as *const () as libc::sighandler_t);
        }
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_pairs = pairs.clone();
    let out = std::thread::spawn(move || relay(stdout, std::io::stdout().lock(), out_pairs));
    let err = std::thread::spawn(move || relay(stderr, std::io::stderr().lock(), pairs));
    let status = child.wait();
    let _ = out.join();
    let _ = err.join();
    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1))),
        Err(_) => std::process::exit(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_placeholders_inside_text() {
        let mut found = Vec::new();
        placeholders_in("Bearer loka-secret-abc123 and loka-secret-ffff, loka-secret-abc123", &mut found);
        assert_eq!(found, vec!["loka-secret-abc123".to_string(), "loka-secret-ffff".to_string()]);
    }

    #[test]
    fn output_turns_real_values_back_into_placeholders_across_chunks() {
        let pairs = vec![("loka-secret-aa".to_string(), "hunter2-secret".to_string())];
        let mut redactor = Redactor::new(&pairs);
        let mut out = Vec::new();
        for chunk in ["token=hunt", "er2-sec", "ret done ", "hunter2-secret\n", "hunter"] {
            out.extend(redactor.push(chunk.as_bytes()));
        }
        out.extend(redactor.finish());
        assert_eq!(String::from_utf8(out).unwrap(), "token=loka-secret-aa done loka-secret-aa\nhunter");
    }

    #[tokio::test]
    async fn the_hub_resolves_only_known_placeholders_and_the_shell_runs_with_real_values() {
        let dir = std::env::temp_dir().join(format!("cmd-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = start(dir.join("hub.sock"), dir.join("bin")).await.unwrap();
        let mut cmd = tokio::process::Command::new("true");
        cmd.env("DEPLOY_TOKEN", "real-deploy-token");
        cmd.env("WORLD_BROWSER_LAUNCHER", "/x/browser");
        let granted = BTreeMap::from([
            ("DEPLOY_TOKEN".to_string(), "real-deploy-token".to_string()),
            ("WORLD_BROWSER_LAUNCHER".to_string(), "/x/browser".to_string()),
        ]);
        assert!(hub.shield(&mut cmd, &granted, Hook::ClaudeShell));
        let env: HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().to_string(), v.unwrap().to_string_lossy().to_string()))
            .collect();
        let token = env["DEPLOY_TOKEN"].clone();
        assert!(token.starts_with(MARK));
        assert_eq!(env["WORLD_BROWSER_LAUNCHER"], "/x/browser");
        assert!(env["CLAUDE_CODE_SHELL"].starts_with(&*dir.join("bin").to_string_lossy()));
        assert!(Path::new(&env[REAL_SHELL_ENV]).is_absolute());
        let socket = env[HUB_ENV].clone();
        let answer = tokio::task::spawn_blocking(move || resolve(&socket, &[token.clone(), "loka-secret-00".into()]).unwrap()).await.unwrap();
        assert_eq!(answer.len(), 1);
        assert_eq!(answer[0].1, "real-deploy-token");
        let _ = std::fs::remove_dir_all(dir);
    }
}
