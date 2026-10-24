use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::ws::{Message as ClientMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use tracing::warn;

const MAX_BODY: usize = 64 * 1024 * 1024;
const MARK: &str = "loka-";
const HEX_LEN: usize = 40;
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

pub struct Spec {
    pub id: &'static str,
    vars: &'static [(&'static str, &'static str)],
    upstream: &'static str,
    base_env: Option<&'static str>,
}

pub const ANTHROPIC: &str = "anthropic";
pub const OPENAI: &str = "openai";

const SPECS: &[Spec] = &[
    Spec {
        id: ANTHROPIC,
        vars: &[("ANTHROPIC_API_KEY", "sk-ant-api03-"), ("ANTHROPIC_AUTH_TOKEN", ""), ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-")],
        upstream: "https://api.anthropic.com",
        base_env: Some("ANTHROPIC_BASE_URL"),
    },
    Spec {
        id: OPENAI,
        vars: &[("OPENAI_API_KEY", "sk-"), ("CODEX_API_KEY", "sk-")],
        upstream: "https://api.openai.com/v1",
        base_env: Some("OPENAI_BASE_URL"),
    },
    Spec { id: "openrouter", vars: &[("OPENROUTER_API_KEY", "sk-or-v1-")], upstream: "https://openrouter.ai/api/v1", base_env: None },
    Spec { id: "xai", vars: &[("XAI_API_KEY", "xai-")], upstream: "https://api.x.ai/v1", base_env: Some("XAI_BASE_URL") },
    Spec {
        id: "google",
        vars: &[("GEMINI_API_KEY", "AIza"), ("GOOGLE_GENERATIVE_AI_API_KEY", "AIza")],
        upstream: "https://generativelanguage.googleapis.com",
        base_env: None,
    },
    Spec { id: "fal", vars: &[("FAL_KEY", "")], upstream: "https://fal.run", base_env: None },
];

pub fn spec(id: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.id == id)
}

struct Secret {
    value: String,
    placeholder: String,
    origins: Vec<String>,
}

pub struct KeyProxy {
    addr: SocketAddr,
    key: hmac::Key,
    secrets: Mutex<HashMap<String, Secret>>,
    upstreams: Mutex<HashMap<String, String>>,
}

static PROXY: OnceLock<&'static KeyProxy> = OnceLock::new();

pub fn get() -> Option<&'static KeyProxy> {
    PROXY.get().copied()
}

pub fn enabled() -> bool {
    !matches!(
        std::env::var("WORLD_KEY_PROXY").map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Ok("off" | "0" | "false" | "no")
    )
}

pub async fn start() -> Result<&'static KeyProxy> {
    if let Some(proxy) = get() {
        return Ok(proxy);
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.context("could not open the model key proxy")?;
    let mut secret = [0u8; 32];
    SystemRandom::new().fill(&mut secret).map_err(|_| anyhow::anyhow!("no system randomness"))?;
    let proxy: &'static KeyProxy = Box::leak(Box::new(KeyProxy {
        addr: listener.local_addr()?,
        key: hmac::Key::new(hmac::HMAC_SHA256, &secret),
        secrets: Mutex::new(HashMap::new()),
        upstreams: Mutex::new(HashMap::new()),
    }));
    if PROXY.set(proxy).is_err() {
        return Ok(get().expect("proxy set"));
    }
    let app = Router::new()
        .route("/u/{upstream}", any(forward_root))
        .route("/u/{upstream}/{*rest}", any(forward_path))
        .with_state(proxy);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            warn!(%error, "model key proxy stopped");
        }
    });
    Ok(proxy)
}

pub fn origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.origin().ascii_serialization())
}

#[derive(Default, Debug)]
pub struct Shielded {
    pub bases: BTreeMap<&'static str, String>,
    pub placeholders: HashMap<String, String>,
    pub shell_openai_base: Option<String>,
}

impl Shielded {
    pub fn openai_base(&self) -> Option<&str> {
        self.bases.get(OPENAI).map(String::as_str)
    }

