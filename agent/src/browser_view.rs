use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use loka_shared::terminal::{TerminalJob, TermFrame};

use crate::remote_runner::RunnerApi;
use crate::terminal::{connect_api, send_frame};

pub const NOT_RUNNING: &str = "not_running";
pub const CLOSED: &str = "closed";
const LOOK_AGAIN: Duration = Duration::from_secs(3);
const PING_EVERY: Duration = Duration::from_secs(25);
const MAX_FPS: u32 = 10;
const PASTE_MAX_CHARS: usize = 2000;

#[derive(Debug, PartialEq)]
enum FromBrowser {
    Frame { frame: TermFrame, seq: u64 },
    Page(TermFrame),
    Showing(bool),
}

fn from_browser(text: &str) -> Option<FromBrowser> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value.get("type")?.as_str()? {
        "frame" => {
            let seq = value.get("seq").and_then(Value::as_u64).unwrap_or(0);
            let data = value.get("data")?.as_str()?.to_string();
            let meta = value.get("metadata");
            let size = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_f64).map(|v| v.round().max(0.0) as u32).unwrap_or(0);
            Some(FromBrowser::Frame { frame: TermFrame::Frame { seq, data, width: size("deviceWidth"), height: size("deviceHeight") }, seq })
        }
        "url" => Some(FromBrowser::Page(TermFrame::Page { url: value.get("url")?.as_str()?.to_string() })),
        "status" => value.get("screencasting").and_then(Value::as_bool).map(FromBrowser::Showing),
        _ => None,
    }
}

fn to_browser(frame: &TermFrame) -> Vec<Value> {
    match frame {
        TermFrame::Mouse { event, x, y, button, clicks, dx, dy, modifiers } => {
            let mut message = json!({ "type": "input_mouse", "eventType": event, "x": x, "y": y, "modifiers": modifiers });
            if let Some(button) = button {
                message["button"] = json!(button);
            }
            if let Some(clicks) = clicks {
                message["clickCount"] = json!(clicks);
            }
            if dx.is_some() || dy.is_some() {
                message["deltaX"] = json!(dx.unwrap_or(0.0));
                message["deltaY"] = json!(dy.unwrap_or(0.0));
            }
            vec![message]
        }
        TermFrame::Key { event, key, code, text, key_code, modifiers } => {
            let mut message = json!({ "type": "input_keyboard", "eventType": event, "key": key, "modifiers": modifiers });
            if let Some(code) = code {
                message["code"] = json!(code);
            }
            if let Some(text) = text {
                message["text"] = json!(text);
            }
            if let Some(key_code) = key_code {
                message["windowsVirtualKeyCode"] = json!(key_code);
            }
            vec![message]
        }
        TermFrame::Paste { text } => text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(PASTE_MAX_CHARS)
            .map(|c| json!({ "type": "input_keyboard", "eventType": "char", "key": c.to_string(), "text": if c == '\n' { "\r".to_string() } else { c.to_string() } }))
            .collect(),
        _ => Vec::new(),
    }
}

enum FromViewer {
    Gone,
    Input(TermFrame),
    Nothing,
}

fn from_viewer(message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>) -> FromViewer {
    match message {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<TermFrame>(text.as_str()) {
            Ok(TermFrame::Close { .. }) => FromViewer::Gone,
            Ok(frame) => FromViewer::Input(frame),
            Err(_) => FromViewer::Nothing,
        },
        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => FromViewer::Gone,
        Some(Ok(_)) => FromViewer::Nothing,
    }
}

async fn find_port(root: &Path, job: &TerminalJob, agent_id: Uuid, own: bool) -> Option<u16> {
    let (root, view, quest) = (root.to_path_buf(), job.id, job.task_id);
    tokio::task::spawn_blocking(move || crate::browser::stream_port(&root, agent_id, view, own, quest)).await.ok().flatten()
}

async fn open_page(root: PathBuf, agent_id: Uuid, view: Uuid, url: String) -> Option<String> {
    match tokio::task::spawn_blocking(move || crate::browser::open_for_owner(&root, agent_id, view, &url)).await {
        Ok(Ok(())) => None,
        Ok(Err(refusal)) => Some(format!("Could not open that page: {refusal}")),
        Err(_) => Some("Could not open that page".to_string()),
    }
}

pub async fn run_session(api: &RunnerApi, root: &Path, job: TerminalJob) -> Result<()> {
    let agent_id = job.agent_id.context("browser view without an agent")?;
    let watched = watch(api, root, &job, agent_id).await;
    let (root, view) = (root.to_path_buf(), job.id);
    let _ = tokio::task::spawn_blocking(move || crate::browser::close_run(&root, agent_id, view)).await;
    watched
}

