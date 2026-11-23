use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::mpsc;
use uuid::Uuid;

use loka_shared::terminal::{idle_secs, TerminalSession, TermFrame};
use loka_shared::world::WorldStore;

pub const TICKET_TTL_SECS: i64 = 120;
const REFRESH: Duration = Duration::from_secs(30);

type Tx = mpsc::UnboundedSender<String>;
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TermGrant {
    pub user: Uuid,
    pub session: Uuid,
    pub runner: Uuid,
    pub exp: i64,
}

pub fn sign(grant: &TermGrant, secret: &[u8]) -> String {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(grant).expect("grant serializes"));
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(body.as_bytes());
    format!("{body}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub fn verify(ticket: &str, secret: &[u8], now: i64) -> Option<TermGrant> {
    let (body, sig) = ticket.split_once('.')?;
    let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
    let mut mac = HmacSha256::new_from_slice(secret).ok()?;
    mac.update(body.as_bytes());
    mac.verify_slice(&sig).ok()?;
    let grant: TermGrant = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    (grant.exp >= now).then_some(grant)
}

pub fn ticket_secret() -> Vec<u8> {
    use sha2::Digest;
    for name in ["WORLD_WORKSHOP_SECRET", "AUTH_SECRET"] {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                return Sha256::digest(format!("terminal:{value}")).to_vec();
            }
        }
    }
    static RANDOM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    RANDOM
        .get_or_init(|| {
            tracing::warn!("Neither WORLD_WORKSHOP_SECRET nor AUTH_SECRET is set; tickets only work on this API replica");
            uuid::Uuid::new_v4().as_bytes().iter().chain(uuid::Uuid::new_v4().as_bytes()).copied().collect()
        })
        .clone()
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<Uuid, SessionLinks>,
}

struct SessionLinks {
    runner: Option<(Uuid, Tx)>,
    browser: Option<(Uuid, Tx)>,
    last_activity: Instant,
    scrollback: std::collections::VecDeque<Value>,
    scroll_bytes: usize,
    latest: Vec<Value>,
}

const LATEST_ONLY: [&str; 3] = ["page", "idle", "frame"];
const SCROLLBACK_MAX_BYTES: usize = 64 * 1024;
const SCROLLBACK_MAX_FRAMES: usize = 400;

impl SessionLinks {
    fn new() -> Self {
        Self { runner: None, browser: None, last_activity: Instant::now(), scrollback: std::collections::VecDeque::new(), scroll_bytes: 0, latest: Vec::new() }
    }

    fn push_scrollback(&mut self, frame: &Value) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        if LATEST_ONLY.contains(&kind) {
            self.latest.retain(|old| old.get("type").and_then(Value::as_str) != Some(kind));
            self.latest.push(frame.clone());
            return;
        }
        if kind != "out" && kind != "hello" {
            return;
        }
        let bytes = frame.get("data").and_then(Value::as_str).map(str::len).unwrap_or(1) + 1;
        self.scroll_bytes += bytes;
        self.scrollback.push_back(frame.clone());
        while self.scroll_bytes > SCROLLBACK_MAX_BYTES || self.scrollback.len() > SCROLLBACK_MAX_FRAMES {
            if let Some(old) = self.scrollback.pop_front() {
                self.scroll_bytes = self.scroll_bytes.saturating_sub(old.get("data").and_then(Value::as_str).map(str::len).unwrap_or(1) + 1);
            }
        }
    }

    fn take_scrollback(&mut self) -> Vec<Value> {
        self.scroll_bytes = 0;
        self.scrollback.drain(..).chain(self.latest.drain(..)).collect()
    }
}

pub struct TermHub {
    inner: Mutex<Inner>,
}

