mod checks;

use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewWindow};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use checks::{Arrival, MoveSummary, Moved, ProInfo, Ticket};

const WAIT_FOR_BROWSER: Duration = Duration::from_secs(20 * 60);
const READ_REQUEST: Duration = Duration::from_secs(10);
const REQUEST_HEAD_LIMIT: usize = 8 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MOVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
const RECOVER_EVERY: Duration = Duration::from_secs(10);
const RECOVER_FOR: Duration = Duration::from_secs(5 * 60);
const REPOSTS: u32 = 2;
const TOO_BIG: &str = "Your Loka is too big to move in one go";
const MOVE_FAILED: &str = "Could not move your Loka";
const UNREACHABLE: &str = "Could not reach Loka Pro. Check your connection";
const EXPIRED: &str = "This upgrade link has expired";
const EVENT: &str = "world:upgrade";

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UpgradeView {
    step: &'static str,
    org: Option<String>,
    version: Option<String>,
    done: Option<u64>,
    total: Option<u64>,
    error: Option<String>,
    agents: Option<u64>,
    reports: Option<u64>,
    keys: Option<usize>,
    renamed: Vec<String>,
    needs_values: Vec<String>,
    notes: Vec<String>,
    download_page: Option<String>,
    can_retry: bool,
    blocked: Option<&'static str>,
    login_note: bool,
}

#[derive(Default)]
pub(crate) struct Upgrade {
    flow: Mutex<Flow>,
}

#[derive(Default)]
struct Flow {
    view: UpgradeView,
    generation: u64,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    page: Option<String>,
}

const WORKING: &[&str] = &["waiting", "checking", "packing", "moving", "downloading", "verifying", "installing", "opening"];

fn working(step: &str) -> bool {
    WORKING.contains(&step)
}

fn idle() -> UpgradeView {
    UpgradeView { step: "idle", ..UpgradeView::default() }
}

fn current(app: &AppHandle) -> UpgradeView {
    let view = app.state::<Upgrade>().flow.lock().map(|flow| flow.view.clone()).unwrap_or_default();
    if view.step.is_empty() {
        idle()
    } else {
        view
    }
}

fn announce(app: &AppHandle, view: &UpgradeView) {
    if let (Some(window), Ok(detail)) = (app.get_webview_window("main"), serde_json::to_string(view)) {
        let _ = window.eval(format!("window.dispatchEvent(new CustomEvent('{EVENT}',{{detail:{detail}}}))"));
    }
}

fn show(app: &AppHandle, generation: u64, change: impl FnOnce(&mut Flow) -> UpgradeView) -> bool {
    let upgrade = app.state::<Upgrade>();
    let view = {
        let Ok(mut flow) = upgrade.flow.lock() else {
            return false;
        };
        if flow.generation != generation {
            return false;
        }
        let view = change(&mut flow);
        flow.view = view.clone();
        view
    };
    eprintln!("Upgrade to Pro: {}", view.step);
    announce(app, &view);
    true
}

fn step(app: &AppHandle, generation: u64, name: &'static str) -> bool {
    show(app, generation, |flow| UpgradeView { step: name, error: None, done: None, total: None, ..flow.view.clone() })
}

fn fail_move(app: &AppHandle, generation: u64, error: String) {
    show(app, generation, |flow| UpgradeView { step: "move_failed", error: Some(error), ..flow.view.clone() });
}

#[tauri::command]
pub(crate) async fn upgrade_start(window: WebviewWindow) -> Result<UpgradeView, String> {
    crate::native::require_trusted(&window)?;
    start(window.app_handle()).await
}

#[tauri::command]
pub(crate) fn upgrade_status(window: WebviewWindow) -> Result<UpgradeView, String> {
    crate::native::require_trusted(&window)?;
    Ok(current(window.app_handle()))
}

