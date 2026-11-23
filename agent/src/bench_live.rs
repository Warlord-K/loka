use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cli::{self, CliOptions, CliResult, EventSender, StreamEvent, SteerSlot};

pub fn tree(root: u32) -> Vec<(u32, u64, String)> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,rss=,comm="]).output() else {
        return Vec::new();
    };
    let mut children: HashMap<u32, Vec<(u32, u64, String)>> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(rss)) = (
            parts.next().and_then(|p| p.parse::<u32>().ok()),
            parts.next().and_then(|p| p.parse::<u32>().ok()),
            parts.next().and_then(|p| p.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let comm = parts.collect::<Vec<_>>().join(" ");
        let name = comm.rsplit('/').next().unwrap_or(&comm).to_string();
        children.entry(ppid).or_default().push((pid, rss, name));
    }
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for (child, rss, name) in children.get(&pid).into_iter().flatten() {
            if seen.insert(*child) && name != "ps" {
                found.push((*child, *rss, name.clone()));
                stack.push(*child);
            }
        }
    }
    found
}

pub fn tree_rss_kb(root: u32) -> u64 {
    tree(root).iter().map(|(_, rss, _)| rss).sum()
}

async fn settle() -> u64 {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let rss = tree_rss_kb(std::process::id());
        if rss == 0 || Instant::now() > deadline {
            return rss / 1024;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn breakdown() -> serde_json::Value {
    let mut by_name: HashMap<String, (u64, u32)> = HashMap::new();
    for (_, rss, name) in tree(std::process::id()) {
        let entry = by_name.entry(name).or_default();
        entry.0 += rss / 1024;
        entry.1 += 1;
    }
    json!(by_name.into_iter().map(|(name, (mb, n))| json!({ "name": name, "mb": mb, "procs": n })).collect::<Vec<_>>())
}

pub struct Peak {
    stop: Arc<AtomicBool>,
    max: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Peak {
    pub fn watch() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let max = Arc::new(AtomicU64::new(0));
        let (s, m) = (stop.clone(), max.clone());
        let root = std::process::id();
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                m.fetch_max(tree_rss_kb(root), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(150));
            }
        });
        Self { stop, max, handle: Some(handle) }
    }

    pub fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.max.load(Ordering::Relaxed) / 1024
    }
}

struct Spec<'a> {
    prompt: &'a str,
    sid: Option<&'a str>,
    resume: bool,
    fork: bool,
    lean: bool,
    rewind_to: Option<&'a str>,
    max_turns: u32,
    steer: SteerSlot,
}

impl<'a> Spec<'a> {
    fn lean(prompt: &'a str, sid: Option<&'a str>, resume: bool) -> Self {
        Self { prompt, sid, resume, fork: false, lean: true, rewind_to: None, max_turns: 2, steer: SteerSlot::default() }
    }
}

fn options<'a>(dir: &'a Path, model: &'a str, spec: Spec<'a>, events: Option<EventSender>) -> CliOptions<'a> {
    CliOptions {
        working_dir: dir,
        prompt: spec.prompt,
        system_prompt: Some("You are a terse benchmark assistant. Answer in as few words as possible."),
        allowed_tools: None,
        max_turns: spec.max_turns,
        model,
        mcp_config_json: None,
        session_id: spec.sid,
        resume: spec.resume,
        event_tx: events,
        image_paths: Vec::new(),
        fork: spec.fork,
        lean: spec.lean,
        effort: Some("low"),
        fast: false,
        environment: Default::default(),
        mcp_has_secrets: false,
        sensitive: false,
        rewind_to: spec.rewind_to,
        approval: Default::default(),
        steer: spec.steer,
        omp_models: None,
        provider_keys: Default::default(),
        readable_files: Vec::new(),
    }
}

struct Clock {
    first_text_ms: Arc<AtomicU64>,
    spawned_ms: Arc<AtomicU64>,
}

