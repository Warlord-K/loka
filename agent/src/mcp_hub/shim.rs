use super::gateway::{Gateway, Step};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

pub fn run(args: &[String]) -> Result<()> {
    let (socket, name, indexed) = match args {
        [flag, socket, name] if flag == "--hub" => (socket.clone(), name.clone(), false),
        [flag, socket, name, tools, mode] if flag == "--hub" && tools == "--tools" && mode == "index" => (socket.clone(), name.clone(), true),
        _ => bail!("usage: loka-agent mcp-shim --hub <socket> <server> [--tools index]"),
    };
    let stream = UnixStream::connect(&socket).with_context(|| format!("the MCP hub at {socket} is not running"))?;
    let mut writer = stream.try_clone()?;
    writer.write_all(format!("{}\n", serde_json::json!({ "server": name })).as_bytes())?;
    let out = Arc::new(Mutex::new(std::io::stdout()));
    relay(stream, std::io::stdin().lock(), out, indexed.then(|| Gateway::new(&name)))
}

fn emit<W: Write>(out: &Mutex<W>, bytes: &[u8]) -> std::io::Result<()> {
    let mut out = out.lock().unwrap();
    out.write_all(bytes)?;
    out.flush()
}

pub(crate) fn relay<R: Read, W: Write + Send + 'static>(hub: UnixStream, mut input: R, out: Arc<Mutex<W>>, gateway: Option<Gateway>) -> Result<()> {
    let mut writer = hub.try_clone()?;
    let reader = hub.try_clone()?;
    let gateway = gateway.map(|gateway| Arc::new(Mutex::new(gateway)));
    let pump_gateway = gateway.clone();
    let pump_out = out.clone();
    let pump = std::thread::spawn(move || {
        let mut lines = BufReader::new(reader);
        let mut line = Vec::new();
        while lines.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
            let bytes = match &pump_gateway {
                Some(gateway) => {
                    let text = String::from_utf8_lossy(&line);
                    let mut shaped = gateway.lock().unwrap().on_server_line(text.trim_end_matches(['\r', '\n']));
                    shaped.push('\n');
                    shaped.into_bytes()
                }
                None => line.clone(),
            };
            if emit(&pump_out, &bytes).is_err() {
                break;
            }
            line.clear();
        }
    });
    match gateway {
        Some(gateway) => {
            for line in BufReader::new(input).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let step = gateway.lock().unwrap().on_client_line(&line);
                match step {
                    Step::Forward(text) => writer.write_all(format!("{text}\n").as_bytes())?,
                    Step::Answer(text) => emit(&out, format!("{text}\n").as_bytes())?,
                }
            }
        }
        None => {
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = input.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buffer[..read])?;
            }
        }
    }
    let _ = hub.shutdown(std::net::Shutdown::Both);
    let _ = pump.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines_of(sink: &Sink) -> Vec<Value> {
        String::from_utf8_lossy(&sink.0.lock().unwrap()).lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }

    fn wait_for(sink: &Sink, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lines = lines_of(sink);
            if lines.len() >= count || Instant::now() > deadline {
                return lines;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn big_list(id: u64) -> String {
        let tools: Vec<Value> = (0..30).map(|i| json!({ "name": format!("tool_{i}"), "description": format!("does {i}. {}", "x".repeat(300)), "inputSchema": { "type": "object" } })).collect();
        format!("{}\n", json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } }))
    }

    #[test]
    fn an_indexed_shim_answers_find_tools_while_the_hub_side_is_idle() {
        let (hub_side, test_hub) = UnixStream::pair().unwrap();
        let (client_input, mut test_client) = UnixStream::pair().unwrap();
        let sink = Sink::default();
        let out = Arc::new(Mutex::new(sink.clone()));
        let shim = std::thread::spawn(move || relay(hub_side, client_input, out, Some(Gateway::new("fake"))));

        test_client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n").unwrap();
        let mut hub_lines = BufReader::new(test_hub.try_clone().unwrap());
        let mut asked = String::new();
        hub_lines.read_line(&mut asked).unwrap();
        assert!(asked.contains("tools/list"), "{asked}");
        (&test_hub).write_all(big_list(1).as_bytes()).unwrap();
        let listed = wait_for(&sink, 1);
        let names: Vec<&str> = listed[0]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["find_tools", "use_tool"]);

        test_client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"find_tools\",\"arguments\":{\"query\":\"tool_7\"}}}\n").unwrap();
        let found = wait_for(&sink, 2);
        assert!(found.len() >= 2, "find_tools was never answered while the reader was idle: {found:?}");
        assert!(found[1]["result"]["content"][0]["text"].as_str().unwrap().contains("## tool_7"), "{found:?}");

        test_client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"use_tool\",\"arguments\":{\"name\":\"tool_7\",\"arguments\":{\"q\":1}}}}\n").unwrap();
        let mut forwarded = String::new();
        hub_lines.read_line(&mut forwarded).unwrap();
        let forwarded: Value = serde_json::from_str(&forwarded).unwrap();
        assert_eq!((forwarded["id"].clone(), forwarded["params"].clone()), (json!(3), json!({ "name": "tool_7", "arguments": { "q": 1 } })));
        (&test_hub).write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n").unwrap();
        let done = wait_for(&sink, 3);
        assert_eq!(done[2]["result"]["content"][0]["text"], "done");

        drop(test_client);
        shim.join().unwrap().unwrap();
    }

    #[test]
    fn a_plain_shim_still_copies_both_directions() {
        let (hub_side, test_hub) = UnixStream::pair().unwrap();
        let (client_input, mut test_client) = UnixStream::pair().unwrap();
        let sink = Sink::default();
        let out = Arc::new(Mutex::new(sink.clone()));
        let shim = std::thread::spawn(move || relay(hub_side, client_input, out, None));
        test_client.write_all(b"{\"id\":1,\"method\":\"ping\"}\n").unwrap();
        let mut hub_lines = BufReader::new(test_hub.try_clone().unwrap());
        let mut line = String::new();
        hub_lines.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "{\"id\":1,\"method\":\"ping\"}");
        (&test_hub).write_all(b"{\"id\":1,\"result\":{}}\n").unwrap();
        assert_eq!(wait_for(&sink, 1)[0]["id"], 1);
        drop(test_client);
        shim.join().unwrap().unwrap();
    }
}