#[tauri::command]
pub(crate) fn upgrade_cancel(window: WebviewWindow) -> Result<UpgradeView, String> {
    crate::native::require_trusted(&window)?;
    let app = window.app_handle();
    let upgrade = app.state::<Upgrade>();
    {
        let mut flow = upgrade.flow.lock().map_err(|_| "Loka could not read the upgrade.".to_string())?;
        if !matches!(flow.view.step, "waiting" | "checking") {
            return Err("The move already started, so it can't be cancelled.".into());
        }
        if let Some(task) = flow.task.take() {
            task.abort();
        }
        flow.generation += 1;
        flow.page = None;
        flow.view = idle();
    }
    let view = current(app);
    announce(app, &view);
    Ok(view)
}

#[tauri::command]
pub(crate) async fn upgrade_retry(window: WebviewWindow, action: String) -> Result<UpgradeView, String> {
    crate::native::require_trusted(&window)?;
    let app = window.app_handle();
    let view = current(app);
    match (action.as_str(), view.step) {
        ("page", "waiting") => {
            let page = app.state::<Upgrade>().flow.lock().ok().and_then(|flow| flow.page.clone());
            if let Some(page) = page {
                open::that(page).map_err(|e| e.to_string())?;
            }
            Ok(current(app))
        }
        ("download", _) => {
            if let Some(page) = view.download_page.clone() {
                open::that(page).map_err(|e| e.to_string())?;
            }
            Ok(current(app))
        }
        ("again", "move_failed" | "timed_out" | "idle") => start(app).await,
        ("close", step) if !working(step) => {
            if let Ok(mut flow) = app.state::<Upgrade>().flow.lock() {
                flow.generation += 1;
                flow.view = idle();
            }
            Ok(current(app))
        }
        _ => Err("That isn't possible right now.".into()),
    }
}

async fn start(app: &AppHandle) -> Result<UpgradeView, String> {
    let site = checks::pro_site();
    let state = checks::random_hex(32).map_err(|_| "Loka could not start the upgrade.".to_string())?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|_| "Loka could not start the upgrade.".to_string())?;
    let port = listener.local_addr().map_err(|_| "Loka could not start the upgrade.".to_string())?.port();
    let page = checks::upgrade_url(&site, port, &state).ok_or_else(|| "Loka could not start the upgrade.".to_string())?;
    let upgrade = app.state::<Upgrade>();
    let generation = {
        let mut flow = upgrade.flow.lock().map_err(|_| "Loka could not read the upgrade.".to_string())?;
        if working(flow.view.step) {
            return Err("An upgrade is already running.".into());
        }
        flow.generation += 1;
        flow.page = Some(page.clone());
        flow.view = UpgradeView { step: "waiting", ..UpgradeView::default() };
        flow.generation
    };
    let runner = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        run(runner, generation, listener, state, site).await;
    });
    if let Ok(mut flow) = app.state::<Upgrade>().flow.lock() {
        if flow.generation == generation {
            flow.task = Some(task);
        } else {
            task.abort();
        }
    }
    let view = current(app);
    announce(app, &view);
    open::that(&page).map_err(|_| "Loka could not open your browser.".to_string())?;
    Ok(view)
}

async fn run(app: AppHandle, generation: u64, listener: TcpListener, state: String, site: String) {
    let Some(ticket) = receive(listener, &state, &site).await else {
        show(&app, generation, |flow| UpgradeView { step: "timed_out", ..flow.view.clone() });
        return;
    };
    let org = ticket.org.clone();
    if !show(&app, generation, |flow| {
        flow.page = None;
        UpgradeView { step: "checking", org: Some(org), ..UpgradeView::default() }
    }) {
        return;
    }
    let client = match pro_client() {
        Ok(client) => client,
        Err(error) => return fail_move(&app, generation, error),
    };
    let info = match pro_info(&client, &ticket).await {
        Ok(info) => info,
        Err(error) => return fail_move(&app, generation, error),
    };
    if info.moved {
        return moved(&app, generation, info, None).await;
    }
    if !step(&app, generation, "packing") {
        return;
    }
    if let Ok(mut flow) = app.state::<Upgrade>().flow.lock() {
        if flow.generation == generation {
            flow.task = None;
        }
    }
    let body = match pack(&app).await {
        Ok(body) => body,
        Err(error) => return fail_move(&app, generation, error),
    };
    if !step(&app, generation, "moving") {
        return;
    }
    match send_move(&client, &ticket, &body).await {
        Ok(done) => moved(&app, generation, done.next, Some(done.summary)).await,
        Err(error) => fail_move(&app, generation, error),
    }
}