fn clocked(started: Instant) -> (EventSender, Clock, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    let first_text_ms = Arc::new(AtomicU64::new(0));
    let spawned_ms = Arc::new(AtomicU64::new(0));
    let (f, s) = (first_text_ms.clone(), spawned_ms.clone());
    let handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let at = started.elapsed().as_millis() as u64;
            match event {
                StreamEvent::Spawned { .. } => {
                    let _ = s.compare_exchange(0, at.max(1), Ordering::Relaxed, Ordering::Relaxed);
                }
                StreamEvent::AssistantText(_) => {
                    let _ = f.compare_exchange(0, at.max(1), Ordering::Relaxed, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    });
    (tx, Clock { first_text_ms, spawned_ms }, handle)
}

fn report(backend: &str, op: &str, run: usize, fields: serde_json::Value) {
    let mut line = json!({ "backend": backend, "op": op, "run": run, "load": load() });
    if let (Some(base), Some(extra)) = (line.as_object_mut(), fields.as_object()) {
        base.extend(extra.clone());
    }
    eprintln!("BENCH {line}");
}

fn load() -> f64 {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

async fn measure(backend: &str, op: &str, run: usize, dir: &Path, model: &str, spec: Spec<'_>) -> Option<CliResult> {
    let leftover_mb = settle().await;
    let started = Instant::now();
    let (tx, clock, listener) = clocked(started);
    let peak = Peak::watch();
    let outcome = cli::run(backend, options(dir, model, spec, Some(tx))).await;
    let total = started.elapsed().as_millis() as u64;
    let peak_mb = peak.finish();
    let _ = listener.await;
    match outcome {
        Ok(result) => {
            report(
                backend,
                op,
                run,
                json!({
                    "ms": total,
                    "spawn_ms": clock.spawned_ms.load(Ordering::Relaxed),
                    "first_text_ms": clock.first_text_ms.load(Ordering::Relaxed),
                    "peak_mb": peak_mb,
                    "leftover_mb": leftover_mb,
                    "cost": result.cost_usd,
                    "in": result.usage.input_tokens,
                    "cached": result.usage.cached_input_tokens,
                    "out": result.usage.output_tokens,
                    "reply": cli::host_redactor().clip(&result.output, 60),
                }),
            );
            Some(result)
        }
        Err(error) => {
            report(backend, op, run, json!({ "ms": total, "error": cli::host_redactor().clip(&format!("{error:#}"), 300) }));
            None
        }
    }
}

const SLEEPER: &str = "Run this exact shell command in the foreground and wait for it: python3 -c \"import time; time.sleep(25); print('slept')\" . Then reply with its output.";

async fn steer_once(backend: &str, run: usize, dir: &Path, model: &str, interrupt: bool) {
    settle().await;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    let steer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let sent = Instant::now();
        let text = if interrupt {
            "Stop waiting on that command and do not run it again. Just reply with the word banana."
        } else {
            "Also put the word banana on its own line at the end of your final reply."
        };
        let ok = cli::deliver(&sender, text.into(), interrupt).await;
        (ok, sent.elapsed().as_millis() as u64, sent)
    });
    let spec = Spec { prompt: SLEEPER, sid: None, resume: false, fork: false, lean: false, rewind_to: None, max_turns: 10, steer: SteerSlot::new(receiver) };
    let peak = Peak::watch();
    let outcome = cli::run(backend, options(dir, model, spec, None)).await;
    let total = started.elapsed().as_millis() as u64;
    let peak_mb = peak.finish();
    let (ok, ack_ms, sent) = steer.await.unwrap_or((false, 0, Instant::now()));
    let after_steer_ms = sent.elapsed().as_millis() as u64;
    let op = if interrupt { "interrupt" } else { "steer" };
    match outcome {
        Ok(result) => report(
            backend,
            op,
            run,
            json!({
                "ms": total,
                "ack_ms": ack_ms,
                "accepted": ok,
                "done_after_steer_ms": after_steer_ms,
                "honoured": result.output.to_lowercase().contains("banana"),
                "peak_mb": peak_mb,
                "cost": result.cost_usd,
            }),
        ),
        Err(error) => report(backend, op, run, json!({ "ms": total, "error": cli::host_redactor().clip(&format!("{error:#}"), 300) })),
    }
}

