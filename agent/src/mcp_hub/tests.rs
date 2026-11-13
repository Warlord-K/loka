use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const FAKE_SERVER: &str = r#"
import sys, json, os
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m or 'method' not in m:
        continue
    if m['method'] == 'initialize':
        r = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fake'}}
    elif m['method'] == 'tools/call':
        if m['params']['name'] == 'die':
            os._exit(1)
        r = {'content': [{'type': 'text', 'text': json.dumps({'pid': os.getpid(), 'id': m['id'], 'token': os.environ.get('TOKEN'), 'cwd': os.getcwd()})}]}
    else:
        r = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': m['id'], 'result': r}), flush=True)
"#;

fn spec(definition: Value) -> Spec {
    Spec::from_definition("s", &definition).unwrap().unwrap()
}

#[test]
fn sharing_defaults_follow_the_server_kind() {
    assert!(spec(json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github@2025.4.8"] })).shared);
    assert!(spec(json!({ "command": "/usr/local/bin/mcp-grafana" })).shared);
    assert!(spec(json!({ "command": "/usr/bin/python3", "args": ["-E", "-s", "/app/slack_mcp.py"] })).shared);
    assert!(!spec(json!({ "command": "npx", "args": ["@playwright/mcp"] })).shared, "unknown servers stay per session");
    assert!(!spec(json!({ "command": "npx", "args": ["@modelcontextprotocol/server-filesystem", "."] })).shared);
    assert!(!spec(json!({ "command": "npx", "args": ["@modelcontextprotocol/server-github"], "shared": false })).shared);
    assert!(spec(json!({ "command": "my-server", "shared": true })).shared);
    assert!(spec(json!({ "url": "https://x.test/mcp", "headers": { "A": "b" } })).shared);
    assert!(Spec::from_definition("s", &json!({ "url": "https://x.test/mcp" })).unwrap().is_none(), "keyless remote servers need no hub");
    assert!(Spec::from_definition("s", &json!({ "command": "x", "env": { "WORLD_SECRETS_KEY": "a" } })).is_err());
}

#[test]
fn instances_never_cross_owners_or_credentials() {
    let github = |token: &str| spec(json!({ "command": "npx", "args": ["@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": token } }));
    let owner_a = isolation_for(Some(Uuid::from_u128(1)), Uuid::from_u128(10));
    let owner_b = isolation_for(Some(Uuid::from_u128(2)), Uuid::from_u128(11));
    let same_owner_other_agent = isolation_for(Some(Uuid::from_u128(1)), Uuid::from_u128(12));
    assert_eq!(instance_key(&owner_a, &github("t1"), Some(1)), instance_key(&same_owner_other_agent, &github("t1"), Some(2)), "one owner's agents share");
    assert_ne!(instance_key(&owner_a, &github("t1"), None), instance_key(&owner_b, &github("t1"), None));
    assert_ne!(instance_key(&owner_a, &github("t1"), None), instance_key(&owner_a, &github("t2"), None));
    let browser = spec(json!({ "command": "npx", "args": ["@playwright/mcp"] }));
    assert_ne!(instance_key(&owner_a, &browser, Some(1)), instance_key(&owner_a, &browser, Some(2)), "per-session servers never share");
    assert_eq!(isolation_for(None, Uuid::from_u128(9)), format!("agent:{}", Uuid::from_u128(9)));
}

#[test]
fn redaction_covers_bearer_parts() {
    let secrets = spec(json!({ "url": "https://x.test", "headers": { "Authorization": "Bearer abcdefgh12345" } })).secrets();
    assert!(secrets.contains(&"Bearer abcdefgh12345".to_string()));
    assert!(secrets.contains(&"abcdefgh12345".to_string()));
}

async fn hub() -> &'static Hub {
    static STARTED: tokio::sync::OnceCell<&'static Hub> = tokio::sync::OnceCell::const_new();
    *STARTED
        .get_or_init(|| async { start(std::env::temp_dir().join(format!("wmh-{}.sock", std::process::id()))).await.unwrap() })
        .await
}

fn me() -> Pids {
    let pid = std::process::id();
    Arc::new(Mutex::new(vec![(pid, cli::process_start(pid))]))
}

struct Client {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    async fn connect(hub: &Hub, name: &str) -> Self {
        let stream = tokio::net::UnixStream::connect(&hub.socket).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        writer.write_all(format!("{}\n", json!({ "server": name })).as_bytes()).await.unwrap();
        Self { lines: BufReader::new(reader).lines(), writer }
    }

    async fn call(&mut self, message: Value) -> Value {
        self.writer.write_all(format!("{message}\n").as_bytes()).await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line()).await.unwrap().unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn tool(&mut self, id: i64, name: &str) -> Value {
        let reply = self.call(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name } })).await;
        assert_eq!(reply["id"], json!(id), "{reply}");
        reply.pointer("/result/content/0/text").and_then(Value::as_str).map(|text| serde_json::from_str(text).unwrap()).unwrap_or(reply)
    }

    async fn initialize(&mut self) {
        let reply = self.call(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t" } } })).await;
        assert_eq!(reply["result"]["serverInfo"]["name"], "fake", "{reply}");
        self.writer.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_end_to_end() {
    shares_one_process_between_sessions_and_restarts_it().await;
    proxy_adds_the_keys_only_for_live_runs().await;
}

async fn shares_one_process_between_sessions_and_restarts_it() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let hub = hub().await;
    let dir = tempfile_dir();
    let script = dir.join("fake.py");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    let definition = |token: &str| json!({ "command": "python3", "args": [script.to_string_lossy()], "env": { "TOKEN": token }, "shared": true });
    let owner = Some(Uuid::new_v4());
    let agent = Uuid::new_v4();
    let servers = |token: &str| {
        let mut servers = Map::new();
        servers.insert("fake".into(), definition(token));
        servers.insert("remote".into(), json!({ "url": "https://example.test/mcp", "headers": { "Authorization": "Bearer remote-secret-123" } }));
        servers.insert("open".into(), json!({ "url": "https://example.test/open", "shared": true }));
        servers
    };
    let job = |agent| Job { isolation: isolation_for(owner, agent), agent, cwd: &dir, pids: me(), passthrough: &[] };
    let prepared = hub.prepare(job(agent), &servers("first-secret")).await.unwrap();
    let text = Value::Object(prepared.servers.clone()).to_string();
    assert!(!text.contains("first-secret") && !text.contains("remote-secret-123"), "{text}");
    assert_eq!(prepared.servers["fake"]["args"][0], "mcp-shim");
    assert!(prepared.servers["remote"]["url"].as_str().unwrap().starts_with("http://127.0.0.1:"));
    assert!(prepared.servers["remote"]["headers"][TOKEN_HEADER].is_string());
    assert_eq!(prepared.servers["open"], json!({ "url": "https://example.test/open" }));
    let other_agent = hub.prepare(job(Uuid::new_v4()), &servers("first-secret")).await.unwrap();

    let mut a = Client::connect(hub, "fake").await;
    let mut b = Client::connect(hub, "fake").await;
    a.initialize().await;
    b.initialize().await;
    let (one, two) = (a.tool(1, "x").await, b.tool(1, "x").await);
    assert_eq!(one["pid"], two["pid"], "both sessions reach one process");
    assert_eq!(one["token"], "first-secret");
    assert_ne!(one["id"], two["id"], "ids are rewritten per session");
    assert_eq!(hub.instance_count().await, 1);

    let crashed = a.tool(2, "die").await;
    assert!(crashed["error"]["message"].as_str().unwrap().contains("restarted"), "{crashed}");
    let again = b.tool(3, "x").await;
    assert_ne!(again["pid"], one["pid"], "a crashed server comes back on the next call");

    drop(prepared);
    drop(other_agent);
    let rotated = hub.prepare(job(agent), &servers("second-secret")).await.unwrap();
    let fresh = a.tool(4, "x").await;
    assert_eq!(fresh["token"], "second-secret", "a changed key starts a new process");
    assert_ne!(fresh["pid"], again["pid"]);
    assert_eq!(hub.instance_count().await, 1, "the instance with the old key stops at once");

    rotated.registration.as_ref().unwrap().revoker().revoke().await;
    let refused = a.call(json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "x" } })).await;
    assert_eq!(refused["error"]["code"], OUTSIDE_RUN, "{refused}");
    assert_eq!(hub.instance_count().await, 0, "a revoked key stops its servers");
    drop(rotated);
    let _ = std::fs::remove_dir_all(dir);
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wmh-test-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn proxy_paths_and_legacy_sse_endpoints_stay_on_the_hub() {
    assert_eq!(proxy::upstream_url("https://x.test/v1/mcp?a=1", None, Some("b=2")).unwrap(), "https://x.test/v1/mcp?a=1&b=2");
    assert_eq!(proxy::upstream_url("https://x.test/sse", Some("messages"), Some("s=1")).unwrap(), "https://x.test/messages?s=1");
    assert_eq!(proxy::rewrite_endpoint("/messages?sessionId=1", "https://x.test/sse", "lin ear"), "/mcp/lin%20ear/~/messages?sessionId=1");
    assert_eq!(proxy::rewrite_endpoint("https://x.test/m?s=1", "https://x.test/sse", "l"), "/mcp/l/~/m?s=1");
    assert_eq!(proxy::rewrite_endpoint("https://evil.test/m", "https://x.test/sse", "l"), "https://evil.test/m");
}

