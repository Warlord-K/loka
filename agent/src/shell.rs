use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::cli::job_command;

pub struct RunningShell {
    pub pid: u32,
    receiver: mpsc::Receiver<Vec<u8>>,
    waiter: tokio::task::JoinHandle<Result<Option<i32>>>,
}

impl RunningShell {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.receiver.recv().await
    }

    pub async fn wait(self) -> Option<i32> {
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.waiter).await {
            Ok(Ok(Ok(code))) => code,
            _ => None,
        }
    }
}

pub fn spawn(cwd: &Path, command: &str) -> Result<RunningShell> {
    let shell = if cfg!(target_os = "macos") {
        "/bin/zsh"
    } else {
        "/bin/bash"
    };
    let mut child = job_command(shell)
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {shell}"))?;
    let pid = child.id().context("shell process has no pid")?;
    let (tx, receiver) = mpsc::channel::<Vec<u8>>(128);
    let mut pipes: Vec<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pipes.push(Box::new(stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        pipes.push(Box::new(stderr));
    }
    for pipe in pipes {
        let mut pipe = pipe;
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 8192];
            loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buffer[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    let waiter = tokio::spawn(async move { Ok(child.wait().await?.code()) });
    Ok(RunningShell { pid, receiver, waiter })
}

pub struct Tail {
    buffer: VecDeque<u8>,
    cap: usize,
    pub total: u64,
}

impl Tail {
    pub fn new(cap: usize) -> Self {
        Self {
            buffer: VecDeque::new(),
            cap,
            total: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        if bytes.len() >= self.cap {
            self.buffer.clear();
            self.buffer.extend(&bytes[bytes.len() - self.cap..]);
            return;
        }
        self.buffer.extend(bytes);
        while self.buffer.len() > self.cap {
            self.buffer.pop_front();
        }
    }

    pub fn truncated(&self) -> bool {
        self.total > self.cap as u64
    }

    pub fn text(&self) -> String {
        let raw: Vec<u8> = self.buffer.iter().copied().collect();
        String::from_utf8_lossy(&raw).into_owned()
    }
}
