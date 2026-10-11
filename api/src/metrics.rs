use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use loka_shared::telemetry;

const SLOW_REQUEST: Duration = Duration::from_secs(3);

pub async fn track(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let method = request.method().as_str().to_string();
    let span = tracing::info_span!("request", route = %route, method = %method);
    let started = Instant::now();
    let response = tracing::Instrument::instrument(next.run(request), span).await;
    let elapsed = started.elapsed();
    let status = response.status();
    let long_lived = route.ends_with("/stream") || route.ends_with("/socket") || route.ends_with("/workshop");
    if status.is_server_error() {
        tracing::warn!(route = %route, method = %method, status = status.as_u16(), ms = elapsed.as_millis() as u64, "Request failed");
    } else if elapsed >= SLOW_REQUEST && !long_lived {
        tracing::info!(route = %route, method = %method, status = status.as_u16(), ms = elapsed.as_millis() as u64, "Slow request");
    }
    if telemetry::metrics_on() {
        let class = format!("{}xx", status.as_u16() / 100);
        telemetry::count("http_requests_total", &[("route", &route), ("method", &method), ("status", &class)], 1.0);
        telemetry::observe("http_request_duration_seconds", &[("route", &route), ("method", &method)], elapsed.as_secs_f64());
    }
    response
}

pub struct Open(&'static str);

impl Open {
    pub fn new(name: &'static str) -> Self {
        telemetry::add("open_streams", &[("stream", name)], 1.0);
        Open(name)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        telemetry::add("open_streams", &[("stream", self.0)], -1.0);
    }
}

pub fn sample_pool(pool: sqlx::PgPool) {
    if !telemetry::metrics_on() {
        return;
    }
    tokio::spawn(async move {
        let installed = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')")
            .fetch_one(&pool)
            .await
            .unwrap_or(true);
        if !installed {
            if let Err(error) = sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements").execute(&pool).await {
                tracing::info!(%error, "pg_stat_statements is not available, so the slow statement panel stays empty");
            }
        }
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let size = pool.size() as f64;
            let idle = pool.num_idle() as f64;
            telemetry::gauge("db_pool_connections", &[("state", "idle")], idle);
            telemetry::gauge("db_pool_connections", &[("state", "busy")], (size - idle).max(0.0));
        }
    });
}
