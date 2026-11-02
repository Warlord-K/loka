use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::components::Components;
use std::sync::Arc;

const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const IDLE_POLL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UpdateView {
    state: &'static str,
    current: String,
    version: Option<String>,
    notes: Option<String>,
    progress: Option<f64>,
    error: Option<String>,
    busy: u32,
    auto_install: bool,
    parts: Option<crate::components::PartsView>,
    auto_parts: bool,
    parts_enabled: bool,
}

#[derive(Default)]
pub(crate) struct Updates {
    view: Mutex<UpdateView>,
    ready: Mutex<Option<(Update, Vec<u8>)>>,
    busy: tokio::sync::Mutex<()>,
    waiting: std::sync::atomic::AtomicBool,
}

pub(crate) fn spawn(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            check(&app, true).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

pub(crate) fn check_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        check(&app, false).await;
    });
}

fn current(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

fn with_parts(app: &AppHandle, mut view: UpdateView) -> UpdateView {
    if let Some(components) = app.try_state::<Arc<Components>>() {
        view.parts = components.view();
        view.auto_parts = components.auto();
        view.parts_enabled = components.enabled();
    }
    view
}

pub(crate) fn announce(app: &AppHandle) {
    let current = app.state::<Updates>().view.lock().map(|v| v.clone()).unwrap_or_default();
    let current = if current.state.is_empty() { view(app, "idle") } else { current };
    set(app, current, true);
}

fn set(app: &AppHandle, view: UpdateView, announce: bool) {
    let mut view = with_parts(app, view);
    view.auto_install = crate::native::auto_update(app) || view.auto_parts;
    view.busy = crate::native::runner_busy(app);
    let updates = app.state::<Updates>();
    if let Ok(mut current) = updates.view.lock() {
        *current = view.clone();
    }
    if announce {
        if let (Some(window), Ok(detail)) = (app.get_webview_window("main"), serde_json::to_string(&view)) {
            let _ = window.eval(format!("window.dispatchEvent(new CustomEvent('world:update',{{detail:{detail}}}))"));
        }
    }
}

fn view(app: &AppHandle, state: &'static str) -> UpdateView {
    UpdateView { state, current: current(app), ..UpdateView::default() }
}

async fn check(app: &AppHandle, quiet: bool) {
    let updates = app.state::<Updates>();
    let Ok(_guard) = updates.busy.try_lock() else {
        return;
    };
    let ready_version = updates.ready.lock().ok().and_then(|ready| ready.as_ref().map(|(update, _)| update.version.clone()));
    if let Some(version) = ready_version {
        let announce = updates.view.lock().map(|v| v.clone()).unwrap_or_default();
        set(app, UpdateView { state: "ready", version: Some(version), ..announce }, true);
        return;
    }
    if !quiet {
        set(app, view(app, "checking"), true);
    }
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => {
            if let Some(components) = app.try_state::<Arc<Components>>() {
                components.offer(&crate::components::Plan::default(), String::new(), None);
            }
            set(app, view(app, "current"), !quiet);
            return;
        }
        Err(error) => {
            set(app, UpdateView { error: Some(error.to_string()), ..view(app, "failed") }, !quiet);
            return;
        }
    };
    let version = update.version.clone();
    let notes = update.body.clone();
    if let Some(components) = app.try_state::<Arc<Components>>() {
        let components = components.inner().clone();
        let plan = components.plan(&update.raw_json);
        let offered = components.offer(&plan, version.clone(), notes.clone());
        if offered && components.auto() {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::components::apply(&app).await;
            });
        }
        if !plan.full {
            set(app, view(app, "current"), !quiet || offered);
            return;
        }
    }
    let base = UpdateView { version: Some(version.clone()), notes: notes.clone(), ..view(app, "downloading") };
    set(app, base.clone(), !quiet);
    let total = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let progress_app = app.clone();
    let progress_base = base.clone();
    let seen = total.clone();
    let mut last = 0u64;
    let bytes = update
        .download(
            move |chunk, size| {
                let got = seen.fetch_add(chunk as u64, std::sync::atomic::Ordering::Relaxed) + chunk as u64;
                if let Some(size) = size.filter(|size| *size > 0) {
                    let percent = got * 100 / size;
                    if percent >= last + 10 {
                        last = percent;
                        set(&progress_app, UpdateView { progress: Some(percent as f64 / 100.0), ..progress_base.clone() }, !quiet);
                    }
                }
            },
            || {},
        )
        .await;
    match bytes {
        Ok(bytes) => {
            if let Ok(mut ready) = updates.ready.lock() {
                *ready = Some((update, bytes));
            }
            set(app, UpdateView { state: "ready", progress: None, ..base }, true);
            if crate::native::auto_update(app) {
                let _ = update_when_idle(app.clone());
            }
        }
        Err(error) => set(app, UpdateView { error: Some(error.to_string()), ..view(app, "failed") }, !quiet),
    }
}