impl TermHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(Inner::default()) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn touch(&self, session: Uuid) {
        if let Some(links) = self.lock().sessions.get_mut(&session) {
            links.last_activity = Instant::now();
        }
    }

    pub fn idle_for(&self, session: Uuid) -> Duration {
        self.lock()
            .sessions
            .get(&session)
            .map(|l| l.last_activity.elapsed())
            .unwrap_or_default()
    }

    pub fn has_runner(&self, session: Uuid) -> bool {
        self.lock().sessions.get(&session).and_then(|l| l.runner.as_ref()).is_some()
    }

    async fn is_watched(&self, session: Uuid) -> bool {
        self.lock().sessions.get(&session).and_then(|l| l.browser.as_ref()).is_some()
    }

    pub fn is_live(&self, session: Uuid) -> bool {
        self.has_runner(session)
    }

    fn register(&self, session: Uuid, runner_side: bool) -> (Uuid, Tx, mpsc::UnboundedReceiver<String>, Vec<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = Uuid::new_v4();
        let mut inner = self.lock();
        let links = inner.sessions.entry(session).or_insert_with(SessionLinks::new);
        let slot = if runner_side { &mut links.runner } else { &mut links.browser };
        *slot = Some((conn, tx.clone()));
        let replay = if runner_side { Vec::new() } else { links.take_scrollback() };
        (conn, tx, rx, replay)
    }

    fn unregister(&self, session: Uuid, runner_side: bool, conn: Uuid) {
        let mut inner = self.lock();
        if let Some(links) = inner.sessions.get_mut(&session) {
            let slot = if runner_side { &mut links.runner } else { &mut links.browser };
            if slot.as_ref().is_some_and(|(id, _)| *id == conn) {
                *slot = None;
            }
            if links.runner.is_none() && links.browser.is_none() {
                inner.sessions.remove(&session);
            }
        }
    }

    async fn forward(&self, session: Uuid, to_runner: bool, frame: Value) {
        self.touch(session);
        let local = {
            let mut inner = self.lock();
            if !to_runner {
                if let Some(links) = inner.sessions.get_mut(&session) {
                    links.push_scrollback(&frame);
                }
            }
            inner.sessions.get(&session).and_then(|l| {
                let slot = if to_runner { &l.runner } else { &l.browser };
                slot.as_ref().map(|(_, tx)| tx.clone())
            })
        };
        if let Some(tx) = local {
            let _ = tx.send(frame.to_string());
        }
    }

    pub async fn on_browser_frame(&self, session: Uuid, frame: Value) {
        self.forward(session, true, frame).await;
    }

    pub async fn on_runner_frame(&self, session: Uuid, frame: Value) {
        self.forward(session, false, frame).await;
    }

}

const VIEWER_DRAIN_MAX: usize = 256;
const UNWATCHED_GRACE: Duration = Duration::from_secs(60);

fn unwatched_too_long(since: &mut Option<Instant>, watched: bool, now: Instant) -> bool {
    if watched {
        *since = None;
        return false;
    }
    now.duration_since(*since.get_or_insert(now)) > UNWATCHED_GRACE
}


fn is_picture(frame: &str) -> bool {
    #[derive(Deserialize)]
    struct Kind<'a> {
        #[serde(rename = "type", borrow)]
        kind: &'a str,
    }
    serde_json::from_str::<Kind>(frame).is_ok_and(|k| k.kind == "frame")
}

fn newest_picture_only(waiting: Vec<String>) -> Vec<String> {
    let newest = waiting.iter().rposition(|frame| is_picture(frame));
    waiting.into_iter().enumerate().filter(|(i, frame)| Some(*i) == newest || !is_picture(frame)).map(|(_, frame)| frame).collect()
}

fn frame_value(frame: &TermFrame) -> Value {
    serde_json::to_value(frame).unwrap_or(Value::Null)
}

