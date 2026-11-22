use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use url::Url;
use uuid::Uuid;

use crate::local::{append_log, ensure_private_dir, write_private, Phase, Stack, Supervised};

const SLACK_KEY_FILE: &str = "slack-connection.key";
const SLACK_STATUS_FILE: &str = "slack-capture.json";
const SLACK_WATCH_REFRESH: Duration = Duration::from_secs(60);
const SLACK_PYTHON: &str = "/usr/bin/python3";
const FEATURES: &[&str] = &["workbench", "home_terminal", "folders", "local", "updates", "update_auto", "update_parts", "open_paths"];
const DOCS_URL: &str = "https://loka.ygpt.dev/docs";
const SLACK_IDENTITY_WAIT: Duration = Duration::from_secs(40);

struct NativeState {
    root: PathBuf,
    config_path: PathBuf,
    log_path: PathBuf,
    config: Mutex<NativeConfig>,
    runner: Arc<Supervised>,
    slack: Mutex<SlackWatcher>,
    slack_status: Mutex<SlackView>,
    slack_settled: tokio::sync::watch::Sender<bool>,
}

#[derive(Default)]
struct SlackWatcher {
    generation: u64,
    child: Option<Child>,
    identity: Option<serde_json::Value>,
}

impl SlackWatcher {
    fn replace(&mut self, child: Option<Child>) -> (u64, Option<Child>) {
        self.generation += 1;
        self.identity = None;
        (self.generation, std::mem::replace(&mut self.child, child))
    }

    fn owns(&self, generation: u64) -> bool {
        self.generation == generation && self.child.is_some()
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct NativeConfig {
    runner_id: Option<String>,
    name: String,
    paused: bool,
    repositories: Vec<RepositoryView>,
    #[serde(default)]
    slack_enabled: bool,
    #[serde(default)]
    auto_update: bool,
}

#[derive(Clone, Default, Serialize)]
struct SlackView {
    enabled: bool,
    running: bool,
    workspaces: Vec<String>,
    delivered: u64,
    last_error: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct RepositoryView {
    id: String,
    name: String,
    path: String,
}

#[derive(Serialize)]
struct BackendView {
    name: String,
    installed: bool,
    authenticated: Option<bool>,
}

#[derive(Serialize)]
struct DesktopStatus {
    available: bool,
    runner_id: Option<String>,
    name: String,
    status: String,
    server: String,
    workspace_root: String,
    repositories: Vec<RepositoryView>,
    backends: Vec<BackendView>,
    last_error: Option<String>,
    start_at_login: bool,
    slack: SlackView,
    features: Vec<&'static str>,
}

#[derive(Deserialize)]
struct RunnerRegistration {
    runner: RunnerView,
    token: String,
}

#[derive(Deserialize)]
struct RunnerView {
    id: String,
    name: String,
    status: String,
}

static WEB_ORIGIN: OnceLock<String> = OnceLock::new();
const APP_SESSION_HEADER: &str = "x-loka-app-session";

fn keep_awake() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let spawned = Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        eprintln!("caffeinate failed to start: {e}");
    }
}

pub fn run() {
    let current_path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    std::env::set_var(
        "PATH",
        desktop_path(current_path.as_deref(), home.as_deref()),
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main(app);
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(MacosLauncher::LaunchAgent)
                .build(),
        )
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            desktop_status,
            pair_device,
            pause_runner,
            resume_runner,
            disconnect_device,
            choose_repository,
            open_runner_logs,
            set_start_at_login,
            set_slack_capture,
            crate::workbench::wb_open,
            crate::workbench::wb_list,
            crate::workbench::wb_files,
            crate::workbench::wb_read,
            crate::workbench::wb_write,
            crate::workbench::wb_create,
            crate::workbench::wb_rename,
            crate::workbench::wb_delete,
            crate::workbench::wb_changes,
            crate::workbench::wb_base,
            crate::workbench::wb_find,
            crate::workbench::wb_watch,
            crate::workbench::wb_unwatch,
            crate::workbench::term_open,
            crate::workbench::home_term_open,
            crate::workbench::term_list,
            crate::workbench::term_write,
            crate::workbench::term_resize,
            crate::workbench::term_close,
            crate::folders::folders_list,
            crate::folders::folder_add,
            crate::folders::folder_remove,
            crate::folders::folder_for_path,
            crate::folders::reveal_path,
            crate::updates::update_status,
            crate::updates::update_check,
            crate::updates::update_install,
            crate::updates::update_when_idle,
            crate::updates::update_stop_waiting,
            crate::updates::update_parts_apply,
            crate::updates::update_parts_auto,
            crate::updates::update_auto,
            crate::upgrade::upgrade_start,
            crate::upgrade::upgrade_status,
            crate::upgrade::upgrade_cancel,
            crate::upgrade::upgrade_retry
        ])
        .setup(|app| {
            let root = app.path().app_data_dir()?;
            ensure_private_dir(&root)?;
            ensure_private_dir(&root.join("runner"))?;
            let logs = crate::local::logs_dir();
            let runtime = crate::local::runtime_dir(app.path().resource_dir().ok());
            let components = crate::components::start(app.handle(), &root, &runtime);
            let stack = Stack::new(root.clone(), logs.clone(), runtime, crate::local::sidecar_dir())
                .map_err(std::io::Error::other)?;
            components.choose(&stack);
            let _ = WEB_ORIGIN.set(stack.web_origin());
            let config_path = root.join("desktop.json");
            let log_path = logs.join("runner.log");
            let config = load_config(&config_path).unwrap_or_else(|_| NativeConfig {
                name: default_device_name(),
                ..NativeConfig::default()
            });
            let runner = runner_service(app.handle().clone(), &root, log_path.clone(), &stack.pid_dir());
            app.manage(NativeState {
                root,
                config_path,
                log_path,
                config: Mutex::new(config),
                runner,
                slack: Mutex::new(SlackWatcher::default()),
                slack_status: Mutex::new(SlackView::default()),
                slack_settled: tokio::sync::watch::channel(true).0,
            });
            app.manage(Arc::new(stack));
            app.manage(components);
            app.manage(Arc::new(crate::workbench::Workbench::new()));
            app.manage(crate::updates::Updates::default());
            app.manage(crate::upgrade::Upgrade::default());
            keep_awake();
            build_window(app)?;
            build_menu(app)?;
            crate::updates::spawn(app.handle());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                boot(&handle).await;
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                exit_on_signal(handle).await;
            });
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-world" => show_main(app),
            "check-updates" => {
                show_main(app);
                crate::updates::check_now(app);
            }
            "reload-world" => {
                let failed = matches!(app.state::<Arc<Stack>>().phase(), Phase::Failed { .. });
                if failed {
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        boot(&handle).await;
                    });
                } else if let Some(window) = app.get_webview_window("main") {
                    let _ = window.eval("window.location.reload()");
                }
            }
            "open-settings" => {
                show_main(app);
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.eval("window.dispatchEvent(new CustomEvent('world:desktop-command',{detail:'settings'}))");
                }
            }
            "open-docs" => {
                let _ = open::that(DOCS_URL);
            }
            "command-menu" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.eval("window.dispatchEvent(new CustomEvent('world:desktop-command',{detail:'palette'}))");
                }
            }
            "pause-runner" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = set_paused(&app, true).await;
                });
            }
            "resume-runner" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = set_paused(&app, false).await;
                });
            }
            "choose-repository" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = choose_repository_internal(&app).await;
                });
            }
            "view-logs" => {
                let _ = open::that(crate::local::logs_dir());
            }
            "quit-world" => {
                shutdown(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build Loka")
        .run(|app, event| match event {
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } => show_main(app),
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => shutdown(app),
            _ => {}
        });
}