    pub fn codex_shell_overrides(&self) -> Vec<String> {
        self.shell_openai_base
            .iter()
            .map(|base| format!("shell_environment_policy.set.OPENAI_BASE_URL={}", serde_json::to_string(base).unwrap_or_default()))
            .collect()
    }
}

fn env_of(cmd: &tokio::process::Command) -> HashMap<String, String> {
    cmd.as_std()
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_str()?.to_string(), value?.to_str()?.to_string())))
        .collect()
}

impl KeyProxy {
    fn own_prefix(&self) -> String {
        format!("http://{}/", self.addr)
    }

    fn is_own(&self, url: &str) -> bool {
        url.starts_with(&self.own_prefix())
    }

    pub fn route(&self, upstream: &str) -> String {
        let upstream = upstream.trim().trim_end_matches('/').to_string();
        let tag = hmac::sign(&self.key, format!("upstream\0{upstream}").as_bytes());
        let id: String = tag.as_ref()[..8].iter().map(|byte| format!("{byte:02x}")).collect();
        self.upstreams.lock().unwrap().insert(id.clone(), upstream);
        format!("http://{}/u/{id}", self.addr)
    }

    pub fn place(&self, name: &str, prefix: &str, value: &str, origins: &[String]) -> String {
        if value.contains(MARK) || origins.is_empty() {
            return value.to_string();
        }
        let mut origins = origins.to_vec();
        origins.sort();
        origins.dedup();
        let tag = hmac::sign(&self.key, format!("{name}\0{value}\0{}", origins.join("\0")).as_bytes());
        let hex: String = tag.as_ref()[..HEX_LEN / 2].iter().map(|byte| format!("{byte:02x}")).collect();
        let placeholder = format!("{prefix}{MARK}{hex}");
        self.secrets
            .lock()
            .unwrap()
            .insert(hex, Secret { value: value.to_string(), placeholder: placeholder.clone(), origins });
        placeholder
    }

    fn upstream_for(&self, spec: &Spec, configured: Option<&str>) -> String {
        configured
            .map(str::trim)
            .filter(|url| !url.is_empty() && !self.is_own(url) && origin(url).is_some())
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_else(|| spec.upstream.to_string())
    }

    pub fn openai_base(&self) -> String {
        self.route(spec(OPENAI).expect("openai spec").upstream)
    }

    pub fn shield_value(&self, name: &str, value: &str) -> Option<String> {
        let spec = SPECS.iter().find(|spec| spec.vars.iter().any(|(var, _)| *var == name))?;
        let prefix = spec.vars.iter().find(|(var, _)| *var == name).map(|(_, prefix)| *prefix).unwrap_or("");
        Some(self.place(name, prefix, value, &[origin(spec.upstream)?]))
    }

    pub fn shield(&self, cmd: &mut tokio::process::Command, providers: &[&str]) -> Shielded {
        let env = env_of(cmd);
        let mut shielded = Shielded::default();
        for spec in providers.iter().filter_map(|id| spec(id)) {
            let present: Vec<(&str, &str, &String)> = spec
                .vars
                .iter()
                .filter_map(|(name, prefix)| env.get(*name).filter(|value| value.len() >= 4 && !value.contains(MARK)).map(|value| (*name, *prefix, value)))
                .collect();
            if present.is_empty() {
                continue;
            }
            let upstream = self.upstream_for(spec, spec.base_env.and_then(|name| env.get(name)).map(String::as_str));
            let Some(origin) = origin(&upstream) else { continue };
            for (name, prefix, value) in present {
                let placeholder = self.place(name, prefix, value, std::slice::from_ref(&origin));
                cmd.env(name, &placeholder);
                shielded.placeholders.insert(name.to_string(), placeholder);
            }
            let base = self.route(&upstream);
            if let Some(name) = spec.base_env {
                cmd.env(name, &base);
            }
            shielded.bases.insert(spec.id, base);
        }
        shielded
    }