#[tauri::command]
pub(crate) fn update_status(app: AppHandle) -> UpdateView {
    let view = app.state::<Updates>().view.lock().map(|v| v.clone()).unwrap_or_default();
    let view = if view.state.is_empty() { UpdateView { state: "idle", current: current(&app), ..UpdateView::default() } } else { view };
    let view = with_parts(&app, view);
    let auto_install = crate::native::auto_update(&app) || view.auto_parts;
    UpdateView { busy: crate::native::runner_busy(&app), auto_install, ..view }
}

#[tauri::command]
pub(crate) async fn update_parts_apply(app: AppHandle) -> Result<(), String> {
    crate::components::apply(&app).await
}

#[tauri::command]
pub(crate) async fn update_parts_auto(app: AppHandle, enabled: bool) -> Result<UpdateView, String> {
    update_auto(app, enabled)
}

#[tauri::command]
pub(crate) fn update_auto(app: AppHandle, enabled: bool) -> Result<UpdateView, String> {
    crate::native::set_auto_update(&app, enabled)?;
    if let Some(components) = app.try_state::<Arc<Components>>() {
        let components = components.inner().clone();
        components.set_auto(enabled)?;
        if enabled && components.has_pending() && !components.applying() {
            let applying = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::components::apply(&applying).await;
            });
        }
    }
    install_when_ready(&app, enabled)?;
    Ok(update_status(app))
}

#[tauri::command]
pub(crate) fn update_check(app: AppHandle) {
    check_now(&app);
}

#[tauri::command]
pub(crate) async fn update_install(app: AppHandle) -> Result<(), String> {
    install(&app).await
}

#[tauri::command]
pub(crate) fn update_when_idle(app: AppHandle) -> Result<(), String> {
    let updates = app.state::<Updates>();
    let ready = updates.ready.lock().map(|ready| ready.as_ref().map(|(update, _)| update.version.clone())).ok().flatten();
    let Some(version) = ready else {
        return Err("No update is ready yet.".to_string());
    };
    if updates.waiting.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let base = UpdateView { version: Some(version), ..view(&app, "waiting") };
    set(&app, base.clone(), true);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut idle_checks = 0;
        let mut last = u32::MAX;
        while app.state::<Updates>().waiting.load(Ordering::Acquire) {
            let busy = crate::native::runner_busy(&app);
            if busy != last {
                last = busy;
                set(&app, base.clone(), true);
            }
            idle_checks = if busy == 0 { idle_checks + 1 } else { 0 };
            if idle_checks >= 2 {
                app.state::<Updates>().waiting.store(false, Ordering::Release);
                let _ = install(&app).await;
                return;
            }
            tokio::time::sleep(IDLE_POLL).await;
        }
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn update_stop_waiting(app: AppHandle) {
    let updates = app.state::<Updates>();
    if updates.waiting.swap(false, Ordering::AcqRel) {
        let announce = updates.view.lock().map(|v| v.clone()).unwrap_or_default();
        set(&app, UpdateView { state: "ready", ..announce }, true);
    }
}

fn install_when_ready(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let ready = app.state::<Updates>().ready.lock().map(|ready| ready.is_some()).unwrap_or(false);
    if enabled && ready {
        update_when_idle(app.clone())?;
    }
    Ok(())
}

async fn install(app: &AppHandle) -> Result<(), String> {
    if app.try_state::<Arc<Components>>().is_some_and(|components| components.applying()) {
        return Err("Loka is updating a part right now. Try again in a minute.".to_string());
    }
    let updates = app.state::<Updates>();
    updates.waiting.store(false, Ordering::Release);
    let pending = updates.ready.lock().map_err(|_| "Loka could not read the update.".to_string())?.take();
    let Some((update, bytes)) = pending else {
        return Err("No update is ready yet.".to_string());
    };
    set(app, UpdateView { version: Some(update.version.clone()), ..view(app, "installing") }, true);
    let pausing = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || crate::native::pause_worker_for_update(&pausing)).await;
    crate::native::shutdown(app);
    if let Err(error) = update.install(bytes) {
        set(app, UpdateView { error: Some(error.to_string()), ..view(app, "failed") }, true);
        return Err(format!("The update could not be installed: {error}"));
    }
    app.restart();
}
