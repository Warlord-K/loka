use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn big_list(id: &Value) -> Value {
    let tools: Vec<Value> = (0..30).map(|i| json!({ "name": format!("tool_{i}"), "description": format!("does {i}. {}", "x".repeat(300)), "inputSchema": { "type": "object" } })).collect();
    json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } })
}

fn fake_hub(listener: UnixListener) {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut lines = BufReader::new(stream).lines();
        let handshake: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(handshake["server"], "fake");
        for line in lines {
            let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let id = message["id"].clone();
            let reply = match message["method"].as_str() {
                Some("tools/list") => big_list(&id),
                Some("tools/call") => json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": format!("called {}", message["params"]["name"].as_str().unwrap()) }] } }),
                _ => continue,
            };
            writeln!(writer, "{reply}").unwrap();
        }
    });
}

#[test]
fn the_shim_with_tools_index_lists_the_gateway_and_runs_a_tool_through_it() {
    let dir = std::env::temp_dir().join(format!("loka-shim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("hub.sock");
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipped: this environment does not allow binding a unix socket ({error})");
            return;
        }
        Err(error) => panic!("{error}"),
    };
    fake_hub(listener);
    let mut shim = Command::new(env!("CARGO_BIN_EXE_loka-agent"))
        .args(["mcp-shim", "--hub", &socket.to_string_lossy(), "fake", "--tools", "index"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = shim.stdin.take().unwrap();
    let stdout = shim.stdout.take().unwrap();
    let (lines_tx, lines_rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if let Ok(value) = serde_json::from_str(&line) {
                if lines_tx.send(value).is_err() {
                    return;
                }
            }
        }
    });
    let next = || lines_rx.recv_timeout(Duration::from_secs(20)).expect("the shim answered within 20 seconds");

    writeln!(stdin, "{}", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).unwrap();
    let listed = next();
    let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["find_tools", "use_tool"]);

    writeln!(stdin, "{}", json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "find_tools", "arguments": { "query": "tool_3" } } })).unwrap();
    let found = next();
    assert!(found["result"]["content"][0]["text"].as_str().unwrap().contains("## tool_3"), "{found}");

    writeln!(stdin, "{}", json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "use_tool", "arguments": { "name": "tool_3", "arguments": {} } } })).unwrap();
    let ran = next();
    assert_eq!(ran["result"]["content"][0]["text"], "called tool_3");

    drop(stdin);
    let _ = shim.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