async fn proxy_adds_the_keys_only_for_live_runs() {
    use axum::routing::post;
    let upstream = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/mcp",
        post(|headers: axum::http::HeaderMap, body: String| async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
            let leaked = headers.contains_key(TOKEN_HEADER);
            axum::Json(json!({ "auth": auth, "leaked": leaked, "body": body }))
        }),
    );
    tokio::spawn(async move { axum::serve(upstream, app).await.unwrap() });
    let hub = hub().await;
    let dir = tempfile_dir();
    let agent = Uuid::new_v4();
    let mut servers = Map::new();
    servers.insert("remote".into(), json!({ "type": "http", "url": format!("http://{address}/mcp"), "headers": { "Authorization": "Bearer proxied-secret" } }));
    let job = Job { isolation: isolation_for(Some(Uuid::new_v4()), agent), agent, cwd: &dir, pids: me(), passthrough: &[] };
    let prepared = hub.prepare(job, &servers).await.unwrap();
    let url = prepared.servers["remote"]["url"].as_str().unwrap().to_string();
    let token = prepared.servers["remote"]["headers"][TOKEN_HEADER].as_str().unwrap().to_string();
    assert_eq!(prepared.servers["remote"]["type"], "http");
    let client = reqwest::Client::new();
    let reply: Value = client.post(&url).header(TOKEN_HEADER, &token).body("{}").send().await.unwrap().json().await.unwrap();
    assert_eq!(reply, json!({ "auth": "Bearer proxied-secret", "leaked": false, "body": "{}" }));
    assert_eq!(client.post(&url).body("{}").send().await.unwrap().status(), 401);
    assert_eq!(client.post(&url).header(TOKEN_HEADER, "nope").body("{}").send().await.unwrap().status(), 403);
    drop(prepared);
    assert_eq!(client.post(&url).header(TOKEN_HEADER, &token).body("{}").send().await.unwrap().status(), 403, "the token stops working when the run ends");
    let _ = std::fs::remove_dir_all(dir);
}