async fn exit_on_signal(app: AppHandle) {
    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) =
        (signal(SignalKind::terminate()), signal(SignalKind::interrupt()), signal(SignalKind::hangup()))
    else {
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        _ = hup.recv() => {}
    }
    shutdown(&app);
    app.exit(0);
}

pub(crate) fn shutdown(app: &AppHandle) {
    stop_worker(app);
    stop_slack(app);
    crate::workbench::close_all(app);
    if let Some(stack) = app.try_state::<Arc<Stack>>() {
        stack.stop_all();
    }
}

async fn boot(app: &AppHandle) {
    let stack = app.state::<Arc<Stack>>().inner().clone();
    if !stack.begin() {
        return;
    }
    show_loading(app, "Starting Loka");
    let result = boot_steps(app, &stack).await;
    match result {
        Ok(()) => {
            stack.ready();
            if let (Some(window), Ok(target)) = (app.get_webview_window("main"), Url::parse(&stack.login_url("/world"))) {
                let _ = window.navigate(target);
            }
        }
        Err((message, service)) => {
            stop_worker(app);
            stack.stop_all();
            if let Phase::Failed { message, log } = stack.fail(service, message) {
                show_failure(app, &message, &log);
            }
        }
    }
    stack.end();
}

async fn boot_steps(app: &AppHandle, stack: &Stack) -> Result<(), (String, &'static str)> {
    stack.start_database().await?;
    show_loading(app, "Starting the Loka server");
    stack.start_api().await?;
    show_loading(app, "Connecting this Mac");
    ensure_runner(app).await.map_err(|error| (error, "api"))?;
    let _ = start_slack(app);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        wait_for_slack(&handle.state::<NativeState>(), SLACK_IDENTITY_WAIT).await;
        let _ = start_worker(&handle);
    });
    show_loading(app, "Opening Loka");
    stack.start_web().await
}

fn show_loading(app: &AppHandle, message: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let detail = serde_json::json!({ "state": "starting", "message": message });
        let _ = window.eval(format!("window.lokaShow && window.lokaShow({detail})"));
    }
}

fn show_failure(app: &AppHandle, message: &str, log: &Path) {
    let Some(window) = app.get_webview_window("main") else { return };
    let mut target = match Url::parse("tauri://localhost/index.html") {
        Ok(url) => url,
        Err(_) => return,
    };
    target
        .query_pairs_mut()
        .append_pair("error", message)
        .append_pair("log", &log.to_string_lossy());
    let _ = window.navigate(target);
    show_main(app);
}

fn build_window(app: &mut tauri::App) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Loka")
        .inner_size(1280.0, 820.0)
        .min_inner_size(820.0, 600.0)
        .center()
        .disable_drag_drop_handler()
        .initialization_script(
            "document.addEventListener('keydown', function (event) { if (event.key === 'Escape') event.preventDefault(); });",
        )
        .on_navigation(|url| match frame_navigation(url) {
            FrameNavigation::Allow => true,
            FrameNavigation::Drop => false,
            FrameNavigation::External => {
                let _ = open::that(url.as_str());
                false
            }
        })
        .on_new_window(|url, _| {
            if matches!(url.scheme(), "http" | "https") {
                let _ = open::that(url.as_str());
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(|webview, event| {
            if let tauri::webview::DownloadEvent::Requested { destination, .. } = event {
                let name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let detail = serde_json::json!({ "name": name }).to_string();
                let _ = webview.eval(format!("window.dispatchEvent(new CustomEvent('world:downloaded', {{ detail: {detail} }}))"));
            }
            true
        })
        .build()?;
    window.show()?;
    Ok(())
}

fn build_menu(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open-world", "Open Loka", true, None::<&str>)?;
    let command = MenuItem::with_id(
        app,
        "command-menu",
        "Command Menu",
        true,
        Some("CmdOrCtrl+K"),
    )?;
    let settings = MenuItem::with_id(app, "open-settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let docs = MenuItem::with_id(app, "open-docs", "Loka Docs", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause-runner", "Pause Runner", true, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume-runner", "Resume Runner", true, None::<&str>)?;
    let repository = MenuItem::with_id(
        app,
        "choose-repository",
        "Choose Repository…",
        true,
        None::<&str>,
    )?;
    let logs = MenuItem::with_id(app, "view-logs", "View Logs", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit-world", "Quit Loka", true, Some("CmdOrCtrl+Q"))?;
    let separator = || PredefinedMenuItem::separator(app);
    let updates = MenuItem::with_id(app, "check-updates", "Check for Updates…", true, None::<&str>)?;
    let world = Submenu::with_items(
        app,
        "Loka",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About Loka"), None)?,
            &updates,
            &separator()?,
            &settings,
            &separator()?,
            &open,
            &command,
            &separator()?,
            &pause,
            &resume,
            &repository,
            &logs,
            &separator()?,
            &PredefinedMenuItem::hide(app, Some("Hide Loka"))?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &separator()?,
            &quit,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &separator()?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let reload = MenuItem::with_id(app, "reload-world", "Reload", true, Some("CmdOrCtrl+R"))?;
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[&reload, &separator()?, &PredefinedMenuItem::fullscreen(app, None)?],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, Some("Zoom"))?,
            &separator()?,
            &PredefinedMenuItem::close_window(app, None)?,
            &PredefinedMenuItem::bring_all_to_front(app, None)?,
        ],
    )?;
    let help = Submenu::with_items(app, "Help", true, &[&docs])?;
    app.set_menu(Menu::with_items(app, &[&world, &edit, &view, &window, &help])?)?;
    Ok(())
}

#[tauri::command]
async fn desktop_status(window: WebviewWindow) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    status(&window).await
}

#[tauri::command]
async fn pair_device(window: WebviewWindow, name: String) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Device name must be between 1 and 80 characters.".into());
    }
    let app = window.app_handle();
    let state = app.state::<NativeState>();
    let runner_id = state.config.lock().map_err(lock_error)?.runner_id.clone();
    match runner_id {
        Some(runner_id) => {
            let response = owner_client(app)?
                .patch(format!("{}/api/world/runners/{runner_id}", api_origin(app)))
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await
                .map_err(display_error)?;
            if !response.status().is_success() {
                return Err(format!("Loka could not rename this Mac ({})", response.status()));
            }
            let mut config = state.config.lock().map_err(lock_error)?;
            config.name = name;
            save_config(&state.config_path, &config).map_err(display_error)?;
        }
        None => {
            {
                let mut config = state.config.lock().map_err(lock_error)?;
                config.name = name;
                save_config(&state.config_path, &config).map_err(display_error)?;
            }
            ensure_runner(app).await?;
            start_worker(app)?;
        }
    }
    status(&window).await
}