pub async fn serve_runner(socket: WebSocket, hub: Arc<TermHub>, world: WorldStore, runner: Uuid, session: TerminalSession) {
    let (conn, _tx, mut rx, _replay) = hub.register(session.id, true);
    let _ = world.touch_terminal(session.id).await;
    hub.on_runner_frame(session.id, frame_value(&TermFrame::Status { state: "live".to_string(), device: None })).await;
    let (mut write, mut read) = socket.split();
    let mut refresh = tokio::time::interval(REFRESH);
    refresh.tick().await;
    let idle_limit = Duration::from_secs(idle_secs());
    let mut idle_tick = tokio::time::interval(idle_limit.min(Duration::from_secs(5)).max(Duration::from_secs(1)));
    idle_tick.tick().await;
    let mut close_reason: Option<String> = None;
    let mut unwatched_since: Option<Instant> = None;
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(frame) = serde_json::from_str::<TermFrame>(text.as_str()) else { continue };
                    match &frame {
                        TermFrame::Bye { reason } => {
                            hub.on_runner_frame(session.id, frame_value(&frame)).await;
                            close_reason = Some(reason.clone());
                            break;
                        }
                        _ if frame.runner_output_fits() => hub.on_runner_frame(session.id, frame_value(&frame)).await,
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            outgoing = rx.recv() => match outgoing {
                Some(frame) => {
                    if write.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = refresh.tick() => {
                if !hub.has_runner(session.id) {
                    break;
                }
                let _ = world.touch_terminal(session.id).await;
            }
            _ = idle_tick.tick() => {
                let reason = if hub.idle_for(session.id) > idle_limit {
                    Some("idle")
                } else if unwatched_too_long(&mut unwatched_since, hub.is_watched(session.id).await, Instant::now()) {
                    Some("unwatched")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    close_reason = Some(reason.to_string());
                    hub.on_runner_frame(session.id, frame_value(&TermFrame::Close { reason: Some(reason.to_string()) })).await;
                    break;
                }
            }
        }
    }
    hub.unregister(session.id, true, conn);
    let reason = close_reason.unwrap_or_else(|| "runner_closed".to_string());
    let _ = world.close_terminal(session.id, &reason).await;
    hub.on_runner_frame(session.id, frame_value(&TermFrame::Closed { reason: reason.clone() })).await;
    tracing::info!(target: "terminal_audit", session = %session.id, %runner, %reason, "terminal: device disconnected");
}

pub async fn serve_browser(socket: WebSocket, hub: Arc<TermHub>, world: WorldStore, grant: TermGrant, device: String) {
    let (conn, _tx, mut rx, replay) = hub.register(grant.session, false);
    let (mut write, mut read) = socket.split();
    for frame in &replay {
        if write.send(Message::Text(frame.to_string().into())).await.is_err() {
            break;
        }
    }
    let state = if hub.is_live(grant.session) { "live" } else { "connecting" };
    let _ = write
        .send(Message::Text(frame_value(&TermFrame::Status { state: state.to_string(), device: Some(device) }).to_string().into()))
        .await;
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(frame) = serde_json::from_str::<TermFrame>(text.as_str()) else { continue };
                    match &frame {
                        TermFrame::Close { .. } => break,
                        _ if frame.viewer_input_fits() => hub.on_browser_frame(grant.session, frame_value(&frame)).await,
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            outgoing = rx.recv() => match outgoing {
                Some(first) => {
                    let mut waiting = vec![first];
                    while waiting.len() < VIEWER_DRAIN_MAX {
                        let Ok(more) = rx.try_recv() else { break };
                        waiting.push(more);
                    }
                    let mut ended = false;
                    for frame in newest_picture_only(waiting) {
                        ended = matches!(serde_json::from_str::<TermFrame>(&frame), Ok(TermFrame::Closed { .. }));
                        if write.send(Message::Text(frame.into())).await.is_err() {
                            break;
                        }
                        if ended {
                            break;
                        }
                    }
                    if ended {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        break;
                    }
                }
                None => break,
            },
        }
    }
    hub.unregister(grant.session, false, conn);
    let reason = "browser_closed".to_string();
    let _ = world.close_terminal(grant.session, &reason).await;
    hub.on_browser_frame(grant.session, frame_value(&TermFrame::Close { reason: Some(reason.clone()) })).await;
    tracing::info!(target: "terminal_audit", user = %grant.user, session = %grant.session, runner = %grant.runner, %reason, "terminal: browser disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(seq: u64) -> String {
        frame_value(&TermFrame::Frame { seq, data: "AAAA".into(), width: 10, height: 10 }).to_string()
    }

    #[tokio::test]
    async fn a_view_nobody_watches_closes_after_the_grace_and_a_watched_one_stays() {
        let hub = TermHub::new();
        let session = Uuid::new_v4();
        let (runner, ..) = hub.register(session, true);
        assert!(!hub.is_watched(session).await);
        let (viewer, ..) = hub.register(session, false);
        assert!(hub.is_watched(session).await);
        hub.unregister(session, false, viewer);
        assert!(!hub.is_watched(session).await);
        hub.unregister(session, true, runner);

        let start = Instant::now();
        let mut since = None;
        assert!(!unwatched_too_long(&mut since, false, start));
        assert!(!unwatched_too_long(&mut since, false, start + UNWATCHED_GRACE));
        assert!(unwatched_too_long(&mut since, false, start + UNWATCHED_GRACE + Duration::from_secs(1)));
        let mut since = None;
        assert!(!unwatched_too_long(&mut since, false, start));
        assert!(!unwatched_too_long(&mut since, true, start + UNWATCHED_GRACE));
        assert!(!unwatched_too_long(&mut since, false, start + UNWATCHED_GRACE * 2));
        assert!(!unwatched_too_long(&mut since, true, start + UNWATCHED_GRACE * 10));
    }

    #[test]
    fn a_slow_viewer_gets_only_the_newest_picture_and_every_other_message() {
        let page = frame_value(&TermFrame::Page { url: "https://example.com/\"type\":\"frame\"".into() }).to_string();
        let out = frame_value(&TermFrame::Out { data: "aGk=".into() }).to_string();
        let kept = newest_picture_only(vec![picture(1), page.clone(), picture(2), out.clone(), picture(3)]);
        assert_eq!(kept, vec![page.clone(), out.clone(), picture(3)]);
        assert_eq!(newest_picture_only(vec![page.clone(), out.clone()]), vec![page, out]);
        assert_eq!(newest_picture_only(vec![picture(7)]), vec![picture(7)]);
        assert!(newest_picture_only(Vec::new()).is_empty());
    }
}