async fn receive(listener: TcpListener, state: &str, site: &str) -> Option<Ticket> {
    let deadline = tokio::time::Instant::now() + WAIT_FOR_BROWSER;
    loop {
        let accepted = tokio::time::timeout_at(deadline, listener.accept()).await.ok()?;
        let Ok((mut stream, peer)) = accepted else {
            continue;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(Some(head)) = tokio::time::timeout(READ_REQUEST, read_head(&mut stream)).await else {
            continue;
        };
        let found = match checks::request_target(&head) {
            Some((method, target)) => checks::arrival(method, target, state, site),
            None => Arrival::NotOurs,
        };
        match &found {
            Arrival::WrongState => eprintln!("Upgrade to Pro: refused a return with the wrong state"),
            Arrival::Invalid => eprintln!("Upgrade to Pro: refused a return with unreadable fields"),
            _ => {}
        }
        let _ = stream.write_all(&checks::reply_for(&found)).await;
        let _ = stream.shutdown().await;
        if let Arrival::Ticket(ticket) = found {
            return Some(ticket);
        }
    }
}

async fn read_head(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        if buffer.len() >= REQUEST_HEAD_LIMIT {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(buffer).ok()
}

fn pro_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(!cfg!(debug_assertions))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .user_agent("Loka")
        .build()
        .map_err(|_| "Loka could not start the upgrade.".to_string())
}

fn bearer(ticket: &Ticket) -> Result<reqwest::header::HeaderValue, String> {
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", ticket.ticket)).map_err(|_| EXPIRED.to_string())?;
    value.set_sensitive(true);
    Ok(value)
}

enum Answer {
    Info(Box<ProInfo>),
    Refused(u16, Option<String>),
    Unreachable,
}

async fn ask_pro(client: &reqwest::Client, ticket: &Ticket) -> Answer {
    let Ok(auth) = bearer(ticket) else {
        return Answer::Refused(401, None);
    };
    let sent = client
        .get(format!("{}/api/move/pro", ticket.api))
        .header(reqwest::header::AUTHORIZATION, auth)
        .timeout(CALL_TIMEOUT)
        .send()
        .await;
    let Ok(response) = sent else {
        return Answer::Unreachable;
    };
    let status = response.status().as_u16();
    eprintln!("Upgrade to Pro: GET /api/move/pro {status}");
    let Ok(bytes) = response.bytes().await else {
        return Answer::Unreachable;
    };
    if status == 200 {
        return match serde_json::from_slice::<ProInfo>(&bytes) {
            Ok(info) => Answer::Info(Box::new(info)),
            Err(_) => Answer::Refused(status, Some("Loka Pro sent an answer Loka could not read".into())),
        };
    }
    Answer::Refused(status, checks::server_error(&bytes))
}

async fn pro_info(client: &reqwest::Client, ticket: &Ticket) -> Result<ProInfo, String> {
    match ask_pro(client, ticket).await {
        Answer::Info(info) => Ok(*info),
        Answer::Refused(401, error) => Err(error.unwrap_or_else(|| EXPIRED.into())),
        Answer::Refused(status, error) => Err(error.unwrap_or_else(|| format!("Loka Pro did not answer ({status})"))),
        Answer::Unreachable => Err(UNREACHABLE.into()),
    }
}

async fn pack(app: &AppHandle) -> Result<Vec<u8>, String> {
    let mut packed = crate::native::move_out(app).await?;
    let mut value: serde_json::Value = serde_json::from_slice(&packed).map_err(|_| "Loka could not pack your Loka.".to_string())?;
    packed.fill(0);
    if !value.get("bundle").is_some_and(serde_json::Value::is_object) {
        return Err("Loka could not pack your Loka.".into());
    }
    value["make_main"] = serde_json::Value::Bool(true);
    let mut json = serde_json::to_vec(&value).map_err(|_| "Loka could not pack your Loka.".to_string())?;
    drop(value);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let written = encoder.write_all(&json);
    json.fill(0);
    written.map_err(|_| "Loka could not pack your Loka.".to_string())?;
    encoder.finish().map_err(|_| "Loka could not pack your Loka.".to_string())
}

fn drop_reply_for_testing() -> bool {
    cfg!(debug_assertions) && std::env::var("LOKA_UPGRADE_DROP_REPLY").is_ok_and(|v| v == "1")
}

enum Posted {
    Answer(u16, Vec<u8>),
    Lost,
}

async fn post_move(client: &reqwest::Client, ticket: &Ticket, body: &[u8], drop_reply: bool) -> Posted {
    let Ok(auth) = bearer(ticket) else {
        return Posted::Answer(401, Vec::new());
    };
    let sent = client
        .post(format!("{}/api/move", ticket.api))
        .header(reqwest::header::AUTHORIZATION, auth)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .timeout(MOVE_TIMEOUT)
        .body(body.to_vec())
        .send()
        .await;
    let Ok(response) = sent else {
        eprintln!("Upgrade to Pro: POST /api/move got no answer");
        return Posted::Lost;
    };
    let status = response.status().as_u16();
    eprintln!("Upgrade to Pro: POST /api/move {status}");
    if drop_reply {
        return Posted::Lost;
    }
    match response.bytes().await {
        Ok(bytes) => Posted::Answer(status, bytes.to_vec()),
        Err(_) => Posted::Lost,
    }
}

async fn send_move(client: &reqwest::Client, ticket: &Ticket, body: &[u8]) -> Result<Moved, String> {
    let mut rate_limited = false;
    let mut reposts = 0;
    let mut drop_reply = drop_reply_for_testing();
    loop {
        let posted = post_move(client, ticket, body, drop_reply).await;
        drop_reply = false;
        match posted {
            Posted::Answer(200, bytes) => {
                return serde_json::from_slice::<Moved>(&bytes).map_err(|_| "Loka Pro moved your Loka but sent an answer Loka could not read".to_string());
            }
            Posted::Answer(429, _) if !rate_limited => {
                rate_limited = true;
                tokio::time::sleep(RATE_LIMIT_WAIT).await;
            }
            Posted::Answer(413, _) => return Err(TOO_BIG.into()),
            Posted::Answer(_, bytes) => return Err(checks::server_error(&bytes).unwrap_or_else(|| MOVE_FAILED.into())),
            Posted::Lost => match recover(client, ticket).await {
                Recovered::Moved(next) => return Ok(Moved { summary: MoveSummary::default(), next: *next }),
                Recovered::NotMoved if reposts < REPOSTS => reposts += 1,
                Recovered::NotMoved | Recovered::Gone => return Err(MOVE_FAILED.into()),
            },
        }
    }
}

enum Recovered {
    Moved(Box<ProInfo>),
    NotMoved,
    Gone,
}

async fn recover(client: &reqwest::Client, ticket: &Ticket) -> Recovered {
    let deadline = tokio::time::Instant::now() + RECOVER_FOR;
    loop {
        tokio::time::sleep(RECOVER_EVERY).await;
        match ask_pro(client, ticket).await {
            Answer::Info(info) if info.moved => return Recovered::Moved(info),
            Answer::Info(_) => return Recovered::NotMoved,
            _ if tokio::time::Instant::now() >= deadline => return Recovered::Gone,
            _ => {}
        }
    }
}

fn summary_view(view: UpgradeView, summary: Option<&MoveSummary>) -> UpgradeView {
    match summary {
        Some(summary) => UpgradeView {
            agents: Some(summary.counts.agents),
            reports: Some(summary.counts.reports),
            keys: Some(summary.keys_moved.len()),
            renamed: summary.renamed.clone(),
            needs_values: summary.needs_values.clone(),
            notes: summary.notes.clone(),
            ..view
        },
        None => view,
    }
}

async fn moved(app: &AppHandle, generation: u64, next: ProInfo, summary: Option<MoveSummary>) {
    let site = checks::pro_site();
    let page = checks::download_page(&next, &site);
    let version = next.pro.as_ref().map(|pro| pro.version.clone());
    show(app, generation, |flow| summary_view(UpgradeView { step: "moved", download_page: Some(page), version, ..flow.view.clone() }, summary.as_ref()));
}
