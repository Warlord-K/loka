use std::path::{Path, PathBuf};

use crate::cli::{run, CliOptions, CliResult};

fn options<'a>(dir: &'a Path, prompt: &'a str, model: &'a str) -> CliOptions<'a> {
    CliOptions {
        working_dir: dir,
        prompt,
        system_prompt: Some("Answer in as few words as possible."),
        allowed_tools: None,
        max_turns: 6,
        model,
        mcp_config_json: None,
        session_id: None,
        resume: false,
        event_tx: None,
        image_paths: Vec::new(),
        fork: false,
        lean: true,
        effort: None,
        fast: false,
        environment: std::env::var("FAL_KEY").ok().map(|key| [("FAL_KEY".to_string(), key)].into()).unwrap_or_default(),
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("parity-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

async fn ask(backend: &str, opts: CliOptions<'_>) -> CliResult {
    let started = std::time::Instant::now();
    let result = run(backend, opts).await.unwrap();
    eprintln!(
        "PARITY {backend} {} ms session={:?} checkpoint={:?} usage={:?} cost={} output={:?}",
        started.elapsed().as_millis(),
        result.session_id,
        result.checkpoint,
        result.usage,
        result.cost_usd,
        result.output.chars().take(std::env::var("PARITY_FULL").map(|_| 4000).unwrap_or(120)).collect::<String>()
    );
    result
}

fn backend_and_model() -> (String, String) {
    let backend = std::env::var("PARITY_BACKEND").expect("set PARITY_BACKEND to cursor or opencode");
    let model = std::env::var("PARITY_MODEL").expect("set PARITY_MODEL to a model the CLI accepts");
    (backend, model)
}

#[tokio::test]
#[ignore]
async fn live_lean_fork_and_rewind() {
    let (backend, model) = backend_and_model();
    let dir = scratch("lean");
    let first = ask(&backend, options(&dir, "Remember the word kumquat. Reply ok.", &model)).await;
    let sid = first.session_id.clone().expect("session id");
    assert!(first.usage.input_tokens + first.usage.cached_input_tokens > 0, "usage is reported");
    let by_prompt = backend == "omp";
    let kept = if by_prompt { "Also remember the word zebra".to_string() } else { first.checkpoint.clone().expect("checkpoint after the first turn") };

    let second = ask(&backend, CliOptions { session_id: Some(&sid), resume: true, ..options(&dir, "Also remember the word zebra. Reply ok.", &model) }).await;
    assert!(by_prompt || second.checkpoint.is_some());

    let task_dir = scratch("task");
    let forked = ask(
        &backend,
        CliOptions { session_id: Some(&sid), resume: true, fork: true, ..options(&task_dir, "List every word I asked you to remember, comma separated.", &model) },
    )
    .await;
    let words = forked.output.to_lowercase();
    assert!(words.contains("kumquat") && words.contains("zebra"), "fork keeps the conversation: {words}");
    assert_ne!(forked.session_id.as_deref(), Some(sid.as_str()), "a fork gets its own session");

    let rewound = ask(
        &backend,
        CliOptions { session_id: Some(&sid), resume: true, rewind_to: Some(&kept), ..options(&dir, "List every word I asked you to remember, comma separated.", &model) },
    )
    .await;
    let words = rewound.output.to_lowercase();
    assert!(words.contains("kumquat") && !words.contains("zebra"), "rewind drops the later turn: {words}");

    let main = ask(&backend, CliOptions { session_id: Some(&sid), resume: true, ..options(&dir, "List every word I asked you to remember, comma separated.", &model) }).await;
    let words = main.output.to_lowercase();
    assert!(words.contains("kumquat") && words.contains("zebra"), "the main session is untouched: {words}");
}

#[tokio::test]
#[ignore]
async fn live_lean_turns_cannot_run_commands() {
    let (backend, model) = backend_and_model();
    let dir = scratch("toolless");
    let marker = dir.join("made-by-lean-turn");
    let prompt = format!("Run the shell command `touch {}` and then reply done.", marker.display());
    ask(&backend, options(&dir, &prompt, &model)).await;
    assert!(!marker.exists(), "a lean turn ran a command");
}

struct HomeDir(PathBuf);

impl Drop for HomeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home_scratch(name: &str) -> HomeDir {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let dir = home.join(format!(".parity-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    HomeDir(dir)
}

#[tokio::test]
#[ignore]
async fn live_sandboxed_task_hides_secrets() {
    let (backend, model) = backend_and_model();
    let dir = scratch("sandbox");
    let secrets = home_scratch("secret");
    let secret = secrets.0.join("secret.txt");
    std::fs::write(&secret, "hunter2-parity").unwrap();
    let runner_temp = std::env::temp_dir().join(format!("loka-parity-{}.json", uuid::Uuid::new_v4().simple()));
    std::fs::write(&runner_temp, "swordfish-parity").unwrap();
    let outside = home_scratch("outside");
    let escaped = outside.0.join("escaped.txt");
    let prompt = format!(
        "Use the shell for each step, never ask for extra permissions, and report every result verbatim: 1) echo inside > inside.txt 2) cat {} 3) cat {} 4) echo out > {} 5) echo tmp > \"$TMPDIR/parity-tmp.txt\" && cat \"$TMPDIR/parity-tmp.txt\"",
        secret.display(),
        runner_temp.display(),
        escaped.display()
    );
    let result = ask(&backend, CliOptions { lean: false, system_prompt: None, ..options(&dir, &prompt, &model) }).await;
    let _ = std::fs::remove_file(&runner_temp);
    assert!(dir.join("inside.txt").exists(), "the task can write its workspace");
    if backend == "cursor" {
        assert!(!result.output.contains("hunter2-parity"), "the task read a home file outside its sandbox");
    }
    assert!(!result.output.contains("swordfish-parity"), "the task read the runner's temp files");
    assert!(!escaped.exists(), "the task wrote outside its workspace");
}

#[tokio::test]
#[ignore]
async fn live_owner_task_signs_a_merge_commit() {
    let (backend, model) = backend_and_model();
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let key = std::env::var("SIGN_TEST_KEY").map(PathBuf::from).unwrap_or_else(|_| home.join(".ssh/loka_signing"));
    assert!(key.is_file(), "set SIGN_TEST_KEY to a passphrase-less private key under ~/.ssh");
    let root = scratch("sign");
    let main = root.join("main");
    let tree = root.join("tree");
    std::fs::create_dir_all(&main).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let plain = ["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"];
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "init"]].concat());
    git(&main, &["branch", "side"]);
    git(&main, &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "on main"]].concat());
    git(&main, &["worktree", "add", "-q", tree.to_str().unwrap(), "side"]);
    git(&tree, &[&plain[..], &["commit", "-q", "--allow-empty", "-m", "on side"]].concat());
    std::fs::write(
        tree.join("sign.sh"),
        format!("git -c user.email=t@t -c user.name=t -c gpg.format=ssh -c user.signingkey={}.pub merge --no-ff -S -m merged main 2>&1 && echo signed-merge\n", key.display()),
    )
    .unwrap();
    let prompt = "Use the shell. Run exactly `sh sign.sh` once, never ask for extra permissions, and quote its full output verbatim.";
    let result = ask(&backend, CliOptions { lean: false, system_prompt: None, ..options(&tree, prompt, &model) }).await;
    let parents = git(&tree, &["log", "-1", "--format=%P"]);
    let body = git(&tree, &["cat-file", "-p", "HEAD"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(parents.split_whitespace().count(), 2, "no merge commit: {}", result.output);
    assert!(body.lines().any(|line| line.starts_with("gpgsig ")), "merge commit is unsigned: {}", result.output);
}

#[tokio::test]
#[ignore]
async fn live_owner_task_runs_in_the_owner_sandbox() {
    let (backend, model) = backend_and_model();
    let dir = scratch("owner-sandbox");
    let outside = home_scratch("outside");
    let escaped = outside.0.join("escaped.txt");
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let caches: Vec<PathBuf> = [".cargo/registry", ".npm"].iter().map(|cache| home.join(cache)).filter(|cache| cache.is_dir()).collect();
    let probe = format!(".loka-parity-{}", uuid::Uuid::new_v4().simple());
    let cache_lines: String = caches
        .iter()
        .map(|cache| format!("touch {dir}/{probe} && echo cache-ok-{name} >> results.txt; rm -f {dir}/{probe}\n", dir = cache.display(), name = cache.file_name().unwrap().to_string_lossy()))
        .collect();
    std::fs::write(
        dir.join("check.sh"),
        format!(
            "echo in > inside.txt && echo write-in-ok >> results.txt\necho out > {} 2>/dev/null && echo write-out-ok >> results.txt\n{cache_lines}curl -s -o /dev/null -w '%{{http_code}}' https://pypi.org/simple/ | grep -q 200 && echo net-ok >> results.txt\ncat results.txt\n",
            escaped.display()
        ),
    )
    .unwrap();
    let prompt = "Use the shell. Run exactly `sh check.sh` once, never ask for extra permissions, and quote its output.";
    let result = ask(&backend, CliOptions { lean: false, system_prompt: None, approval: crate::cli::Approval::Auto, ..options(&dir, prompt, &model) }).await;
    let results = std::fs::read_to_string(dir.join("results.txt")).unwrap_or_default();
    eprintln!("OWNER SANDBOX {backend}: {results:?}");
    assert!(!escaped.exists() && !results.contains("write-out-ok"), "wrote outside its worktree: {results:?}");
    let mut wanted = vec![("write-in-ok".to_string(), "could not write its worktree"), ("net-ok".to_string(), "could not reach a package host")];
    wanted.extend(caches.iter().map(|cache| (format!("cache-ok-{}", cache.file_name().unwrap().to_string_lossy()), "could not write a package cache")));
    for (line, what) in wanted {
        assert!(results.contains(&line), "{what}: {results:?} {}", result.output);
    }
}

#[tokio::test]
#[ignore]
async fn live_unoffered_model_runs_cold() {
    let (backend, model) = backend_and_model();
    let pool = crate::world::warm::Pool::new(2);
    let dir = scratch("unoffered");
    let Err(error) = pool.turn(&backend, uuid::Uuid::new_v4(), None, uuid::Uuid::new_v4(), &options(&dir, "Reply ok.", &model)).await else {
        panic!("the ACP server should not offer this model");
    };
    assert!(crate::acp::is_unoffered(&error), "unexpected error: {error:#}");
    assert!(!crate::world::warm::supports_model(&backend, &model));
    let cold = ask(&backend, options(&dir, "Reply with the single word ok.", &model)).await;
    assert!(cold.output.to_lowercase().contains("ok"), "the cold run answers: {}", cold.output);
}

#[tokio::test]
#[ignore]
async fn live_warm_session_interoperates_with_cold_runs() {
    let (backend, model) = backend_and_model();
    let pool = crate::world::warm::Pool::new(2);
    let dir = scratch("warm");
    let session = uuid::Uuid::new_v4();
    let first_run = uuid::Uuid::new_v4();
    let started = std::time::Instant::now();
    let (first, t1) = pool.turn(&backend, session, None, first_run, &options(&dir, "Remember the word kumquat. Reply ok.", &model)).await.unwrap();
    eprintln!("PARITY warm first {} ms {t1:?} checkpoint={:?} usage={:?}", started.elapsed().as_millis(), first.checkpoint, first.usage);
    assert!(t1.spawned);
    let sid = first.session_id.clone().expect("session id");
    let second_run = uuid::Uuid::new_v4();
    let started = std::time::Instant::now();
    let (second, t2) = pool
        .turn(&backend, session, Some(first_run), second_run, &CliOptions { session_id: Some(&sid), resume: true, ..options(&dir, "Also remember zebra. Which two words do you hold now? Comma separated.", &model) })
        .await
        .unwrap();
    eprintln!("PARITY warm reused {} ms {t2:?} checkpoint={:?} output={:?}", started.elapsed().as_millis(), second.checkpoint, second.output);
    assert!(!t2.spawned);
    assert!(second.output.to_lowercase().contains("kumquat"));
    assert!(backend == "omp" || second.checkpoint.is_some());
    let cold = ask(&backend, CliOptions { session_id: Some(&sid), resume: true, ..options(&dir, "List every word I asked you to remember, comma separated.", &model) }).await;
    assert!(cold.output.to_lowercase().contains("zebra"), "a cold run sees the warm turns: {}", cold.output);
    let task_dir = scratch("warm-task");
    let forked = ask(&backend, CliOptions { session_id: Some(&sid), resume: true, fork: true, ..options(&task_dir, "List every word I asked you to remember, comma separated.", &model) }).await;
    assert!(forked.output.to_lowercase().contains("zebra"), "a fork sees the warm turns: {}", forked.output);
    let started = std::time::Instant::now();
    let (third, t3) = pool
        .turn(&backend, session, Some(uuid::Uuid::new_v4()), uuid::Uuid::new_v4(), &CliOptions { session_id: Some(&sid), resume: true, ..options(&dir, "Say the first word again.", &model) })
        .await
        .unwrap();
    eprintln!("PARITY warm respawn {} ms {t3:?} output={:?}", started.elapsed().as_millis(), third.output);
    assert!(t3.spawned);
    assert!(third.output.to_lowercase().contains("kumquat"));
    pool.clear();
}

#[tokio::test]
#[ignore]
async fn live_reads_an_attached_image() {
    let (backend, model) = backend_and_model();
    let image = PathBuf::from(std::env::var("PARITY_IMAGE").expect("set PARITY_IMAGE to a solid red png"));
    let dir = scratch("image");
    let result = ask(&backend, CliOptions { image_paths: vec![image], ..options(&dir, "What single color fills the attached image? One word.", &model) }).await;
    assert!(result.output.to_lowercase().contains("red"), "{}", result.output);
}

#[tokio::test]
#[ignore]
async fn live_streams_usage_while_running() {
    let (backend, model) = backend_and_model();
    let dir = scratch("usage");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = run(&backend, CliOptions { event_tx: Some(tx), ..options(&dir, "Reply with the single word ok.", &model) }).await.unwrap();
    let mut live = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let crate::cli::StreamEvent::Usage(usage) = event {
            live.push(usage);
        }
    }
    eprintln!("PARITY live usage {live:?} final {:?}", result.usage);
    let last = live.last().expect("usage streamed before the run ended");
    assert!(last.input_tokens + last.cached_input_tokens + last.output_tokens > 0);
}