#[tauri::command]
async fn pause_runner(window: WebviewWindow) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    set_paused(window.app_handle(), true).await?;
    status(&window).await
}

#[tauri::command]
async fn resume_runner(window: WebviewWindow) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    set_paused(window.app_handle(), false).await?;
    status(&window).await
}

#[tauri::command]
async fn disconnect_device(window: WebviewWindow) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    Err("Loka runs your agents on this Mac, so it stays connected. Pause the runner instead.".into())
}

#[tauri::command]
async fn choose_repository(window: WebviewWindow) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    choose_repository_internal(window.app_handle()).await?;
    status(&window).await
}

#[tauri::command]
async fn open_runner_logs(window: WebviewWindow) -> Result<(), String> {
    require_trusted(&window)?;
    let state = window.state::<NativeState>();
    append_log(&state.log_path).map_err(display_error)?;
    open::that(&state.log_path).map_err(display_error)
}

#[tauri::command]
async fn set_start_at_login(window: WebviewWindow, enabled: bool) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    if enabled {
        window.app_handle().autolaunch().enable().map_err(display_error)?;
    } else {
        window.app_handle().autolaunch().disable().map_err(display_error)?;
    }
    status(&window).await
}

#[tauri::command]
async fn set_slack_capture(window: WebviewWindow, enabled: bool) -> Result<DesktopStatus, String> {
    require_trusted(&window)?;
    let app = window.app_handle();
    let state = app.state::<NativeState>();
    if enabled && state.config.lock().map_err(lock_error)?.runner_id.is_none() {
        return Err("Wait for this Mac to connect before capturing Slack notifications.".into());
    }
    stop_slack(app);
    let key_path = state.root.join(SLACK_KEY_FILE);
    if enabled {
        write_slack_key(&key_path).map_err(display_error)?;
    } else {
        let _ = std::fs::remove_file(&key_path);
    }
    {
        let mut config = state.config.lock().map_err(lock_error)?;
        config.slack_enabled = enabled;
        save_config(&state.config_path, &config).map_err(display_error)?;
    }
    *state.slack_status.lock().map_err(lock_error)? = SlackView::default();
    if enabled {
        start_slack(app)?;
    }
    publish_slack_status(app);
    status(&window).await
}

fn api_origin(app: &AppHandle) -> String {
    app.state::<Arc<Stack>>().api_origin()
}

fn owner_client(app: &AppHandle) -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut session = reqwest::header::HeaderValue::from_str(app.state::<Arc<Stack>>().app_session()).map_err(display_error)?;
    session.set_sensitive(true);
    headers.insert(APP_SESSION_HEADER, session);
    local_builder().default_headers(headers).build().map_err(display_error)
}

pub(crate) async fn move_out(app: &AppHandle) -> Result<Vec<u8>, String> {
    let response = owner_client(app)?
        .post(format!("{}/api/world/me/move-out", api_origin(app)))
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|_| "Loka could not pack your Loka.".to_string())?;
    let status = response.status();
    eprintln!("Upgrade to Pro: POST /api/world/me/move-out {}", status.as_u16());
    if !status.is_success() {
        return Err(format!("Loka could not pack your Loka ({})", status.as_u16()));
    }
    response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|_| "Loka could not pack your Loka.".to_string())
}

fn local_client() -> Result<reqwest::Client, String> {
    local_builder().build().map_err(display_error)
}

fn local_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
}

async fn ensure_runner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<NativeState>();
    let stack = app.state::<Arc<Stack>>();
    let client = owner_client(app)?;
    let origin = stack.api_origin();
    let response = client
        .get(format!("{origin}/api/world/runners"))
        .send()
        .await
        .map_err(display_error)?;
    if !response.status().is_success() {
        return Err(format!("Loka could not list this Mac's runner ({})", response.status()));
    }
    let runners: Vec<RunnerView> = response.json().await.map_err(display_error)?;
    let config = state.config.lock().map_err(lock_error)?.clone();
    let known = config
        .runner_id
        .as_ref()
        .filter(|_| stack.runner_token().is_some())
        .and_then(|id| runners.iter().find(|runner| &runner.id == id && runner.status != "revoked"));
    let keep = match known {
        Some(runner) => {
            let mut config = state.config.lock().map_err(lock_error)?;
            config.paused = runner.status == "paused";
            if !runner.name.is_empty() {
                config.name = runner.name.clone();
            }
            save_config(&state.config_path, &config).map_err(display_error)?;
            runner.id.clone()
        }
        None => {
            let name = if config.name.trim().is_empty() { default_device_name() } else { config.name.clone() };
            let response = client
                .post(format!("{origin}/api/world/runners"))
                .json(&serde_json::json!({ "name": name, "platform": "macos" }))
                .send()
                .await
                .map_err(display_error)?;
            if !response.status().is_success() {
                return Err(format!("Loka could not connect this Mac ({})", response.status()));
            }
            let registration: RunnerRegistration = response.json().await.map_err(display_error)?;
            stack.set_runner_token(Some(registration.token))?;
            let mut config = state.config.lock().map_err(lock_error)?;
            config.runner_id = Some(registration.runner.id.clone());
            config.name = registration.runner.name;
            config.paused = registration.runner.status == "paused";
            save_config(&state.config_path, &config).map_err(display_error)?;
            registration.runner.id
        }
    };
    for runner in runners.iter().filter(|runner| runner.id != keep && runner.status != "revoked") {
        let _ = client
            .delete(format!("{origin}/api/world/runners/{}", runner.id))
            .send()
            .await;
    }
    Ok(())
}

async fn status(window: &WebviewWindow) -> Result<DesktopStatus, String> {
    let app = window.app_handle();
    let state = app.state::<NativeState>();
    let config = state.config.lock().map_err(lock_error)?.clone();
    let worker_running = state.runner.running();
    let last_error = state.runner.last_error();
    let state_name = if config.runner_id.is_none() {
        "connecting"
    } else if config.paused {
        "paused"
    } else if worker_running {
        "idle"
    } else if last_error.is_some() {
        "error"
    } else {
        "offline"
    };
    let slack = slack_view(app, config.slack_enabled)?;
    Ok(DesktopStatus {
        available: true,
        runner_id: config.runner_id,
        name: config.name,
        status: state_name.into(),
        server: api_origin(app),
        workspace_root: state.root.join("runner").to_string_lossy().to_string(),
        repositories: config.repositories,
        backends: backend_statuses().await,
        last_error,
        start_at_login: app.autolaunch().is_enabled().unwrap_or(false),
        slack,
        features: FEATURES.to_vec(),
    })
}