async fn watch(api: &RunnerApi, root: &Path, job: &TerminalJob, agent_id: Uuid) -> Result<()> {
    let (mut viewer_write, mut viewer_read) = connect_api(api, job.id).await?.split();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut told: Option<&str> = None;
    let mut own = false;
    'session: loop {
        let local = match find_port(root, job, agent_id, own).await {
            Some(port) => tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/?pacing=ack&maxFps={MAX_FPS}")).await.ok(),
            None => None,
        };
        let Some((local, _)) = local else {
            let reason = told.unwrap_or(NOT_RUNNING);
            if told.is_none() {
                send_frame(&mut viewer_write, &TermFrame::Idle { reason: reason.to_string() }).await?;
                told = Some(reason);
            }
            let wait = tokio::time::sleep(LOOK_AGAIN);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    message = viewer_read.next() => match from_viewer(message) {
                        FromViewer::Gone => break 'session,
                        FromViewer::Input(TermFrame::Open { url }) => {
                            match open_page(root.to_path_buf(), agent_id, job.id, url).await {
                                Some(message) => send_frame(&mut viewer_write, &TermFrame::Error { message }).await?,
                                None => own = true,
                            }
                            break;
                        }
                        _ => {}
                    },
                    _ = ping.tick() => viewer_write.send(Message::Ping(Vec::new().into())).await?,
                }
            }
            continue;
        };
        let (mut browser_write, mut browser_read) = local.split();
        loop {
            tokio::select! {
                message = browser_read.next() => match message {
                    Some(Ok(Message::Text(text))) => match from_browser(text.as_str()) {
                        Some(FromBrowser::Frame { frame, seq }) => {
                            if frame.runner_output_fits() {
                                send_frame(&mut viewer_write, &frame).await?;
                            }
                            let _ = browser_write.send(Message::Text(json!({ "type": "ack", "seq": seq }).to_string().into())).await;
                        }
                        Some(FromBrowser::Page(page)) => send_frame(&mut viewer_write, &page).await?,
                        Some(FromBrowser::Showing(false)) => {
                            send_frame(&mut viewer_write, &TermFrame::Idle { reason: NOT_RUNNING.to_string() }).await?;
                        }
                        Some(FromBrowser::Showing(true)) | None => {}
                    },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        send_frame(&mut viewer_write, &TermFrame::Idle { reason: CLOSED.to_string() }).await?;
                        told = Some(CLOSED);
                        break;
                    }
                    Some(Ok(_)) => {}
                },
                message = viewer_read.next() => match from_viewer(message) {
                    FromViewer::Gone => break 'session,
                    FromViewer::Input(TermFrame::Open { url }) => {
                        match open_page(root.to_path_buf(), agent_id, job.id, url).await {
                            Some(message) => send_frame(&mut viewer_write, &TermFrame::Error { message }).await?,
                            None if !own => {
                                own = true;
                                continue 'session;
                            }
                            None => {}
                        }
                    }
                    FromViewer::Input(frame) => {
                        for message in to_browser(&frame) {
                            if browser_write.send(Message::Text(message.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    FromViewer::Nothing => {}
                },
                _ = ping.tick() => viewer_write.send(Message::Ping(Vec::new().into())).await?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_from_the_browser_stream_become_view_frames() {
        let frame = r#"{"type":"frame","seq":41,"data":"AAAA","metadata":{"deviceWidth":1280,"deviceHeight":720.4,"pageScaleFactor":1}}"#;
        assert_eq!(
            from_browser(frame),
            Some(FromBrowser::Frame { frame: TermFrame::Frame { seq: 41, data: "AAAA".into(), width: 1280, height: 720 }, seq: 41 })
        );
        assert_eq!(from_browser(r#"{"type":"url","url":"https://example.com/a"}"#), Some(FromBrowser::Page(TermFrame::Page { url: "https://example.com/a".into() })));
        assert_eq!(from_browser(r#"{"type":"status","screencasting":false,"engine":"chrome"}"#), Some(FromBrowser::Showing(false)));
        assert_eq!(from_browser(r#"{"type":"status","engine":"chrome"}"#), None);
        assert_eq!(from_browser(r#"{"type":"console","text":"hi"}"#), None);
        assert_eq!(from_browser(r#"{"type":"frame","seq":1}"#), None);
        assert_eq!(from_browser("not json"), None);
    }

    #[test]
    fn viewer_input_becomes_browser_stream_input() {
        let click = TermFrame::Mouse { event: "mousePressed".into(), x: 10.0, y: 20.0, button: Some("left".into()), clicks: Some(1), dx: None, dy: None, modifiers: 8 };
        assert_eq!(
            to_browser(&click),
            vec![json!({"type": "input_mouse", "eventType": "mousePressed", "x": 10.0, "y": 20.0, "button": "left", "clickCount": 1, "modifiers": 8})]
        );
        let wheel = TermFrame::Mouse { event: "mouseWheel".into(), x: 1.0, y: 2.0, button: None, clicks: None, dx: None, dy: Some(120.0), modifiers: 0 };
        assert_eq!(to_browser(&wheel)[0]["deltaY"], json!(120.0));
        assert_eq!(to_browser(&wheel)[0]["deltaX"], json!(0.0));
        let key = TermFrame::Key { event: "keyDown".into(), key: "Backspace".into(), code: Some("Backspace".into()), text: None, key_code: Some(8), modifiers: 0 };
        assert_eq!(
            to_browser(&key),
            vec![json!({"type": "input_keyboard", "eventType": "keyDown", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8, "modifiers": 0})]
        );
        let paste = to_browser(&TermFrame::Paste { text: "a\u{7}b\n".into() });
        assert_eq!(paste.iter().map(|m| m["text"].as_str().unwrap()).collect::<Vec<_>>(), vec!["a", "b", "\r"]);
        assert!(paste.iter().all(|m| m["eventType"] == "char"));
        assert_eq!(to_browser(&TermFrame::Paste { text: "x".repeat(5000) }).len(), PASTE_MAX_CHARS);
        assert!(to_browser(&TermFrame::In { data: "ls\r".into() }).is_empty());
        assert!(to_browser(&TermFrame::Open { url: "https://example.com".into() }).is_empty());
    }
}