fn live_options<'a>(dir: &'a Path, prompt: &'a str, model: &'a str, mcp: String, events: cli::EventSender) -> cli::CliOptions<'a> {
    cli::CliOptions {
        working_dir: dir,
        prompt,
        system_prompt: Some("You are a terse test assistant."),
        allowed_tools: None,
        max_turns: 6,
        model,
        mcp_config_json: Some(mcp),
        session_id: None,
        resume: false,
        event_tx: Some(events),
        image_paths: Vec::new(),
        fork: false,
        lean: false,
        effort: Some("low"),
        fast: false,
        environment: Default::default(),
        mcp_has_secrets: true,
        sensitive: true,
        rewind_to: None,
        approval: cli::Approval::Full,
        steer: Default::default(),
        omp_models: None,
        provider_keys: Default::default(),
        readable_files: Vec::new(),
    }
}

fn processes_matching(needle: &str) -> Vec<(u32, u64)> {
    let out = std::process::Command::new("ps").args(["-axo", "pid=,rss=,command="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(needle) && !line.contains("ps -axo"))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_claude_and_codex_share_one_server() {
    let bin = std::env::var("LOKA_AGENT_BIN").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../target/debug/loka-agent").to_string());
    let codex_model = std::env::var("CODEX_TEST_MODEL").unwrap_or_else(|_| "gpt-6.1-sol".to_string());
    let hub = hub().await;
    let dir = tempfile_dir();
    let memory = dir.join("memory.json");
    let owner = Some(Uuid::new_v4());
    let mut servers = Map::new();
    servers.insert(
        "memory".into(),
        json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"], "env": { "MEMORY_FILE_PATH": memory.to_string_lossy() } }),
    );
    let prompt = "Call the read_graph tool of the memory MCP server exactly once, then reply with the single word done.";
    let mut runs = Vec::new();
    let mut keep = Vec::new();
    for (backend, model) in [("claude", "haiku".to_string()), ("codex", codex_model.clone())] {
        let agent = Uuid::new_v4();
        let pids: Pids = Default::default();
        let prepared = hub
            .prepare(Job { isolation: isolation_for(owner, agent), agent, cwd: &dir, pids: pids.clone(), passthrough: &[] }, &servers)
            .await
            .unwrap();
        let mut rewritten = prepared.servers.clone();
        rewritten["memory"]["command"] = json!(bin);
        let config = json!({ "mcpServers": rewritten }).to_string();
        assert!(!config.contains(&memory.to_string_lossy().to_string()), "{config}");
        keep.push(prepared);
        let dir = dir.clone();
        runs.push(tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let tools = tokio::spawn(async move {
                let mut used = Vec::new();
                while let Some(event) = rx.recv().await {
                    if let cli::StreamEvent::ToolUse { tool, .. } = event {
                        used.push(tool);
                    }
                }
                used
            });
            let result = cli::record_spawns(pids, cli::run(backend, live_options(&dir, prompt, &model, config, tx))).await;
            (backend, result, tools.await.unwrap())
        }));
    }
    let mut peak = 0;
    let mut finished = Vec::new();
    while !runs.is_empty() {
        tokio::time::sleep(Duration::from_millis(500)).await;
        peak = peak.max(processes_matching("server-memory").len());
        let mut index = 0;
        while index < runs.len() {
            if runs[index].is_finished() {
                finished.push(runs.remove(index).await.unwrap());
            } else {
                index += 1;
            }
        }
    }
    assert_eq!(finished.len(), 2);
    for (backend, result, tools) in &finished {
        let result = result.as_ref().unwrap_or_else(|error| panic!("{backend}: {error:#}"));
        println!("{backend}: tools {tools:?} output {:?}", result.output);
        assert!(tools.iter().any(|tool| tool.contains("read_graph")), "{backend} never called the shared server: {tools:?}");
    }
    println!("hub instances {} peak server-memory processes {peak}", hub.instance_count().await);
    assert_eq!(hub.instance_count().await, 1, "both CLIs used one instance");
    assert!(peak <= 2, "one npx wrapper plus one node process at most, saw {peak}");
    drop(keep);
    let _ = std::fs::remove_dir_all(dir);
}