async fn warm_turns(backend: &str, run: usize, dir: &Path, model: &str) {
    settle().await;
    let pool = crate::world::warm::Pool::new(2);
    let session = uuid::Uuid::new_v4();
    let fresh = (backend == "claude").then(|| uuid::Uuid::new_v4().to_string());
    let mut cli_sid = fresh.clone();
    let mut resume = false;
    let mut latest: Option<uuid::Uuid> = None;
    for turn in 0..3 {
        let run_id = uuid::Uuid::new_v4();
        let prompt = format!("Turn {turn}: reply with just the number {turn}.");
        let opts = options(dir, model, Spec::lean(&prompt, cli_sid.as_deref(), resume), None);
        let peak = Peak::watch();
        let started = Instant::now();
        let outcome = pool.turn(backend, session, latest, run_id, &opts).await;
        let ms = started.elapsed().as_millis() as u64;
        let peak_mb = peak.finish();
        match outcome {
            Ok((result, timing)) => {
                report(
                    backend,
                    if timing.spawned { "warm_spawn" } else { "warm_reuse" },
                    run,
                    json!({
                        "ms": ms,
                        "spawn_ms": timing.spawn_ms,
                        "first_text_ms": timing.first_output_ms,
                        "peak_mb": peak_mb,
                        "cost": result.cost_usd,
                    }),
                );
                cli_sid = result.session_id.or(cli_sid);
                resume = true;
                latest = Some(run_id);
            }
            Err(error) => {
                report(backend, "warm", run, json!({ "error": cli::host_redactor().clip(&format!("{error:#}"), 300) }));
                return;
            }
        }
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    report(backend, "warm_idle", run, json!({ "rss_mb": tree_rss_kb(std::process::id()) / 1024, "procs": breakdown() }));
    pool.clear();
    tokio::time::sleep(Duration::from_secs(2)).await;
}

async fn idle(backend: &str, run: usize, dir: &Path, model: &str) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    settle().await;
    let opts = options(dir, model, Spec { lean: false, max_turns: 10, ..Spec::lean("", None, false) }, None);
    let started = Instant::now();
    let mut ready_ms = None;
    let mut keep: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
    match backend {
        "codex" => match crate::codex::app_server::AppServer::spawn(&opts).await {
            Ok(server) => {
                ready_ms = Some(started.elapsed().as_millis() as u64);
                keep.push(Box::new(server));
            }
            Err(error) => return report(backend, "idle", run, json!({ "error": format!("{error:#}") })),
        },
        "omp" => match crate::omp::build_command(&opts, true).await {
            Ok((mut cmd, run_dir, _)) => {
                let mut child = cmd.spawn().unwrap();
                let mut stdin = child.stdin.take().unwrap();
                let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
                let _ = stdin.write_all(b"{\"type\":\"get_state\",\"id\":\"s\"}\n").await;
                let _ = stdin.flush().await;
                let answered = tokio::time::timeout(Duration::from_secs(30), async {
                    while let Ok(Some(line)) = lines.next_line().await {
                        if line.contains("\"response\"") {
                            return true;
                        }
                    }
                    false
                })
                .await;
                if matches!(answered, Ok(true)) {
                    ready_ms = Some(started.elapsed().as_millis() as u64);
                }
                keep.push(Box::new((child, stdin, lines, run_dir)));
            }
            Err(error) => return report(backend, "idle", run, json!({ "error": format!("{error:#}") })),
        },
        "claude" => {
            let launch = crate::claude::detect::prepare(&opts).await.unwrap();
            let mut cmd = crate::claude::command(&opts, None, &launch).unwrap();
            cmd.arg("--input-format").arg("stream-json");
            keep.push(Box::new(cmd.spawn().unwrap()));
        }
        _ => return,
    }
    tokio::time::sleep(Duration::from_secs(6)).await;
    report(backend, "idle", run, json!({ "ready_ms": ready_ms, "rss_mb": tree_rss_kb(std::process::id()) / 1024, "procs": breakdown() }));
    drop(keep);
    tokio::time::sleep(Duration::from_secs(1)).await;
    crate::cli::kill_process_trees(tree(std::process::id()).iter().map(|(pid, _, _)| *pid).collect()).await;
}

