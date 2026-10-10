use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::StreamExt;
use std::time::Duration;
use tracing::warn;

use super::{Hub, Transport, TOKEN_HEADER};

const MAX_BODY: usize = 4 * 1024 * 1024;
const FORWARDED_REQUEST: &[&str] = &["content-type", "accept", "mcp-session-id", "mcp-protocol-version", "last-event-id"];
const FORWARDED_RESPONSE: &[&str] = &["content-type", "mcp-session-id", "cache-control"];

pub(super) async fn serve(hub: &'static Hub, listener: tokio::net::TcpListener) {
    let app = Router::new()
        .route("/mcp/{name}", any(forward_root))
        .route("/mcp/{name}/~/{*rest}", any(forward_path))
        .with_state(hub);
    if let Err(error) = axum::serve(listener, app).await {
        warn!(%error, "MCP hub proxy stopped");
    }
}

async fn forward_root(
    State(hub): State<&'static Hub>,
    Path(name): Path<String>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(hub, name, None, query, method, headers, body).await
}

async fn forward_path(
    State(hub): State<&'static Hub>,
    Path((name, rest)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(hub, name, Some(rest), query, method, headers, body).await
}

pub(super) fn upstream_url(base: &str, rest: Option<&str>, query: Option<&str>) -> Option<String> {
    let mut url = url::Url::parse(base).ok()?;
    if let Some(rest) = rest {
        url.set_path(&format!("/{}", rest.trim_start_matches('/')));
        url.set_query(query);
    } else if let Some(query) = query {
        let merged = match url.query() {
            Some(existing) => format!("{existing}&{query}"),
            None => query.to_string(),
        };
        url.set_query(Some(&merged));
    }
    Some(url.to_string())
}

pub(super) fn rewrite_endpoint(data: &str, base: &str, name: &str) -> String {
    let Ok(base) = url::Url::parse(base) else {
        return data.to_string();
    };
    let path = if data.starts_with('/') {
        Some(data.to_string())
    } else {
        url::Url::parse(data).ok().filter(|url| url.origin() == base.origin()).map(|url| {
            let mut path = url.path().to_string();
            if let Some(query) = url.query() {
                path.push('?');
                path.push_str(query);
            }
            path
        })
    };
    match path {
        Some(path) => format!("/mcp/{}/~{path}", super::urlencode(name)),
        None => data.to_string(),
    }
}

async fn forward(
    hub: &'static Hub,
    name: String,
    rest: Option<String>,
    query: Option<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = headers.get(TOKEN_HEADER).and_then(|value| value.to_str().ok()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some((spec, guest)) = hub.http_target(token, &name).await else {
        return (StatusCode::FORBIDDEN, "This MCP server is only available while the agent is working on a run").into_response();
    };
    let Transport::Http { url, headers: secret_headers, sse } = &spec.transport else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if body.len() > MAX_BODY {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Some(target) = upstream_url(url, rest.as_deref(), query.as_deref()) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if guest && !loka_shared::feeds::public_url(&target) {
        return (StatusCode::FORBIDDEN, "MCP servers for shared devices must be on the public internet").into_response();
    }
    let client = if guest { guest_client() } else { client() };
    let mut request = client.request(method, &target).body(body);
    for name in FORWARDED_REQUEST {
        if let Some(value) = headers.get(*name) {
            request = request.header(*name, value.clone());
        }
    }
    for (key, value) in secret_headers {
        request = request.header(key, value);
    }
    let upstream = match request.send().await {
        Ok(upstream) => upstream,
        Err(error) => {
            warn!(server = %name, error = %super::redacted(&error.to_string(), &spec), "MCP proxy request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = HeaderMap::new();
    for name in FORWARDED_RESPONSE {
        if let Some(value) = upstream.headers().get(*name).and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok()) {
            out.insert(HeaderName::from_static(name), value);
        }
    }
    let event_stream = upstream
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let body = if *sse && event_stream {
        let base = url.clone();
        let server = name.clone();
        let mut pending = String::new();
        let mut endpoint = false;
        let stream = upstream.bytes_stream().map(move |chunk| {
            let chunk = chunk.map_err(std::io::Error::other)?;
            pending.push_str(&String::from_utf8_lossy(&chunk));
            let mut ready = String::new();
            while let Some(index) = pending.find('\n') {
                let line: String = pending.drain(..=index).collect();
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if let Some(event) = trimmed.strip_prefix("event:") {
                    endpoint = event.trim() == "endpoint";
                    ready.push_str(&line);
                } else if let Some(data) = trimmed.strip_prefix("data:").filter(|_| endpoint) {
                    ready.push_str(&format!("data: {}\n", rewrite_endpoint(data.trim(), &base, &server)));
                } else {
                    if trimmed.is_empty() {
                        endpoint = false;
                    }
                    ready.push_str(&line);
                }
            }
            Ok::<_, std::io::Error>(Bytes::from(ready))
        });
        Body::from_stream(stream)
    } else {
        Body::from_stream(upstream.bytes_stream())
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = out;
    response
}

fn guest_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| loka_shared::feeds::public_only_client(Duration::from_secs(20)).expect("guest proxy client"))
}

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("proxy client")
    })
}