    pub fn shield_codex(&self, cmd: &mut tokio::process::Command, chatgpt_login: bool) -> Shielded {
        let configured = env_of(cmd).remove("OPENAI_BASE_URL");
        let mut shielded = self.shield(cmd, &[OPENAI]);
        if let Some(route) = chatgpt_login.then(|| shielded.bases.remove(OPENAI)).flatten() {
            shielded.shell_openai_base = Some(route);
            match configured {
                Some(url) => cmd.env("OPENAI_BASE_URL", url),
                None => cmd.env_remove("OPENAI_BASE_URL"),
            };
        }
        shielded
    }

    pub fn shield_var(&self, cmd: &mut tokio::process::Command, name: &str, origins: &[String]) -> bool {
        let Some(value) = env_of(cmd).get(name).filter(|value| value.len() >= 4 && !value.contains(MARK)).cloned() else {
            return false;
        };
        let placeholder = self.place(name, "", &value, origins);
        cmd.env(name, placeholder);
        true
    }

    pub fn upstream_origin(&self, route: &str) -> Option<String> {
        let id = route.strip_prefix(&format!("http://{}/u/", self.addr))?.split('/').next()?;
        origin(&self.upstream(id)?)
    }

    fn upstream(&self, id: &str) -> Option<String> {
        self.upstreams.lock().unwrap().get(id).cloned()
    }

    fn substitute(&self, origin: &str, text: &str) -> Result<String, ()> {
        let mut out = text.to_string();
        let mut search = 0;
        while let Some(found) = out[search..].find(MARK) {
            let at = search + found;
            let start = at + MARK.len();
            let hex = out.get(start..start + HEX_LEN).filter(|hex| hex.chars().all(|c| c.is_ascii_hexdigit()));
            let Some(hex) = hex.map(str::to_string) else {
                search = start;
                continue;
            };
            let secrets = self.secrets.lock().unwrap();
            let Some(secret) = secrets.get(&hex) else {
                return Err(());
            };
            if !secret.origins.iter().any(|allowed| allowed == origin) {
                return Err(());
            }
            let prefix_len = secret.placeholder.len() - MARK.len() - HEX_LEN;
            let from = if at >= prefix_len && out[at - prefix_len..].starts_with(&secret.placeholder) { at - prefix_len } else { at };
            let to = start + HEX_LEN;
            out.replace_range(from..to, &secret.value);
            search = from + secret.value.len();
        }
        Ok(out)
    }
}

pub const PROMPT_NOTE: &str = "The model API key in your environment for the model you run on (for example ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY, GEMINI_API_KEY or FAL_KEY) is a placeholder that only works on this device: requests from your own model go through a local proxy that adds the real key. Use the variables as they are and keep any base URL variables you find. Never search for, print or ask for the real key: it is not readable here, and a placeholder that looks odd is expected, not a bug.";

pub fn prompt_note<'a>(backend: &str, mut names: impl Iterator<Item = &'a str>) -> Option<&'static str> {
    get()?;
    if !matches!(backend, "claude" | "codex" | "omp" | "opencode") {
        return None;
    }
    names.any(|name| SPECS.iter().any(|spec| spec.vars.iter().any(|(var, _)| *var == name))).then_some(PROMPT_NOTE)
}

pub fn shield(cmd: &mut tokio::process::Command, providers: &[&str]) -> Shielded {
    get().map(|proxy| proxy.shield(cmd, providers)).unwrap_or_default()
}

pub fn shield_codex(cmd: &mut tokio::process::Command, chatgpt_login: bool) -> Shielded {
    get().map(|proxy| proxy.shield_codex(cmd, chatgpt_login)).unwrap_or_default()
}

pub fn codex_overrides(base: &str) -> Vec<String> {
    vec![format!("openai_base_url={}", serde_json::to_string(base).unwrap_or_default())]
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("model key proxy client")
    })
}

async fn forward_root(State(proxy): State<&'static KeyProxy>, Path(upstream): Path<String>, request: Request) -> Response {
    forward(proxy, upstream, String::new(), request).await
}

async fn forward_path(State(proxy): State<&'static KeyProxy>, Path((upstream, rest)): Path<(String, String)>, request: Request) -> Response {
    forward(proxy, upstream, rest, request).await
}

