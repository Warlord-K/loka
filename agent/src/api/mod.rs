use axum::{routing::get, Router};
use tracing::info;

pub async fn serve(shutdown: tokio::sync::watch::Receiver<()>) {
    let app = Router::new().route("/health", get(health));

    let port = std::env::var("AGENT_API_BIND_PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();
    info!("Health server on :{port}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut rx = shutdown;
            let _ = rx.changed().await;
            info!("Health server shutting down");
        })
        .await
        .unwrap();
}

async fn health() -> &'static str {
    "ok"
}
