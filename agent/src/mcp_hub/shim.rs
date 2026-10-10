use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

pub fn run(args: &[String]) -> Result<()> {
    let (socket, name) = match args {
        [flag, socket, name] if flag == "--hub" => (socket.clone(), name.clone()),
        _ => bail!("usage: loka-agent mcp-shim --hub <socket> <server>"),
    };
    let stream = UnixStream::connect(&socket).with_context(|| format!("the MCP hub at {socket} is not running"))?;
    let mut writer = stream.try_clone()?;
    writer.write_all(format!("{}\n", serde_json::json!({ "server": name })).as_bytes())?;
    let reader = stream.try_clone()?;
    let pump = std::thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        let mut lines = BufReader::new(reader);
        let mut line = Vec::new();
        while lines.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
            if out.write_all(&line).and_then(|_| out.flush()).is_err() {
                break;
            }
            line.clear();
        }
    });
    let mut input = std::io::stdin().lock();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = pump.join();
    Ok(())
}
