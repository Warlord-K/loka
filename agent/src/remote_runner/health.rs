use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use reqwest::redirect::Policy;
use reqwest::Url;
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct RunnerState {
    pub ready: Arc<AtomicBool>,
    pub running: Arc<Mutex<BTreeMap<Uuid, String>>>,
    pub slots: usize,
}

pub async fn start_from_env(state: RunnerState) -> Result<Option<JoinHandle<Result<()>>>> {
    let Some(port) = std::env::var("PORT").ok() else {
        return Ok(None);
    };
    let port = port.parse::<u16>().context("invalid PORT")?;
    if port == 0 {
        bail!("invalid PORT");
    }
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .with_state(state);
    info!(port, "Runner health server starting");
    Ok(Some(tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .context("health server failed")
    })))
}

async fn health(State(state): State<RunnerState>) -> StatusCode {
    if state.ready.load(Ordering::Acquire) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn metrics(State(state): State<RunnerState>) -> String {
    let busy = state.running.lock().unwrap().len();
    format!(
        "# TYPE world_runner_slots gauge\nworld_runner_slots {}\n# TYPE world_runner_busy_slots gauge\nworld_runner_busy_slots {busy}\n",
        state.slots
    )
}

pub fn spawn_self_ping() -> Result<()> {
    let target = std::env::var("SELF_URL")
        .ok()
        .or_else(|| std::env::var("RENDER_EXTERNAL_URL").ok());
    let Some(target) = target else {
        return Ok(());
    };
    let mut url = Url::parse(&target).context("invalid SELF_URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("SELF_URL must use HTTP or HTTPS");
    }
    url.set_path("/health");
    url.set_query(None);
    url.set_fragment(None);
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    info!(target = %url, "Runner self-ping started");
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(240)).await;
            match client.get(url.clone()).send().await {
                Ok(response) if response.status().is_success() => {
                    tracing::debug!(status = %response.status(), "Runner self-ping succeeded");
                }
                Ok(response) => {
                    warn!(status = %response.status(), "Runner self-ping returned an error");
                }
                Err(error) => warn!(%error, "Runner self-ping failed"),
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_tracks_restore_readiness() {
        let ready = Arc::new(AtomicBool::new(false));
        let state = RunnerState {
            ready: ready.clone(),
            running: Arc::new(Mutex::new(BTreeMap::new())),
            slots: 1,
        };
        assert_eq!(
            health(State(state.clone())).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        ready.store(true, Ordering::Release);
        assert_eq!(health(State(state)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_report_busy_slots() {
        let state = RunnerState {
            ready: Arc::new(AtomicBool::new(true)),
            running: Arc::new(Mutex::new(BTreeMap::from([(Uuid::nil(), String::new())]))),
            slots: 3,
        };
        let body = metrics(State(state)).await;
        assert!(body.contains("\nworld_runner_slots 3\n"));
        assert!(body.contains("\nworld_runner_busy_slots 1\n"));
    }
}