fn tree_usage(roots: &[u32]) -> (usize, u64) {
    let out = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,rss="]).output().unwrap();
    let rows: Vec<(u32, u32, u64)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect();
    let mut seen: Vec<u32> = roots.to_vec();
    let mut index = 0;
    while index < seen.len() {
        let parent = seen[index];
        seen.extend(rows.iter().filter(|(_, ppid, _)| *ppid == parent).map(|(pid, _, _)| *pid));
        index += 1;
    }
    let rss = rows.iter().filter(|(pid, _, _)| seen.contains(pid)).map(|(_, _, rss)| rss).sum();
    (rows.iter().filter(|(pid, _, _)| seen.contains(pid)).count(), rss)
}

async fn handshake(stdin: &mut tokio::process::ChildStdin, stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> usize {
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"bench\",\"version\":\"1\"}}}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), stdout.next_line()).await.unwrap().unwrap();
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n").await.unwrap();
    let line = tokio::time::timeout(Duration::from_secs(60), stdout.next_line()).await.unwrap().unwrap().unwrap();
    serde_json::from_str::<Value>(&line).unwrap()["result"]["tools"].as_array().map(Vec::len).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_hub_memory() {
    let agents: usize = std::env::var("BENCH_AGENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let bin = std::env::var("LOKA_AGENT_BIN").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../target/release/loka-agent").to_string());
    let catalog = [
        ("github", json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "bench-token-value" } })),
        ("context7", json!({ "command": "npx", "args": ["-y", "@upstash/context7-mcp"] })),
        ("memory", json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"] })),
        ("thinking", json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-sequential-thinking"] })),
    ];
    let mut direct = Vec::new();
    for _ in 0..agents {
        for (_, definition) in &catalog {
            let mut cmd = tokio::process::Command::new("npx");
            cmd.args(definition["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()));
            for (key, value) in definition.get("env").and_then(Value::as_object).into_iter().flatten() {
                cmd.env(key, value.as_str().unwrap());
            }
            cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
            let mut child = cmd.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
            handshake(&mut stdin, &mut stdout).await;
            direct.push((child, stdin, stdout));
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let roots: Vec<u32> = direct.iter().filter_map(|(child, _, _)| child.id()).collect();
    let (before_count, before_rss) = tree_usage(&roots);
    for (child, _, _) in &mut direct {
        let _ = child.kill().await;
    }
    cli::kill_process_trees(roots).await;
    drop(direct);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let hub = hub().await;
    let dir = tempfile_dir();
    let owner = Some(Uuid::new_v4());
    let mut servers = Map::new();
    for (name, definition) in &catalog {
        servers.insert(name.to_string(), definition.clone());
    }
    let mut keep = Vec::new();
    let mut shims = Vec::new();
    for _ in 0..agents {
        let agent = Uuid::new_v4();
        keep.push(hub.prepare(Job { isolation: isolation_for(owner, agent), agent, cwd: &dir, pids: me(), passthrough: &[] }, &servers).await.unwrap());
        for (name, _) in &catalog {
            let mut child = tokio::process::Command::new(&bin)
                .args(["mcp-shim", "--hub", &hub.socket.to_string_lossy(), name])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
            assert!(handshake(&mut stdin, &mut stdout).await > 0, "{name} answered through the hub");
            shims.push((child, stdin, stdout));
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (server_count, server_rss) = tree_usage(&hub.instance_pids().await);
    let shim_roots: Vec<u32> = shims.iter().filter_map(|(child, _, _)| child.id()).collect();
    let (shim_count, shim_rss) = tree_usage(&shim_roots);
    println!(
        "agents={agents} servers={} before: {before_count} processes, {} MB | after: {server_count} server processes {} MB + {shim_count} shims {} MB = {} processes, {} MB | hub instances {}",
        catalog.len(),
        before_rss / 1024,
        server_rss / 1024,
        shim_rss / 1024,
        server_count + shim_count,
        (server_rss + shim_rss) / 1024,
        hub.instance_count().await,
    );
    drop(shims);
    drop(keep);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mcp_remote_servers_keep_one_sign_in_folder_per_server() {
    let home = std::env::temp_dir().join(format!("mcp-auth-{}", Uuid::new_v4().simple()));
    let mut servers = Map::new();
    servers.insert("mac-notion".into(), json!({"command": "npx", "args": ["-y", "mcp-remote@latest", "https://mcp.notion.com/mcp"], "env": {"MCP_REMOTE_CONFIG_DIR": "***"}}));
    servers.insert("mac notion/fnl".into(), json!({"command": "/opt/bin/mcp-remote", "args": ["https://mcp.notion.com/mcp"]}));
    servers.insert("pinned".into(), json!({"command": "npx", "args": ["mcp-remote", "https://x"], "env": {"MCP_REMOTE_CONFIG_DIR": "/keep/me"}}));
    servers.insert("other".into(), json!({"command": "npx", "args": ["some-server"]}));
    pin_sign_in_dirs(&mut servers, &home);
    let dir = |name: &str| servers[name]["env"]["MCP_REMOTE_CONFIG_DIR"].as_str().map(String::from);
    assert_eq!(dir("mac-notion"), Some(home.join("mac-notion").to_string_lossy().into_owned()));
    assert_eq!(dir("mac notion/fnl"), Some(home.join("mac-notion-fnl").to_string_lossy().into_owned()));
    assert_eq!(dir("pinned").as_deref(), Some("/keep/me"));
    assert_eq!(dir("other"), None);
    assert!(home.join("mac-notion").is_dir() && home.join("mac-notion-fnl").is_dir());
    let again = servers.clone();
    pin_sign_in_dirs(&mut servers, &home);
    assert_eq!(servers, again);
    std::fs::remove_dir_all(&home).unwrap();
}