fn work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("loka-bench-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
        std::fs::write(dir.join(name), name).unwrap();
    }
    dir
}

fn wanted(op: &str) -> bool {
    std::env::var("BENCH_OPS").map(|ops| ops.split(',').any(|o| o == op)).unwrap_or(true)
}

#[tokio::test]
#[ignore]
async fn live_backend_bench() {
    let runs: usize = std::env::var("BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let mut backends = Vec::new();
    for (backend, var, default) in [
        ("claude", "BENCH_CLAUDE_MODEL", Some("haiku")),
        ("codex", "BENCH_CODEX_MODEL", None),
        ("cursor", "BENCH_CURSOR_MODEL", None),
        ("omp", "BENCH_OMP_MODEL", None),
        ("opencode", "BENCH_OPENCODE_MODEL", None),
    ] {
        if let Some(model) = std::env::var(var).ok().or(default.map(String::from)) {
            backends.push((backend, model));
        }
    }
    let only = std::env::var("BENCH_ONLY").ok();
    let dir = work_dir();
    for (backend, model) in backends.iter().filter(|(b, _)| only.as_deref().is_none_or(|o| o.split(',').any(|x| x == *b))) {
        let (backend, model) = (*backend, model.as_str());
        for run in 0..runs {
            if wanted("idle") {
                idle(backend, run, &dir, model).await;
            }
            if std::env::var("BENCH_IDLE_ONLY").is_ok() {
                continue;
            }
            let fresh = (backend == "claude").then(|| uuid::Uuid::new_v4().to_string());
            let Some(cold) = measure(backend, "cold", run, &dir, model, Spec::lean("Reply with just the word ok.", fresh.as_deref(), false)).await else {
                continue;
            };
            let sid = cold.session_id.clone().or(fresh.clone());
            let resume_prompt = "Also remember the word BANANA. Reply with just ok.";
            let resumed = if wanted("resume") {
                measure(backend, "resume", run, &dir, model, Spec::lean(resume_prompt, sid.as_deref(), true)).await
            } else {
                None
            };
            if wanted("fork") {
                let spec = Spec { fork: true, ..Spec::lean("Reply with just the word forked.", sid.as_deref(), true) };
                measure(backend, "fork", run, &dir, model, spec).await;
            }
            if wanted("rewind") && resumed.is_some() && matches!(backend, "claude" | "codex" | "omp") {
                let anchor = if backend == "claude" { cold.checkpoint.clone() } else { Some(resume_prompt.to_string()) };
                if let Some(anchor) = anchor {
                    let spec = Spec { rewind_to: Some(&anchor), ..Spec::lean("Which words did I ask you to remember? Reply in one line.", sid.as_deref(), true) };
                    measure(backend, "rewind", run, &dir, model, spec).await;
                }
            }
            if wanted("compact") && backend == "claude" {
                let spec = Spec { max_turns: 1, ..Spec::lean(crate::world::prompts::COMPACT_PROMPT, sid.as_deref(), true) };
                measure(backend, "compact", run, &dir, model, spec).await;
            }
            if wanted("task") {
                let spec = Spec {
                    lean: false,
                    max_turns: 6,
                    ..Spec::lean("Run `ls -1 | wc -l` in the current directory with your shell tool and reply with just the number.", None, false)
                };
                let task_sid = (backend == "claude").then(|| uuid::Uuid::new_v4().to_string());
                measure(backend, "task", run, &dir, model, Spec { sid: task_sid.as_deref(), ..spec }).await;
            }
            if wanted("warm") && crate::world::warm::supports(backend) {
                warm_turns(backend, run, &dir, model).await;
            }
        }
        let steer_runs: usize = std::env::var("BENCH_STEER_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
        if wanted("steer") && cli::steerable(backend) {
            for run in 0..steer_runs {
                steer_once(backend, run, &dir, model, false).await;
                steer_once(backend, run, &dir, model, true).await;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