async fn set_paused(app: &AppHandle, paused: bool) -> Result<(), String> {
    let state = app.state::<NativeState>();
    let runner_id = state
        .config
        .lock()
        .map_err(lock_error)?
        .runner_id
        .clone()
        .ok_or_else(|| "This Mac is still connecting.".to_string())?;
    let response = owner_client(app)?
        .patch(format!("{}/api/world/runners/{runner_id}", api_origin(app)))
        .json(&serde_json::json!({ "paused": paused }))
        .send()
        .await
        .map_err(display_error)?;
    if !response.status().is_success() {
        return Err(format!("Loka could not update this runner ({})", response.status()));
    }
    {
        let mut config = state.config.lock().map_err(lock_error)?;
        config.paused = paused;
        save_config(&state.config_path, &config).map_err(display_error)?;
    }
    if paused {
        stop_worker(app);
    } else {
        start_worker(app)?;
    }
    Ok(())
}

async fn choose_repository_internal(app: &AppHandle) -> Result<(), String> {
    let Some(path) = rfd::AsyncFileDialog::new().pick_folder().await else {
        return Ok(());
    };
    let path = std::fs::canonicalize(path.path()).map_err(display_error)?;
    let output = Command::new("git")
        .current_dir(&path)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(display_error)?;
    if !output.status.success() {
        return Err("Choose a Git repository with a GitHub origin.".into());
    }
    let origin = String::from_utf8_lossy(&output.stdout);
    let id = github_repo(&origin)
        .ok_or_else(|| "Choose a repository with a GitHub origin.".to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&id)
        .to_string();
    let state = app.state::<NativeState>();
    {
        let mut config = state.config.lock().map_err(lock_error)?;
        config.repositories.retain(|repo| repo.id != id);
        config.repositories.push(RepositoryView {
            id,
            name,
            path: path.to_string_lossy().to_string(),
        });
        save_config(&state.config_path, &config).map_err(display_error)?;
    }
    if state.config.lock().map_err(lock_error)?.runner_id.is_some() {
        stop_worker(app);
        start_worker(app)?;
    }
    Ok(())
}

fn runner_service(app: AppHandle, root: &Path, log: PathBuf, pid_dir: &Path) -> Arc<Supervised> {
    let runner_root = root.join("runner");
    let key_file = root.join(SLACK_KEY_FILE);
    Supervised::new("runner", log, pid_dir, libc::SIGTERM, true, Duration::from_secs(5), move || {
        let state = app.state::<NativeState>();
        let stack = app.state::<Arc<Stack>>();
        let config = state.config.lock().map_err(lock_error)?.clone();
        let runner_id = config.runner_id.ok_or_else(|| "This Mac is not connected yet.".to_string())?;
        let token = stack.runner_token().ok_or_else(|| "This Mac has no runner credential.".to_string())?;
        let executable = crate::local::sidecar("loka-agent").ok_or_else(|| "The bundled runner is missing.".to_string())?;
        let git_config = runner_root.join("gitconfig");
        write_git_config(&git_config).map_err(display_error)?;
        let repositories = config
            .repositories
            .iter()
            .map(|repo| (repo.id.clone(), repo.path.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut command = Command::new(executable);
        command
            .arg("runner")
            .env("WORLD_RUNNER_URL", stack.api_origin())
            .env("WORLD_RUNNER_ID", &runner_id)
            .env("WORLD_RUNNER_TOKEN", token)
            .env("WORLD_RUNNER_ROOT", &runner_root)
            .env("WORLD_RUNNER_DRAIN_SECS", "3")
            .env(
                "WORLD_RUNNER_REPOSITORIES",
                serde_json::to_string(&repositories).map_err(display_error)?,
            )
            .env("GIT_CONFIG_GLOBAL", git_config)
            .env("WORLD_SLACK_KEY_FILE", &key_file)
            .env_remove("DATABASE_URL")
            .env_remove("REDIS_URL")
            .env_remove("AUTH_SECRET")
            .env_remove("NEXTAUTH_SECRET")
            .env_remove("WORLD_SECRETS_KEY")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN");
        for (name, value) in crate::shell_env::logins() {
            command.env(name, value);
        }
        if let Some(script) = slack_script(&app, "slack_mcp.py") {
            command.env("WORLD_SLACK_MCP_SCRIPT", script);
        }
        Ok(command)
    })
}

fn start_worker(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<NativeState>();
    let config = state.config.lock().map_err(lock_error)?.clone();
    if config.runner_id.is_none() || config.paused {
        return Ok(());
    }
    state.runner.start()
}

fn slack_view(app: &AppHandle, enabled: bool) -> Result<SlackView, String> {
    let state = app.state::<NativeState>();
    let running = {
        let mut slack = state.slack.lock().map_err(lock_error)?;
        match slack.child.as_mut() {
            Some(child) => child.try_wait().map_err(display_error)?.is_none(),
            None => false,
        }
    };
    let mut view = state.slack_status.lock().map_err(lock_error)?.clone();
    view.enabled = enabled;
    view.running = running;
    Ok(view)
}

fn slack_script(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("scripts").join(name));
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("scripts")
        .join(name);
    bundled
        .into_iter()
        .chain(std::iter::once(development))
        .find(|path| path.is_file())
}

fn set_slack_error(app: &AppHandle, error: Option<String>) {
    if let Ok(mut view) = app.state::<NativeState>().slack_status.lock() {
        view.last_error = error;
    }
    publish_slack_status(app);
}

fn publish_slack_status(app: &AppHandle) {
    let state = app.state::<NativeState>();
    let enabled = state.config.lock().map(|config| config.slack_enabled).unwrap_or(false);
    let Ok(view) = slack_view(app, enabled) else {
        return;
    };
    let status = serde_json::json!({
        "enabled": view.enabled,
        "running": view.running,
        "workspaces": view.workspaces,
        "error": view.last_error,
        "identity": state.slack.lock().ok().and_then(|slack| slack.identity.clone()),
    });
    let _ = save_json(&state.root.join("runner").join(SLACK_STATUS_FILE), &status);
}

fn start_slack(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<NativeState>();
    let config = state.config.lock().map_err(lock_error)?.clone();
    let Some(runner_id) = config.runner_id else {
        return Ok(());
    };
    if !config.slack_enabled {
        publish_slack_status(app);
        return Ok(());
    }
    {
        let mut slack = state.slack.lock().map_err(lock_error)?;
        if let Some(child) = slack.child.as_mut() {
            if child.try_wait().map_err(display_error)?.is_none() {
                return Ok(());
            }
        }
        slack.child = None;
    }
    let key_path = state.root.join(SLACK_KEY_FILE);
    if !key_path.is_file() {
        write_slack_key(&key_path).map_err(display_error)?;
    }
    let key = std::fs::read_to_string(&key_path).map_err(display_error)?;
    let script = slack_script(app, "slack_watch.py").ok_or_else(|| "The Slack watcher is missing from this app.".to_string())?;
    let runner_secret = app
        .state::<Arc<Stack>>()
        .runner_token()
        .ok_or_else(|| "This Mac has no runner credential.".to_string())?;
    let stderr = append_log(&state.log_path).map_err(display_error)?;
    let mut command = Command::new(SLACK_PYTHON);
    command
        .arg("-E")
        .arg("-s")
        .arg(&script)
        .env_clear()
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("WORLD_SLACK_CONNECTION_KEY", key.trim())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        let message = format!("Could not start the Slack watcher: {error}");
        set_slack_error(app, Some(message.clone()));
        message
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "The Slack watcher has no output.".to_string())?;
    let stdin = child.stdin.take();
    let (generation, previous) = adopt_slack(&state, child)?;
    if let Some(mut previous) = previous {
        terminate_process_tree(&mut previous);
    }
    if let Some(stdin) = stdin {
        let handle = app.clone();
        let (id, secret) = (runner_id.clone(), runner_secret.clone());
        std::thread::spawn(move || feed_slack_watches(handle, stdin, id, secret, generation));
    }
    let handle = app.clone();
    std::thread::spawn(move || relay_slack(handle, stdout, runner_id, runner_secret, generation));
    publish_slack_status(app);
    Ok(())
}

