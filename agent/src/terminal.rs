use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use loka_shared::terminal::{login_shell, TerminalJob, TermFrame};

use crate::remote_runner::RunnerApi;

const OUT_READ_MAX: usize = 32 * 1024;
const PING_EVERY: Duration = Duration::from_secs(25);

fn start_dir(root: &Path) -> PathBuf {
    let workspaces = root.join("workspaces");
    if workspaces.is_dir() {
        workspaces
    } else {
        root.to_path_buf()
    }
}

pub(crate) type ApiSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(crate) async fn connect_api(api: &RunnerApi, session: uuid::Uuid) -> Result<ApiSocket> {
    let url = api.terminal_socket_url(session);
    let mut request = url.as_str().into_client_request().context("invalid terminal socket url")?;
    request.headers_mut().insert(
        "Authorization",
        format!("Runner {}", api.runner_token()).parse().context("invalid runner token")?,
    );
    let (socket, _) = tokio_tungstenite::connect_async_with_config(request, None, false)
        .await
        .context("terminal socket connect failed")?;
    Ok(socket)
}

pub async fn run_session(api: &RunnerApi, root: &Path, job: TerminalJob) -> Result<()> {
    if job.kind == loka_shared::terminal::BROWSER {
        return crate::browser_view::run_session(api, root, job).await;
    }
    let (mut ws_write, mut ws_read) = connect_api(api, job.id).await?.split();
    let cwd = start_dir(root);
    let pair = native_pty_system()
        .openpty(PtySize { rows: (job.rows as u16).max(2), cols: (job.cols as u16).max(10), pixel_width: 0, pixel_height: 0 })
        .context("openpty failed")?;
    let shell = login_shell();
    let launch = crate::cli::job_std_command(&shell);
    let mut command = CommandBuilder::from_argv(
        std::iter::once(launch.get_program()).chain(launch.get_args()).map(ToOwned::to_owned).collect(),
    );
    command.args(["-l", "-i"]);
    command.cwd(&cwd);
    command.env_clear();
    for (key, value) in std::env::vars() {
        let keep = matches!(key.as_str(), "HOME" | "USER" | "LOGNAME" | "PATH" | "TMPDIR" | "SSH_AUTH_SOCK" | "LANG") || key.starts_with("LC_");
        if keep {
            command.env(key, value);
        }
    }
    for (key, value) in launch.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    if crate::cli::job_user().is_some() {
        command.umask(Some(crate::cli::JOB_UMASK));
    }
    if std::env::var("PATH").is_err() {
        command.env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
    }
    command.env("SHELL", &shell);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "Loka");
    let mut child = pair.slave.spawn_command(command).context("failed to start the login shell")?;
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().context("pty reader failed")?;
    let mut writer = pair.master.take_writer().context("pty writer failed")?;
    let master = pair.master;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; OUT_READ_MAX];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<Option<i32>>();
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code() as i32);
        let _ = exit_tx.send(code);
    });
    send_frame(&mut ws_write, &TermFrame::Hello { cols: job.cols as u16, rows: job.rows as u16, cwd: cwd.display().to_string() }).await?;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut bye: Option<String> = None;
    loop {
        tokio::select! {
            chunk = out_rx.recv() => {
                if let Some(bytes) = chunk {
                    send_frame(&mut ws_write, &TermFrame::Out { data: STANDARD.encode(bytes) }).await?;
                }
            }
            code = exit_rx.recv() => {
                bye = Some(match code.flatten() {
                    Some(code) if code != 0 => format!("shell exited ({code})"),
                    _ => "shell exited".to_string(),
                });
                if let Some(reason) = &bye {
                    let _ = send_frame(&mut ws_write, &TermFrame::Bye { reason: reason.clone() }).await;
                }
                break;
            }
            frame = ws_read.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let Ok(frame) = serde_json::from_str::<TermFrame>(text.as_str()) else { continue };
                    match frame {
                        TermFrame::In { data } => {
                            let _ = writer.write_all(data.as_bytes());
                            let _ = writer.flush();
                        }
                        TermFrame::Resize { cols, rows } => {
                            let _ = master.resize(PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 });
                        }
                        TermFrame::Close { .. } => break,
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if ws_write.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = killer.kill();
    if let Some(reason) = bye {
        tracing::debug!(%reason, "Terminal session ended");
    }
    Ok(())
}

pub(crate) async fn send_frame(write: &mut (impl SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin), frame: &TermFrame) -> Result<()> {
    let text = serde_json::to_string(frame)?;
    write.send(Message::Text(text.into())).await?;
    Ok(())
}