fn target_url(upstream: &str, rest: &str, query: Option<&str>) -> String {
    let mut url = upstream.trim_end_matches('/').to_string();
    if !rest.is_empty() {
        url.push('/');
        url.push_str(rest.trim_start_matches('/'));
    }
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn refused() -> Response {
    (StatusCode::UNAUTHORIZED, "This key only works for Loka runs on this device, toward its own provider").into_response()
}

fn swapped_headers(proxy: &KeyProxy, origin: &str, headers: &HeaderMap, skip_websocket: bool) -> Result<HeaderMap, ()> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let lower = name.as_str();
        if HOP_BY_HOP.contains(&lower) || (skip_websocket && lower.starts_with("sec-websocket-")) {
            continue;
        }
        let value = match value.to_str() {
            Ok(text) if text.contains(MARK) => HeaderValue::from_str(&proxy.substitute(origin, text)?).map_err(|_| ())?,
            _ => value.clone(),
        };
        out.append(name.clone(), value);
    }
    Ok(out)
}

async fn forward(proxy: &'static KeyProxy, upstream_id: String, rest: String, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();
    let Some(base) = proxy.upstream(&upstream_id) else {
        return (StatusCode::NOT_FOUND, "Unknown model provider route").into_response();
    };
    let Some(origin) = origin(&base) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let query = match parts.uri.query() {
        Some(query) if query.contains(MARK) => match proxy.substitute(&origin, query) {
            Ok(query) => Some(query),
            Err(()) => return refused(),
        },
        other => other.map(String::from),
    };
    let target = target_url(&base, &rest, query.as_deref());
    let websocket = parts
        .headers
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let Ok(headers) = swapped_headers(proxy, &origin, &parts.headers, websocket) else {
        return refused();
    };
    if websocket {
        let protocols = parts.headers.get("sec-websocket-protocol").cloned();
        let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        return forward_socket(upgrade, &target, headers, protocols).await;
    }
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let upstream = match client().request(parts.method, &target).headers(headers).body(body).send().await {
        Ok(upstream) => upstream,
        Err(error) => {
            warn!(error = %error.without_url(), "model key proxy request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(name.as_str().as_bytes()), HeaderValue::from_bytes(value.as_bytes())) {
            out.append(name, value);
        }
    }
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = out;
    response
}

async fn forward_socket(upgrade: WebSocketUpgrade, target: &str, headers: HeaderMap, protocols: Option<HeaderValue>) -> Response {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = if let Some(rest) = target.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = target.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let Ok(mut request) = url.into_client_request() else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    for (name, value) in &headers {
        request.headers_mut().append(name.clone(), value.clone());
    }
    if let Some(protocols) = protocols {
        request.headers_mut().insert("sec-websocket-protocol", protocols);
    }
    let upstream = match tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async_with_config(request, None, false)).await {
        Ok(Ok((upstream, _))) => upstream,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = response.body().clone().unwrap_or_default();
            return (status, body).into_response();
        }
        Ok(Err(error)) => {
            warn!(error = %error, "model key proxy socket failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    upgrade.max_message_size(MAX_BODY).on_upgrade(move |client| pump(client, upstream))
}

async fn pump(client: WebSocket, upstream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let up = async {
        while let Some(Ok(message)) = client_rx.next().await {
            let message = match message {
                ClientMessage::Text(text) => UpstreamMessage::text(text.as_str()),
                ClientMessage::Binary(data) => UpstreamMessage::binary(data),
                ClientMessage::Ping(data) => UpstreamMessage::Ping(data),
                ClientMessage::Pong(data) => UpstreamMessage::Pong(data),
                ClientMessage::Close(_) => break,
            };
            if upstream_tx.send(message).await.is_err() {
                break;
            }
        }
        let _ = upstream_tx.close().await;
    };
    let down = async {
        while let Some(Ok(message)) = upstream_rx.next().await {
            let message = match message {
                UpstreamMessage::Text(text) => ClientMessage::Text(text.as_str().into()),
                UpstreamMessage::Binary(data) => ClientMessage::Binary(data),
                UpstreamMessage::Ping(data) => ClientMessage::Ping(data),
                UpstreamMessage::Pong(data) => ClientMessage::Pong(data),
                UpstreamMessage::Close(_) => break,
                UpstreamMessage::Frame(_) => continue,
            };
            if client_tx.send(message).await.is_err() {
                break;
            }
        }
        let _ = client_tx.close().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) type Hits = Arc<Mutex<Vec<(String, String)>>>;

    pub(crate) fn test_proxy() -> &'static KeyProxy {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        if let Some(proxy) = get() {
            return proxy;
        }
        std::thread::spawn(|| {
            let runtime = RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap());
            runtime.block_on(start()).unwrap()
        })
        .join()
        .unwrap()
    }

    pub(crate) async fn upstream() -> (String, Hits) {
        let seen: Hits = Arc::default();
        let log = seen.clone();
        let app = Router::new().route(
            "/{*rest}",
            any(move |Path(rest): Path<String>, axum::extract::RawQuery(query): axum::extract::RawQuery, headers: HeaderMap| {
                let log = log.clone();
                async move {
                    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let creds = format!(
                        "{}|{}|{}|{}",
                        header("x-api-key"),
                        header("authorization"),
                        header("x-goog-api-key"),
                        query.unwrap_or_default()
                    );
                    log.lock().unwrap().push((rest.clone(), creds));
                    if rest.starts_with("gateway") {
                        (StatusCode::OK, [("content-type", "text/event-stream")], "data: ok\n\n".to_string())
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            [("content-type", "application/json")],
                            r#"{"type":"error","error":{"type":"authentication_error","message":"mock"},"message":"mock"}"#.to_string(),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn env(cmd: &tokio::process::Command) -> HashMap<String, String> {
        env_of(cmd)
    }

    #[tokio::test]
    async fn cli_sees_a_placeholder_and_the_upstream_gets_the_key() {
        let proxy = test_proxy();
        let (upstream_url, seen) = upstream().await;
        let mut cmd = tokio::process::Command::new("true");
        cmd.env_clear();
        cmd.env("ANTHROPIC_API_KEY", "sk-ant-api03-real-secret");
        cmd.env("ANTHROPIC_BASE_URL", format!("{upstream_url}/gateway"));
        cmd.env("OPENAI_API_KEY", "sk-real-openai");
        cmd.env("FAL_KEY", "id:real-fal");
        cmd.env("PATH", "/usr/bin");
        let shielded = proxy.shield(&mut cmd, &[ANTHROPIC]);
        let env = env(&cmd);
        let placeholder = env["ANTHROPIC_API_KEY"].clone();
        assert!(placeholder.starts_with("sk-ant-api03-loka-"));
        assert!(!env.values().any(|value| value.contains("real-secret")));
        assert_eq!(env["OPENAI_API_KEY"], "sk-real-openai");
        assert_eq!(env["FAL_KEY"], "id:real-fal");
        assert!(env["ANTHROPIC_BASE_URL"].starts_with(&proxy.own_prefix()));
        assert_eq!(shielded.bases[ANTHROPIC], env["ANTHROPIC_BASE_URL"]);
        assert_eq!(env["PATH"], "/usr/bin");

        let mut again = tokio::process::Command::new("true");
        again.env("ANTHROPIC_API_KEY", "sk-ant-api03-real-secret");
        again.env("ANTHROPIC_BASE_URL", format!("{upstream_url}/gateway"));
        assert_eq!(proxy.shield(&mut again, &[ANTHROPIC]).placeholders["ANTHROPIC_API_KEY"], placeholder);

        let http = reqwest::Client::new();
        let base = env["ANTHROPIC_BASE_URL"].clone();
        let response = http
            .post(format!("{base}/v1/messages?beta=true"))
            .header("x-api-key", &placeholder)
            .header("authorization", format!("Bearer {placeholder}"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "data: ok\n\n");
        assert_eq!(
            seen.lock().unwrap().last().cloned(),
            Some(("gateway/v1/messages".into(), "sk-ant-api03-real-secret|Bearer sk-ant-api03-real-secret||beta=true".into()))
        );

        let own_login = http.post(format!("{base}/v1/messages")).header("authorization", "Bearer sk-ant-oat01-host-login").send().await.unwrap();
        assert_eq!(own_login.status(), 200);
        assert_eq!(seen.lock().unwrap().last().map(|hit| hit.1.clone()), Some("|Bearer sk-ant-oat01-host-login||".into()));

        let unknown_route = http
            .post(format!("{}u/0000000000000000/v1/messages", proxy.own_prefix()))
            .header("x-api-key", "anything")
            .send()
            .await
            .unwrap();
        assert_eq!(unknown_route.status(), 404);

        let count = seen.lock().unwrap().len();
        let elsewhere = proxy.route("https://attacker.example");
        let wrong_origin = http.post(format!("{elsewhere}/steal")).header("x-api-key", &placeholder).send().await.unwrap();
        assert_eq!(wrong_origin.status(), 401);
        let forged = http
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", format!("sk-ant-api03-loka-{}", "0".repeat(HEX_LEN)))
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), 401);
        assert_eq!(seen.lock().unwrap().len(), count);
    }

    #[tokio::test]
    async fn placeholders_are_filled_in_any_header_and_the_query() {
        let proxy = test_proxy();
        let (upstream_url, seen) = upstream().await;
        let origin = origin(&upstream_url).unwrap();
        let goog = proxy.place("GEMINI_API_KEY", "AIza", "AIzaRealGoogle", std::slice::from_ref(&origin));
        let fal = proxy.place("FAL_KEY", "", "id:real-fal", std::slice::from_ref(&origin));
        let base = proxy.route(&format!("{upstream_url}/gateway/fal-ai/model/v1"));
        let http = reqwest::Client::new();
        http.post(format!("{base}/chat/completions?key={goog}"))
            .header("x-goog-api-key", &goog)
            .header("authorization", format!("Key {fal}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().last().cloned(),
            Some(("gateway/fal-ai/model/v1/chat/completions".into(), "|Key id:real-fal|AIzaRealGoogle|key=AIzaRealGoogle".into()))
        );
    }

    pub(crate) async fn run_cli(mut cmd: tokio::process::Command) -> String {
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(90), cmd.output()).await {
            Ok(Ok(out)) => format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
            _ => "timed out".into(),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_clis_send_their_key_through_the_proxy() {
        let proxy = test_proxy();
        let (upstream_url, seen) = upstream().await;
        let home = std::env::temp_dir().join(format!("key-proxy-live-{}", std::process::id()));
        std::fs::create_dir_all(home.join("codex")).unwrap();
        std::fs::create_dir_all(home.join("claude")).unwrap();

        let mut claude = tokio::process::Command::new("claude");
        claude.args(["-p", "say hi", "--max-turns", "1", "--setting-sources", ""]);
        claude.current_dir(&home);
        claude.env("CLAUDE_CONFIG_DIR", home.join("claude"));
        claude.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        claude.env("ANTHROPIC_API_KEY", "sk-ant-api03-live-real");
        claude.env("ANTHROPIC_BASE_URL", &upstream_url);
        let placeholder = proxy.shield(&mut claude, &[ANTHROPIC]).placeholders["ANTHROPIC_API_KEY"].clone();
        let output = run_cli(claude).await;
        let hits = seen.lock().unwrap().clone();
        assert!(!output.contains("live-real"), "{output}");
        assert!(
            hits.iter().any(|(path, key)| path.ends_with("v1/messages") && key.starts_with("sk-ant-api03-live-real|")),
            "claude never reached the upstream with the real key: {hits:?} {output}"
        );
        assert!(!hits.iter().any(|(_, key)| key.contains(&placeholder)));

        seen.lock().unwrap().clear();
        let mut codex = tokio::process::Command::new("codex");
        codex.current_dir(&home);
        codex.env("CODEX_HOME", home.join("codex"));
        codex.env("CODEX_API_KEY", "sk-live-openai-real");
        codex.env("OPENAI_BASE_URL", format!("{upstream_url}/v1"));
        codex.env_remove("OPENAI_API_KEY");
        let shielded = proxy.shield(&mut codex, &[OPENAI]);
        codex.arg("exec");
        for value in codex_overrides(shielded.openai_base().unwrap()) {
            codex.arg("-c").arg(value);
        }
        codex.args(["--skip-git-repo-check", "say hi"]);
        let output = run_cli(codex).await;
        let hits = seen.lock().unwrap().clone();
        assert!(!output.contains("501"), "{output}");
        assert!(
            hits.iter().any(|(path, key)| path.ends_with("responses") && key == "|Bearer sk-live-openai-real||"),
            "codex never reached the upstream with the real key: {hits:?} {output}"
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn codex_keeps_its_own_backend_when_signed_in_with_chatgpt() {
        struct Case {
            name: &'static str,
            chatgpt_login: bool,
            vars: &'static [(&'static str, &'static str)],
            base: Base,
            shell: Option<&'static str>,
        }
        enum Base {
            Unset,
            Kept,
            Routed(&'static str),
        }
        const GATEWAY: &str = "https://gateway.example/v1";
        let proxy = test_proxy();
        let cases = [
            Case { name: "chatgpt login with a shell key", chatgpt_login: true, vars: &[("OPENAI_API_KEY", "sk-shell-key")], base: Base::Unset, shell: Some("https://api.openai.com") },
            Case { name: "chatgpt login keeps a configured base", chatgpt_login: true, vars: &[("OPENAI_API_KEY", "sk-shell-key"), ("OPENAI_BASE_URL", GATEWAY)], base: Base::Kept, shell: Some("https://gateway.example") },
            Case { name: "chatgpt login without keys", chatgpt_login: true, vars: &[], base: Base::Unset, shell: None },
            Case { name: "api key login", chatgpt_login: false, vars: &[("OPENAI_API_KEY", "sk-api-key")], base: Base::Routed("https://api.openai.com"), shell: None },
            Case { name: "codex api key", chatgpt_login: false, vars: &[("CODEX_API_KEY", "sk-codex-key")], base: Base::Routed("https://api.openai.com"), shell: None },
            Case { name: "api key with a configured base", chatgpt_login: false, vars: &[("CODEX_API_KEY", "sk-codex-key"), ("OPENAI_BASE_URL", GATEWAY)], base: Base::Routed("https://gateway.example"), shell: None },
        ];
        for case in cases {
            let name = case.name;
            let mut cmd = tokio::process::Command::new("true");
            cmd.env_clear();
            for (key, value) in case.vars {
                cmd.env(key, value);
            }
            let shielded = proxy.shield_codex(&mut cmd, case.chatgpt_login);
            let env = env(&cmd);
            for (key, value) in case.vars.iter().filter(|(key, _)| key.ends_with("_API_KEY")) {
                assert!(env[*key].contains(MARK) && env[*key] != *value, "{name}: {key} must be a placeholder");
            }
            let base = env.get("OPENAI_BASE_URL").map(String::as_str);
            match case.base {
                Base::Unset => assert!(base.is_none() && shielded.openai_base().is_none(), "{name}"),
                Base::Kept => assert!(base == Some(GATEWAY) && shielded.openai_base().is_none(), "{name}"),
                Base::Routed(origin) => {
                    assert_eq!(base, shielded.openai_base(), "{name}");
                    assert_eq!(base.and_then(|base| proxy.upstream_origin(base)).as_deref(), Some(origin), "{name}");
                }
            }
            let shell = shielded.shell_openai_base.as_deref();
            assert_eq!(shell.and_then(|shell| proxy.upstream_origin(shell)).as_deref(), case.shell, "{name}");
            let overrides = shielded.codex_shell_overrides();
            match shell {
                Some(shell) => assert_eq!(overrides, vec![format!("shell_environment_policy.set.OPENAI_BASE_URL=\"{shell}\"")], "{name}"),
                None => assert!(overrides.is_empty(), "{name}"),
            }
        }
    }

    #[test]
    fn target_urls_keep_the_upstream_prefix() {
        assert_eq!(target_url("https://api.openai.com/v1", "responses", None), "https://api.openai.com/v1/responses");
        assert_eq!(target_url("https://x.example/", "v1/messages", Some("beta=true")), "https://x.example/v1/messages?beta=true");
    }
}