fn feed_slack_watches(app: AppHandle, mut stdin: std::process::ChildStdin, runner_id: String, runner_secret: String, generation: u64) {
    use std::io::Write;
    let client = local_client().unwrap_or_default();
    let endpoint = format!("{}/api/runner/{runner_id}/slack/watches", api_origin(&app));
    while slack_owns(&app, generation) {
        let fetched = tauri::async_runtime::block_on(async {
            let response = client
                .get(&endpoint)
                .header(reqwest::header::AUTHORIZATION, format!("Runner {runner_secret}"))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            response.json::<serde_json::Value>().await.ok()
        });
        if let Some(mut watches) = fetched {
            watches["type"] = serde_json::json!("watches");
            if writeln!(stdin, "{watches}").and_then(|_| stdin.flush()).is_err() {
                return;
            }
        }
        std::thread::sleep(SLACK_WATCH_REFRESH);
    }
}

fn slack_owns(app: &AppHandle, generation: u64) -> bool {
    app.state::<NativeState>()
        .slack
        .lock()
        .map(|slack| slack.owns(generation))
        .unwrap_or(false)
}

async fn wait_for_slack(state: &NativeState, limit: Duration) {
    let mut settled = state.slack_settled.subscribe();
    let _ = tokio::time::timeout(limit, settled.wait_for(|settled| *settled)).await;
}

fn adopt_slack(state: &NativeState, child: Child) -> Result<(u64, Option<Child>), String> {
    let mut slack = state.slack.lock().map_err(lock_error)?;
    state.slack_settled.send_replace(false);
    Ok(slack.replace(Some(child)))
}

fn release_slack(state: &NativeState) -> Option<Child> {
    let mut slack = state.slack.lock().ok()?;
    state.slack_settled.send_replace(true);
    slack.replace(None).1
}

fn record_slack_status(state: &NativeState, generation: u64, message: &serde_json::Value) -> bool {
    let Ok(mut slack) = state.slack.lock() else {
        return false;
    };
    if !slack.owns(generation) {
        return false;
    }
    let workspaces = message["workspaces"].as_array().cloned().unwrap_or_default();
    slack.identity = match workspaces.as_slice() {
        [workspace] => workspace["team_id"]
            .as_str()
            .zip(workspace["user_id"].as_str())
            .map(|(team_id, user_id)| serde_json::json!({ "team_id": team_id, "user_id": user_id })),
        _ => None,
    };
    let teams = workspaces
        .iter()
        .filter_map(|workspace| workspace["team"].as_str().map(String::from))
        .collect::<Vec<_>>();
    let error = message["error"].as_str().map(slack_error_text);
    if let Ok(mut view) = state.slack_status.lock() {
        if !teams.is_empty() || error.is_none() {
            view.workspaces = teams;
        }
        view.last_error = error;
    }
    true
}

fn record_slack_delivery(state: &NativeState, generation: u64, delivered: Result<(), String>) -> bool {
    let Ok(slack) = state.slack.lock() else {
        return false;
    };
    if !slack.owns(generation) {
        return false;
    }
    if let Ok(mut view) = state.slack_status.lock() {
        match delivered {
            Ok(()) => view.delivered += 1,
            Err(error) => view.last_error = Some(error),
        }
    }
    true
}

fn settle_slack(state: &NativeState, generation: u64) {
    if let Ok(slack) = state.slack.lock() {
        if slack.owns(generation) {
            state.slack_settled.send_replace(true);
        }
    }
}

fn relay_slack(app: AppHandle, stdout: std::process::ChildStdout, runner_id: String, runner_secret: String, generation: u64) {
    let client = local_client().unwrap_or_default();
    let endpoint = format!("{}/api/runner/{runner_id}/slack", api_origin(&app));
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match message.get("type").and_then(serde_json::Value::as_str) {
            Some("status") => {
                if !record_slack_status(&app.state::<NativeState>(), generation, &message) {
                    break;
                }
                publish_slack_status(&app);
                settle_slack(&app.state::<NativeState>(), generation);
            }
            Some("notification") => {
                let delivered = tauri::async_runtime::block_on(deliver_slack(
                    &client,
                    &endpoint,
                    &runner_secret,
                    &message,
                    || slack_owns(&app, generation),
                ));
                let failed = delivered.is_err();
                if !record_slack_delivery(&app.state::<NativeState>(), generation, delivered) {
                    break;
                }
                if failed {
                    publish_slack_status(&app);
                }
            }
            _ => {}
        }
    }
    let state = app.state::<NativeState>();
    {
        let Ok(slack) = state.slack.lock() else {
            return;
        };
        if !slack.owns(generation) {
            return;
        }
        if let Ok(mut view) = state.slack_status.lock() {
            view.last_error = Some(stopped_text(view.last_error.as_deref()));
        }
    }
    publish_slack_status(&app);
    settle_slack(&state, generation);
    std::thread::sleep(Duration::from_secs(30));
    let enabled = state.config.lock().map(|config| config.slack_enabled).unwrap_or(false);
    if enabled && slack_owns(&app, generation) {
        let _ = start_slack(&app);
    }
}

async fn deliver_slack(
    client: &reqwest::Client,
    endpoint: &str,
    runner_secret: &str,
    notification: &serde_json::Value,
    current: impl Fn() -> bool,
) -> Result<(), String> {
    let body = serde_json::json!({ "notifications": [notification] });
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
        }
        if !current() {
            return Err("The Slack watcher was replaced.".into());
        }
        let request = client
            .post(endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Runner {runner_secret}"))
            .timeout(Duration::from_secs(20))
            .json(&body);
        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) if response.status().is_client_error() => {
                return Err(format!("Loka rejected a Slack notification ({})", response.status()))
            }
            Ok(response) => last = format!("Loka could not save a Slack notification ({})", response.status()),
            Err(error) => last = format!("Could not reach Loka: {error}"),
        }
    }
    Err(last)
}

const RETRY_NOTE: &str = "Trying again in 30 seconds.";

fn stopped_text(reason: Option<&str>) -> String {
    match reason.map(|r| r.trim_end_matches(RETRY_NOTE).trim()).filter(|r| !r.is_empty()) {
        Some(reason) => format!("{reason} {RETRY_NOTE}"),
        None => format!("The Slack watcher stopped. {RETRY_NOTE}"),
    }
}

fn slack_error_text(code: &str) -> String {
    match code {
        "slack_app_missing" => "Install the Slack desktop app on this Mac and sign in. Loka reads your Slack notifications from it.".into(),
        "slack_not_signed_in" | "slack_token_not_found" | "slack_cookie_not_found" => {
            "Open the Slack desktop app on this Mac and sign in. Capture picks it up within a minute.".into()
        }
        "invalid_auth" | "token_revoked" | "not_authed" | "account_inactive" => {
            "Slack signed this Mac out. Sign in again in the Slack desktop app and capture picks it up within a minute.".into()
        }
        "slack_storage_blocked" => {
            "macOS is blocking Loka from reading the Slack app's data. Turn on Loka in System Settings, Privacy and Security, Full Disk Access, then quit and reopen Loka.".into()
        }
        "slack_storage_unavailable" => "Loka could not read the Slack app's data just now. It tries again every minute; quitting and reopening Slack usually helps.".into(),
        "keychain_lookup_failed" | "keychain_lookup_timed_out" => {
            "When macOS asks about Slack Safe Storage, choose Always Allow. If you missed it, turn capture off and on to ask again.".into()
        }
        code if code.starts_with("cookie_") => "Loka could not unlock the Slack sign-in. Quit and reopen Slack, then turn capture off and on.".into(),
        "slack_connection_not_authorized" => {
            "The Slack connection key is invalid. Turn capture off and on again.".into()
        }
        "slack_network_failed" => "Slack is unreachable right now. Capture tries again by itself.".into(),
        "slack_http_failed" | "slack_response_invalid" | "rate_limited" => "Slack is having trouble right now. Capture tries again by itself.".into(),
        other => format!("Slack capture stopped: {other}"),
    }
}

pub(crate) fn runner_busy(app: &AppHandle) -> u32 {
    let Some(state) = app.try_state::<NativeState>() else {
        return 0;
    };
    if !state.runner.running() {
        return 0;
    }
    std::fs::read_to_string(state.root.join("runner").join("busy"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

pub(crate) fn stop_starting_at_login(app: &AppHandle) -> bool {
    let autolaunch = app.autolaunch();
    autolaunch.is_enabled().unwrap_or(false) && autolaunch.disable().is_ok()
}

pub(crate) fn pause_worker_for_update(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeState>() {
        state.runner.hand_off(libc::SIGUSR1, Duration::from_secs(25));
    }
}

fn stop_slack(app: &AppHandle) {
    let state = app.state::<NativeState>();
    if let Some(mut child) = release_slack(&state) {
        terminate_process_tree(&mut child);
    }
}

fn stop_worker(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeState>() {
        state.runner.stop();
    }
}

fn terminate_process_tree(child: &mut Child) {
    crate::local::terminate(child, libc::SIGTERM, true, Duration::from_secs(1));
}

#[derive(Debug, PartialEq, Eq)]
enum FrameNavigation {
    Allow,
    Drop,
    External,
}

const EMBED_HOSTS: &[&str] = &["youtube.com", "www.youtube.com", "youtube-nocookie.com", "www.youtube-nocookie.com"];
const EMBED_NOISE: &[&str] = &["doubleclick.net", "googlesyndication.com", "googleadservices.com", "google.com", "gstatic.com", "ytimg.com"];

fn frame_navigation(url: &Url) -> FrameNavigation {
    if trusted_world_url(url) || matches!(url.scheme(), "tauri" | "about") {
        return FrameNavigation::Allow;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return FrameNavigation::Drop;
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if EMBED_HOSTS.contains(&host.as_str()) && url.path().starts_with("/embed/") {
        return FrameNavigation::Allow;
    }
    if EMBED_NOISE.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
        return FrameNavigation::Drop;
    }
    FrameNavigation::External
}

fn trusted_url(url: &Url, origin: &str) -> bool {
    Url::parse(origin).is_ok_and(|origin| {
        url.scheme() == origin.scheme()
            && url.host_str() == origin.host_str()
            && url.port_or_known_default() == origin.port_or_known_default()
    })
}

fn trusted_world_url(url: &Url) -> bool {
    WEB_ORIGIN.get().is_some_and(|origin| trusted_url(url, origin))
}

pub(crate) fn require_trusted(window: &WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(display_error)?;
    if trusted_world_url(&url) {
        Ok(())
    } else {
        Err("Desktop commands are only available to Loka.".into())
    }
}

pub(crate) fn workspaces_root(window: &WebviewWindow) -> Result<PathBuf, String> {
    Ok(window.state::<NativeState>().root.join("runner").join("workspaces"))
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn load_config(path: &Path) -> Result<NativeConfig, std::io::Error> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

fn save_config(path: &Path, config: &NativeConfig) -> Result<(), std::io::Error> {
    save_json(path, config)
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    write_private(&temporary, &bytes)?;
    std::fs::rename(temporary, path)
}

fn write_slack_key(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let key = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    write_private(path, key.as_bytes())
}

fn git_config_text(include: Option<&Path>) -> String {
    let include = include.map(|path| format!("[include]\n\tpath = {}\n", path.display())).unwrap_or_default();
    let github = "\thelper =\n\thelper = !gh auth git-credential\n";
    format!("{include}[credential \"https://github.com\"]\n{github}[credential \"https://gist.github.com\"]\n{github}")
}

fn write_git_config(path: &Path) -> Result<(), std::io::Error> {
    let include = std::env::var_os("HOME").map(PathBuf::from).map(|home| home.join(".gitconfig")).filter(|path| path.is_file());
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(git_config_text(include.as_deref()).as_bytes())
}

fn opencode_enabled() -> bool {
    std::env::var("WORLD_ENABLE_OPENCODE")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

async fn backend_statuses() -> Vec<BackendView> {
    let mut views = Vec::new();
    for (name, command, args) in [
        ("codex", "codex", Some(&["login", "status"][..])),
        ("claude", "claude", Some(&["auth", "status"][..])),
        ("cursor", "cursor-agent", Some(&["status"][..])),
        ("opencode", "opencode", None),
        ("omp", "omp", None),
        ("browser", "agent-browser", None),
    ] {
        if name == "opencode" && !opencode_enabled() {
            continue;
        }
        let installed = command_exists(command);
        let authenticated = if let (true, Some(args)) = (installed, args) {
            let mut process = Command::new(command);
            for name in [
                "WORLD_SECRETS_KEY",
                "WORLD_RUNNER_TOKEN",
                "WORLD_RUNNER_MCP_TOKEN",
                "DATABASE_URL",
                "REDIS_URL",
                "AUTH_SECRET",
                "NEXTAUTH_SECRET",
                "GH_TOKEN",
                "GITHUB_TOKEN",
            ] {
                process.env_remove(name);
            }
            process
                .args(args)
                .output()
                .ok()
                .map(|output| output.status.success())
        } else {
            None
        };
        views.push(BackendView {
            name: name.into(),
            installed,
            authenticated,
        });
    }
    views
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
}

fn node_manager_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        ".claude/local",
        ".npm-global/bin",
        ".volta/bin",
        ".bun/bin",
        "Library/pnpm",
        ".asdf/shims",
        ".local/share/mise/shims",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .collect();
    let version = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().trim_start_matches('v').split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let newest_nvm = std::fs::read_dir(home.join(".nvm/versions/node"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.join("bin/node").exists())
        .max_by_key(|dir| version(dir));
    dirs.extend(newest_nvm.map(|dir| dir.join("bin")));
    dirs
}

fn desktop_path(existing: Option<&std::ffi::OsStr>, home: Option<&Path>) -> std::ffi::OsString {
    let mut paths = existing
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut additions = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ];
    if let Some(home) = home {
        additions.push(home.join(".local/bin"));
        additions.push(home.join(".cargo/bin"));
        additions.extend(node_manager_dirs(home).into_iter().filter(|dir| dir.is_dir()));
    }
    for path in additions {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    std::env::join_paths(paths).expect("desktop PATH entries are valid")
}

fn github_repo(origin: &str) -> Option<String> {
    let origin = origin.trim().trim_end_matches(".git");
    let repo = origin
        .strip_prefix("git@github.com:")
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"))
        .or_else(|| origin.strip_prefix("https://github.com/"))?;
    let parts = repo.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return None;
    }
    Some(repo.to_string())
}

fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "My Mac".into())
}

pub(crate) fn auto_update(app: &AppHandle) -> bool {
    app.state::<NativeState>().config.lock().map(|config| config.auto_update).unwrap_or(false)
}

pub(crate) fn set_auto_update(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let state = app.state::<NativeState>();
    let mut config = state.config.lock().map_err(lock_error)?;
    config.auto_update = enabled;
    save_config(&state.config_path, &config).map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> String {
    "Desktop state is unavailable.".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn test_state() -> NativeState {
        NativeState {
            root: PathBuf::new(),
            config_path: PathBuf::new(),
            log_path: PathBuf::new(),
            config: Mutex::new(NativeConfig::default()),
            runner: Supervised::new("runner", PathBuf::new(), Path::new(""), libc::SIGTERM, true, Duration::from_secs(1), || {
                Err("unused".to_string())
            }),
            slack: Mutex::new(SlackWatcher::default()),
            slack_status: Mutex::new(SlackView::default()),
            slack_settled: tokio::sync::watch::channel(true).0,
        }
    }

    #[test]
    fn only_the_current_slack_watcher_publishes_identity_before_the_runner_starts() {
        let state = test_state();
        let watcher = || Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let status = serde_json::json!({
            "type": "status",
            "error": null,
            "workspaces": [{"team_id": "T1", "user_id": "U1", "team": "Acme"}],
        });
        let identity = |state: &NativeState| state.slack.lock().unwrap().identity.clone();

        let (old, _) = adopt_slack(&state, watcher()).unwrap();
        assert!(!*state.slack_settled.borrow());
        assert!(record_slack_status(&state, old, &status));
        assert!(!*state.slack_settled.borrow());
        assert_eq!(identity(&state), Some(serde_json::json!({"team_id": "T1", "user_id": "U1"})));
        settle_slack(&state, old);
        assert!(*state.slack_settled.borrow());

        let (new, mut replaced) = adopt_slack(&state, watcher()).unwrap();
        assert!(!*state.slack_settled.borrow());
        assert_eq!(identity(&state), None);
        assert!(!record_slack_status(&state, old, &status));
        assert!(!record_slack_delivery(&state, old, Ok(())));
        settle_slack(&state, old);
        assert!(!*state.slack_settled.borrow());
        assert_eq!(identity(&state), None);
        assert_eq!(state.slack_status.lock().unwrap().delivered, 0);

        let mut stopped = release_slack(&state).unwrap();
        assert!(*state.slack_settled.borrow());
        assert!(!record_slack_status(&state, new, &status));
        assert_eq!(identity(&state), None);
        for child in [replaced.as_mut().unwrap(), &mut stopped] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn the_runner_never_waits_longer_than_the_limit_for_a_watcher_that_stays_silent() {
        let state = test_state();
        let started = Instant::now();
        tauri::async_runtime::block_on(wait_for_slack(&state, Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));

        state.slack_settled.send_replace(false);
        let started = Instant::now();
        tauri::async_runtime::block_on(wait_for_slack(&state, Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!*state.slack_settled.borrow());
    }

    #[test]
    fn a_stopped_watcher_keeps_its_reason() {
        let keychain = slack_error_text("keychain_lookup_timed_out");
        assert_eq!(stopped_text(Some(&keychain)), format!("{keychain} {RETRY_NOTE}"));
        assert_eq!(stopped_text(Some(&stopped_text(Some(&keychain)))), format!("{keychain} {RETRY_NOTE}"));
        assert_eq!(stopped_text(None), format!("The Slack watcher stopped. {RETRY_NOTE}"));
        assert!(slack_error_text("slack_storage_blocked").contains("Full Disk Access"));
        assert!(!slack_error_text("slack_not_signed_in").contains("Full Disk Access"));
        for code in ["slack_app_missing", "invalid_auth", "cookie_decryption_failed", "rate_limited", "anything"] {
            assert!(!slack_error_text(code).contains('\u{2014}'));
        }
    }

    #[test]
    fn every_command_is_allowed_for_the_world_window() {
        let source = include_str!("native.rs");
        let start = source.find("generate_handler![").unwrap();
        let end = start + source[start..].find("])").unwrap();
        let manifest = include_str!("../build.rs");
        let capability: serde_json::Value = serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        let allowed: Vec<&str> = capability["permissions"].as_array().unwrap().iter().filter_map(|value| value.as_str()).collect();
        let commands: Vec<&str> = source[start + "generate_handler![".len()..end]
            .split(',')
            .map(|entry| entry.trim().rsplit("::").next().unwrap_or_default())
            .filter(|name| !name.is_empty())
            .collect();
        assert!(commands.contains(&"desktop_status"));
        assert!(!commands.contains(&"set_remote_workshop"));
        assert!(!commands.contains(&"sign_in_browser"));
        for command in commands {
            assert!(manifest.contains(&format!("\"{command}\"")), "{command} missing from build.rs");
            let permission = format!("allow-{}", command.replace('_', "-"));
            assert!(allowed.contains(&permission.as_str()), "{permission} missing from capabilities/main.json");
        }
    }

    #[cfg(unix)]
    #[test]
    fn github_credentials_skip_the_keychain_and_store_helpers() {
        use std::os::unix::fs::PermissionsExt;
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!("world-git-credentials-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let generic_log = dir.join("generic.log");
        let gh_log = dir.join("gh.log");
        let user_config = dir.join("user.gitconfig");
        let generic = dir.join("git-credential-generic");
        std::fs::write(&generic, format!("#!/bin/sh\necho \"$@\" >> '{}'\n", generic_log.display())).unwrap();
        std::fs::set_permissions(&generic, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&user_config, format!("[credential]\n\thelper = {}\n", generic.display())).unwrap();
        let config = dir.join("gitconfig");
        std::fs::write(&config, git_config_text(Some(&user_config))).unwrap();
        let gh = dir.join("gh");
        std::fs::write(&gh, format!("#!/bin/sh\necho \"$@\" >> '{}'\n", gh_log.display())).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        let approve = |host: &str| {
            let mut child = Command::new("git")
                .args(["credential", "approve"])
                .env("GIT_CONFIG_GLOBAL", &config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("PATH", &path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("protocol=https\nhost={host}\nusername=x\npassword=y\n\n").as_bytes())
                .unwrap();
            child.wait().unwrap();
        };
        approve("github.com");
        assert!(gh_log.is_file());
        assert!(!generic_log.exists());
        approve("gist.github.com");
        assert!(!generic_log.exists());
        approve("example.com");
        assert!(generic_log.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capabilities_allow_only_the_local_origin() {
        let capability: serde_json::Value = serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        let urls: Vec<&str> = capability["remote"]["urls"].as_array().unwrap().iter().filter_map(|value| value.as_str()).collect();
        assert_eq!(urls, vec!["http://127.0.0.1:*/*"]);
    }

    #[test]
    fn old_configs_still_load() {
        let old: NativeConfig = serde_json::from_str(r#"{"runner_id":"r1","name":"Mac","paused":false,"repositories":[],"workshop_off":true}"#).unwrap();
        assert_eq!(old.runner_id.as_deref(), Some("r1"));
    }

    #[test]
    fn only_the_local_web_origin_is_privileged() {
        let origin = "http://127.0.0.1:43123";
        assert!(trusted_url(&Url::parse("http://127.0.0.1:43123/world").unwrap(), origin));
        assert!(!trusted_url(&Url::parse("http://127.0.0.1:43124/world").unwrap(), origin));
        assert!(!trusted_url(&Url::parse("http://localhost:43123/world").unwrap(), origin));
        assert!(!trusted_url(&Url::parse("https://127.0.0.1:43123/world").unwrap(), origin));
        assert!(!trusted_url(&Url::parse("https://loka.ygpt.dev/world").unwrap(), origin));
        assert!(!trusted_url(&Url::parse("tauri://localhost/index.html").unwrap(), origin));
    }

    #[test]
    fn music_embeds_load_inside_the_window() {
        let nav = |u: &str| frame_navigation(&Url::parse(u).unwrap());
        assert_eq!(nav("https://www.youtube.com/embed/abc?enablejsapi=1"), FrameNavigation::Allow);
        assert_eq!(nav("about:blank"), FrameNavigation::Allow);
        assert_eq!(nav("tauri://localhost/index.html"), FrameNavigation::Allow);
        assert_eq!(nav("https://googleads.g.doubleclick.net/pagead/id"), FrameNavigation::Drop);
        assert_eq!(nav("https://www.youtube.com/watch?v=abc"), FrameNavigation::External);
        assert_eq!(nav("https://www.youtube.com.evil.test/embed/abc"), FrameNavigation::External);
        assert_eq!(nav("https://loka.ygpt.dev/world"), FrameNavigation::External);
    }

    #[test]
    fn repository_parser_accepts_only_github_owner_and_name() {
        assert_eq!(
            github_repo("git@github.com:Warlord-K/world.git").as_deref(),
            Some("Warlord-K/world")
        );
        assert_eq!(
            github_repo("https://github.com/Warlord-K/world.git").as_deref(),
            Some("Warlord-K/world")
        );
        assert!(github_repo("https://example.com/Warlord-K/world.git").is_none());
        assert!(github_repo("https://github.com/Warlord-K/world/extra.git").is_none());
    }

    #[test]
    fn status_serialization_never_contains_a_runner_secret() {
        let status = DesktopStatus {
            available: true,
            runner_id: Some("runner-id".into()),
            name: "Mac".into(),
            status: "idle".into(),
            server: "http://127.0.0.1:47811".into(),
            workspace_root: "/private/root".into(),
            repositories: Vec::new(),
            backends: Vec::new(),
            last_error: None,
            start_at_login: false,
            slack: SlackView::default(),
            features: FEATURES.to_vec(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("token"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn desktop_path_preserves_existing_order_and_adds_cli_locations() {
        let existing = std::ffi::OsStr::new("/custom/bin:/usr/bin:/bin:/usr/sbin:/sbin");
        let path = desktop_path(Some(existing), Some(Path::new("/Users/yatharth")));
        let paths = std::env::split_paths(&path).collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/custom/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/usr/sbin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/Users/yatharth/.local/bin"),
                PathBuf::from("/Users/yatharth/.cargo/bin"),
            ]
        );
    }

    #[test]
    fn desktop_path_finds_node_managers_that_exist() {
        let home = std::env::temp_dir().join(format!("world-path-{}", Uuid::new_v4()));
        for dir in [".volta/bin", ".nvm/versions/node/v9.0.0/bin", ".nvm/versions/node/v20.11.1/bin"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        std::fs::write(home.join(".nvm/versions/node/v9.0.0/bin/node"), "").unwrap();
        std::fs::write(home.join(".nvm/versions/node/v20.11.1/bin/node"), "").unwrap();
        let path = desktop_path(Some(std::ffi::OsStr::new("/usr/bin")), Some(&home));
        let paths = std::env::split_paths(&path).collect::<Vec<_>>();
        assert!(paths.contains(&home.join(".volta/bin")));
        assert!(paths.contains(&home.join(".nvm/versions/node/v20.11.1/bin")));
        assert!(!paths.contains(&home.join(".nvm/versions/node/v9.0.0/bin")));
        assert!(!paths.contains(&home.join(".bun/bin")));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stopping_a_worker_kills_its_process_group() {
        use std::os::unix::process::CommandExt;

        let marker = std::env::temp_dir().join(format!("world-worker-child-{}", Uuid::new_v4()));
        let script = format!(
            "sh -c 'trap \"\" TERM; while :; do sleep 1; done' & echo $! > '{}' && wait",
            marker.display()
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]).process_group(0);
        let mut child = command.spawn().unwrap();
        for _ in 0..20 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let descendant = std::fs::read_to_string(&marker).unwrap();
        terminate_process_tree(&mut child);
        let status = Command::new("kill")
            .args(["-0", descendant.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success());
    }
}
