use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::{
    extract::{ConnectInfo, Query, Request},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct UserId(pub Uuid);

const DEFAULT_USER_ID: &str = "00000000-0000-0000-0000-000000000000";

const SECURE_COOKIE: &str = "__Secure-authjs.session-token";
const INSECURE_COOKIE: &str = "authjs.session-token";
pub const APP_SESSION_HEADER: &str = "x-loka-app-session";
pub const APP_SESSION_COOKIE: &str = "loka-app-session";
const APP_SESSION_MIN_LEN: usize = 32;
const APP_SESSION_MAX_AGE_SECS: u64 = 30 * 24 * 3600;

static APP_SESSION: OnceLock<String> = OnceLock::new();
static LOCAL_POLICY: OnceLock<LocalPolicy> = OnceLock::new();

pub fn auth_disabled() -> bool {
    std::env::var("AUTH_DISABLED").unwrap_or_default() == "true"
}

pub async fn auth_middleware(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    if auth_disabled() {
        if req.headers().contains_key("cf-connecting-ip") {
            return Err(StatusCode::UNAUTHORIZED);
        }
        if !LocalPolicy::current().page_ok(&req) {
            tracing::warn!(
                host = ?req.headers().get("host"),
                origin = ?req.headers().get("origin"),
                "AUTH_DISABLED=true refused a request that did not come from this app's own pages; list the exact origin in AUTH_DISABLED_ORIGINS if this was you"
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
        if !app_session_ok(&req, app_session_secret()) {
            tracing::debug!("AUTH_DISABLED=true refused a request without the app session; open Loka from the desktop app, or run loka-api login-link in development");
            return Err(StatusCode::UNAUTHORIZED);
        }
        let uid = disabled_auth_user(&req);
        req.extensions_mut().insert(UserId(uid));
        return Ok(next.run(req).await);
    }

    let uid = signed_in_user(&req)?;
    req.extensions_mut().insert(UserId(uid));
    Ok(next.run(req).await)
}

fn signed_in_user(req: &Request) -> Result<Uuid, StatusCode> {
    let (token, salt) = extract_token(req).ok_or(StatusCode::UNAUTHORIZED)?;
    if salt_is_cookie(req) && cross_site_write(req) {
        return Err(StatusCode::FORBIDDEN);
    }

    let auth_secret = session_secret().ok_or_else(|| {
        tracing::error!("AUTH_SECRET is missing or shorter than 16 characters, so no session can be trusted; set it to the same value the website uses");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let payload = decrypt_jwe(&token, &auth_secret, &salt).map_err(|e| {
        tracing::warn!("JWT decrypt failed: {e}");
        StatusCode::UNAUTHORIZED
    })?;

    let claims: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let user_id_str = claims
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let uid = Uuid::parse_str(user_id_str).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let expires = claims.get("exp").and_then(|v| v.as_i64()).ok_or(StatusCode::UNAUTHORIZED)?;
    if chrono::Utc::now().timestamp() > expires {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(uid)
}

pub fn app_session_secret() -> &'static str {
    APP_SESSION.get_or_init(load_app_session)
}

fn load_app_session() -> String {
    if let Some(given) = loka_shared::loka_env::var("APP_SESSION_SECRET").map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
        if given.len() >= APP_SESSION_MIN_LEN {
            return given;
        }
        tracing::warn!("LOKA_APP_SESSION_SECRET is shorter than 32 characters, so a fresh app session was made instead");
    }
    let path = loka_shared::loka_env::var("APP_SESSION_FILE").map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if let Some(saved) = path.as_deref().and_then(|path| std::fs::read_to_string(path).ok()).map(|value| value.trim().to_string()).filter(|value| value.len() >= APP_SESSION_MIN_LEN) {
        return saved;
    }
    let made = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    if let Some(path) = path {
        match write_private(std::path::Path::new(&path), &made) {
            Ok(()) => tracing::info!(path, "Wrote the app session; run loka-api login-link to open Loka in a browser"),
            Err(e) => tracing::error!(path, error = %e, "Could not write the app session file"),
        }
    }
    made
}

fn write_private(path: &std::path::Path, value: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(value.as_bytes())?;
    file.sync_all()
}

fn cookie_value<'a>(req: &'a Request, wanted: &str) -> Option<&'a str> {
    req.headers()
        .get_all("cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == wanted)
        .map(|(_, value)| value.trim())
}

fn app_session_ok(req: &Request, secret: &str) -> bool {
    if secret.len() < APP_SESSION_MIN_LEN {
        return false;
    }
    let matches = |given: &str| constant_time_eq(given.trim().as_bytes(), secret.as_bytes());
    if req.headers().get(APP_SESSION_HEADER).and_then(|v| v.to_str().ok()).is_some_and(matches) {
        return true;
    }
    let site = req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok());
    site.is_none_or(|site| matches!(site, "same-origin" | "none")) && cookie_value(req, APP_SESSION_COOKIE).is_some_and(matches)
}

const LOGIN_CODE_SECS: i64 = 120;
static USED_LOGIN_CODES: std::sync::Mutex<Vec<(i64, String)>> = std::sync::Mutex::new(Vec::new());

fn login_code_mac(secret: &str, expires: i64) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(format!("loka-login:{expires}").as_bytes());
    mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn login_code(secret: &str, now: i64) -> String {
    let expires = now + LOGIN_CODE_SECS;
    format!("{expires}.{}", login_code_mac(secret, expires))
}

fn login_code_ok(secret: &str, code: &str, now: i64) -> bool {
    let Some((expires, mac)) = code.trim().split_once('.') else {
        return false;
    };
    let Ok(expires) = expires.parse::<i64>() else {
        return false;
    };
    if secret.len() < APP_SESSION_MIN_LEN || expires < now || expires > now + LOGIN_CODE_SECS {
        return false;
    }
    if !constant_time_eq(mac.as_bytes(), login_code_mac(secret, expires).as_bytes()) {
        return false;
    }
    let Ok(mut used) = USED_LOGIN_CODES.lock() else {
        return false;
    };
    used.retain(|(until, _)| *until >= now);
    if used.iter().any(|(_, seen)| seen == mac) {
        return false;
    }
    used.push((expires, mac.to_string()));
    true
}

pub fn login_link(base: &str, secret: &str) -> String {
    format!("{}/api/app-session?code={}", base.trim_end_matches('/'), login_code(secret, chrono::Utc::now().timestamp()))
}

pub fn saved_app_session() -> Option<String> {
    if let Some(given) = loka_shared::loka_env::var("APP_SESSION_SECRET").map(|value| value.trim().to_string()).filter(|value| value.len() >= APP_SESSION_MIN_LEN) {
        return Some(given);
    }
    let path = loka_shared::loka_env::var("APP_SESSION_FILE")?;
    std::fs::read_to_string(path.trim()).ok().map(|value| value.trim().to_string()).filter(|value| value.len() >= APP_SESSION_MIN_LEN)
}

#[derive(serde::Deserialize)]
pub struct AppSessionHandoff {
    code: Option<String>,
    next: Option<String>,
}

pub async fn app_session_handoff(Query(handoff): Query<AppSessionHandoff>, req: Request) -> Response {
    if !auth_disabled() || req.headers().contains_key("cf-connecting-ip") || !LocalPolicy::current().page_ok(&req) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let secret = app_session_secret();
    if !handoff.code.as_deref().is_some_and(|code| login_code_ok(secret, code, chrono::Utc::now().timestamp())) {
        tracing::warn!("Refused an app session handoff with a wrong, used or expired code; open Loka from the desktop app or run loka-api login-link for a fresh link");
        return (StatusCode::UNAUTHORIZED, "This link is wrong, used or expired. Open Loka from the desktop app, or run loka-api login-link for a fresh one.").into_response();
    }
    let cookie = format!("{APP_SESSION_COOKIE}={secret}; HttpOnly; SameSite=Strict; Path=/; Max-Age={APP_SESSION_MAX_AGE_SECS}");
    let Ok(cookie) = HeaderValue::from_str(&cookie) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(location) = HeaderValue::from_str(&safe_next(handoff.next.as_deref())) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    (
        StatusCode::SEE_OTHER,
        [(header::SET_COOKIE, cookie), (header::LOCATION, location), (header::CACHE_CONTROL, HeaderValue::from_static("no-store")), (header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"))],
    )
        .into_response()
}

fn safe_next(next: Option<&str>) -> String {
    match next.map(str::trim) {
        Some(path) if path.starts_with('/') && !path.starts_with("//") && !path.contains('\\') && !path.chars().any(char::is_control) => path.to_string(),
        _ => "/world".to_string(),
    }
}

pub fn session_secret() -> Option<String> {
    std::env::var("AUTH_SECRET").ok().filter(|secret| secret.trim().len() >= 16)
}

pub fn default_bind_host() -> &'static str {
    let auth_disabled = auth_disabled();
    let contained = std::path::Path::new("/.dockerenv").exists() || std::env::var_os("KUBERNETES_SERVICE_HOST").is_some();
    if auth_disabled && !contained {
        "127.0.0.1"
    } else {
        "0.0.0.0"
    }
}

fn bare_host(raw: &str) -> &str {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match raw.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !host.contains(':') => host,
        _ => raw,
    }
}

fn url_host(raw: &str) -> Option<&str> {
    let rest = raw.trim().split_once("://")?.1;
    Some(bare_host(rest.split(['/', '?', '#']).next().unwrap_or(rest)))
}

const LOOPBACK_NAMES: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];
const DEFAULT_PORTS: [u16; 2] = [3000, 8081];
const APP_ORIGINS: [&str; 3] = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];
const URL_SETTINGS: [&str; 4] = ["WORLD_APP_URL", "WORLD_PUBLIC_API_URL", "AUTH_URL", "NEXTAUTH_URL"];

#[derive(Clone, Debug, Default)]
pub struct LocalPolicy {
    hosts: Vec<String>,
    origins: Vec<String>,
}

impl LocalPolicy {
    pub fn current() -> &'static LocalPolicy {
        LOCAL_POLICY.get_or_init(|| LocalPolicy::build(|name| std::env::var(name).ok()))
    }

    fn build(env: impl Fn(&str) -> Option<String>) -> Self {
        let list = |name: &str| -> Vec<String> {
            env(name).unwrap_or_default().split(',').map(|item| item.trim().to_string()).filter(|item| !item.is_empty()).collect()
        };
        let mut ports: Vec<u16> = DEFAULT_PORTS.to_vec();
        for name in ["FRONTEND_PORT", "PORT", "API_PORT"] {
            if let Some(port) = env(name).and_then(|value| value.trim().parse::<u16>().ok()) {
                ports.push(port);
            }
        }
        let named_hosts: Vec<String> = list("AUTH_DISABLED_HOSTS").iter().map(|host| bare_host(host).trim_end_matches('.').to_lowercase()).filter(|host| !host.is_empty()).collect();
        let mut hosts = named_hosts.clone();
        let mut origins: Vec<String> = APP_ORIGINS.iter().map(|origin| origin.to_string()).collect();
        for name in URL_SETTINGS {
            if let Some(value) = env(name) {
                if let Some(host) = url_host(&value) {
                    hosts.push(host.trim_end_matches('.').to_lowercase());
                }
                origins.extend(normalize_origin(&value));
            }
        }
        for name in LOOPBACK_NAMES {
            for port in &ports {
                origins.extend(normalize_origin(&format!("http://{name}:{port}")));
            }
        }
        for host in &named_hosts {
            let host = if host.contains(':') { format!("[{host}]") } else { host.clone() };
            origins.extend(normalize_origin(&format!("http://{host}")));
            origins.extend(normalize_origin(&format!("https://{host}")));
            for port in &ports {
                origins.extend(normalize_origin(&format!("http://{host}:{port}")));
            }
        }
        origins.extend(list("AUTH_DISABLED_ORIGINS").iter().filter_map(|origin| normalize_origin(origin)));
        hosts.sort();
        hosts.dedup();
        origins.sort();
        origins.dedup();
        LocalPolicy { hosts, origins }
    }

    fn host_ok(&self, raw: &str) -> bool {
        let host = bare_host(raw).trim_end_matches('.').to_lowercase();
        if host.is_empty() {
            return false;
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return ip.is_loopback() || self.hosts.contains(&host);
        }
        host == "localhost"
            || (!host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            || [".localhost", ".internal", ".svc", ".cluster.local"].iter().any(|suffix| host.ends_with(suffix))
            || self.hosts.contains(&host)
    }

    pub fn origin_ok(&self, raw: &str) -> bool {
        normalize_origin(raw).is_some_and(|origin| raw.trim().split_once("://").is_some_and(|(_, rest)| !rest.contains('/')) && self.origins.contains(&origin))
    }

    fn page_ok(&self, req: &Request) -> bool {
        let header = |name: &str| req.headers().get(name).map(|value| value.to_str().ok());
        if let Some(host) = header("host") {
            if !host.is_some_and(|host| self.host_ok(host)) {
                return false;
            }
        }
        if header("sec-fetch-site") == Some(Some("cross-site")) {
            return false;
        }
        match header("origin") {
            Some(origin) => origin.is_some_and(|origin| self.origin_ok(origin)),
            None => true,
        }
    }
}

fn normalize_origin(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        "tauri" => None,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (host, after) = inner.split_once(']')?;
            let port = if after.is_empty() { None } else { Some(after.strip_prefix(':')?) };
            (format!("[{}]", host.to_lowercase()), port)
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host.to_lowercase(), Some(port)),
            None => (authority.to_lowercase(), None),
        },
    };
    if host.is_empty() || host == "[]" || !host.chars().all(|c| c.is_ascii_alphanumeric() || "-._[]:".contains(c)) {
        return None;
    }
    let port = match port {
        None => None,
        Some(port) => Some(port.parse::<u16>().ok()?),
    };
    Some(match port.filter(|port| Some(*port) != default_port) {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

fn disabled_auth_user(req: &Request) -> Uuid {
    let local = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback())
        && !["cf-connecting-ip", "x-forwarded-for", "x-real-ip"].iter().any(|name| req.headers().contains_key(*name));
    req.headers()
        .get("x-world-user")
        .filter(|_| local)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap_or_else(|| Uuid::parse_str(DEFAULT_USER_ID).unwrap())
}

fn salt_is_cookie(req: &Request) -> bool {
    req.headers().get("authorization").is_none()
}

fn cross_site_write(req: &Request) -> bool {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    req.headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| !matches!(site, "same-origin" | "none"))
}

fn extract_token(req: &Request) -> Option<(String, String)> {
    if let Some(bearer) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        let decoded = urlencoding::decode(bearer).unwrap_or_else(|_| bearer.into());
        return Some((decoded.into_owned(), SECURE_COOKIE.to_string()));
    }

    let cookie_header = req.headers().get("cookie")?.to_str().ok()?;
    let cookies: Vec<(&str, &str)> = cookie_header
        .split(';')
        .filter_map(|c| {
            let c = c.trim();
            let (name, val) = c.split_once('=')?;
            Some((name, val))
        })
        .collect();

    for base_name in &[SECURE_COOKIE, INSECURE_COOKIE] {
        if let Some((_, val)) = cookies.iter().find(|(name, _)| *name == *base_name) {
            let decoded = urlencoding::decode(val).unwrap_or_else(|_| (*val).into());
            return Some((decoded.into_owned(), base_name.to_string()));
        }

        let mut chunks: Vec<(usize, &str)> = Vec::new();
        for (name, val) in &cookies {
            if let Some(suffix) = name.strip_prefix(base_name) {
                if let Some(idx_str) = suffix.strip_prefix('.') {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        chunks.push((idx, val));
                    }
                }
            }
        }

        if !chunks.is_empty() {
            chunks.sort_by_key(|(idx, _)| *idx);
            let combined: String = chunks.into_iter().map(|(_, v)| v).collect();
            let decoded =
                urlencoding::decode(&combined).unwrap_or_else(|_| combined.clone().into());
            return Some((decoded.into_owned(), base_name.to_string()));
        }
    }

    None
}

fn decrypt_jwe(token: &str, secret: &str, salt: &str) -> anyhow::Result<Vec<u8>> {
    use aes::Aes256;
    use cbc::cipher::{BlockDecryptMut, KeyIvInit};
    use hmac::{Hmac, Mac};
    use sha2::Sha512;

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 5 {
        anyhow::bail!("Invalid JWE: expected 5 parts, got {}", parts.len());
    }

    let header_b64 = parts[0];
    let iv_b64 = parts[2];
    let ciphertext_b64 = parts[3];
    let tag_b64 = parts[4];

    let iv = URL_SAFE_NO_PAD.decode(iv_b64)?;
    let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext_b64)?;
    let tag = URL_SAFE_NO_PAD.decode(tag_b64)?;

    let info = format!("Auth.js Generated Encryption Key ({salt})");
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt.as_bytes()), secret.as_bytes());
    let mut key = [0u8; 64];
    hk.expand(info.as_bytes(), &mut key)
        .map_err(|_| anyhow::anyhow!("HKDF expand failed"))?;

    let mac_key = &key[..32];
    let enc_key = &key[32..];

    let aad = header_b64.as_bytes();
    let al = (aad.len() as u64 * 8).to_be_bytes();

    let mut hmac =
        Hmac::<Sha512>::new_from_slice(mac_key).map_err(|_| anyhow::anyhow!("HMAC init failed"))?;
    hmac.update(aad);
    hmac.update(&iv);
    hmac.update(&ciphertext);
    hmac.update(&al);
    let mac_result = hmac.finalize().into_bytes();

    let computed_tag = &mac_result[..32];
    if !constant_time_eq(computed_tag, &tag) {
        anyhow::bail!("JWE tag verification failed");
    }

    type Aes256CbcDec = cbc::Decryptor<Aes256>;
    let decryptor = Aes256CbcDec::new_from_slices(enc_key, &iv)
        .map_err(|_| anyhow::anyhow!("AES init failed"))?;

    let mut buf = ciphertext;
    let plaintext = decryptor
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|_| anyhow::anyhow!("AES decrypt failed"))?;

    Ok(plaintext.to_vec())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn policy() -> LocalPolicy {
        LocalPolicy::build(|name| match name {
            "AUTH_DISABLED_HOSTS" => Some("loka.example.dev".to_string()),
            "AUTH_DISABLED_ORIGINS" => Some("http://192.168.1.20:3000".to_string()),
            "FRONTEND_PORT" => Some("3100".to_string()),
            _ => None,
        })
    }

    fn page(host: Option<&str>, origin: Option<&str>, site: Option<&str>) -> Request {
        let mut builder = axum::http::Request::builder().method(Method::POST).uri("/api/world/tokens");
        for (name, value) in [("host", host), ("origin", origin), ("sec-fetch-site", site)] {
            if let Some(value) = value {
                builder = builder.header(name, value);
            }
        }
        builder.body(Body::empty()).unwrap()
    }

    fn from_page(host: Option<&str>, origin: Option<&str>, site: Option<&str>) -> bool {
        policy().page_ok(&page(host, origin, site))
    }

    #[test]
    fn auth_disabled_only_answers_local_pages() {
        assert!(from_page(Some("localhost:8081"), None, None), "curl and same-origin reads");
        assert!(from_page(Some("127.0.0.1:8081"), Some("http://localhost:3000"), Some("same-site")));
        assert!(from_page(Some("127.0.0.2:8081"), Some("http://127.0.0.1:3000"), None), "all of 127.0.0.0/8 is loopback");
        assert!(from_page(Some("api:8081"), Some("http://localhost:3100"), None), "the compose frontend on a custom port");
        assert!(from_page(Some("[::1]:8081"), Some("http://[::1]:3000"), None));
        assert!(from_page(Some("loka.example.dev"), Some("https://loka.example.dev"), None), "hosts named in the environment");
        assert!(from_page(Some("api:8081"), Some("http://192.168.1.20:3000"), None), "an origin listed in AUTH_DISABLED_ORIGINS");
        assert!(from_page(Some("127.0.0.1:8081"), Some("tauri://localhost"), None), "the app's own webview");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("https://evil.example"), Some("cross-site")), "another website calling the local API");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("null"), None));
        assert!(!from_page(Some("127.0.0.1:8081"), None, Some("cross-site")), "a cross-site read without an origin");
        assert!(!from_page(Some("evil.example:8081"), None, None), "a rebound DNS name pointing at this machine");
        assert!(!from_page(Some("localhost.evil.example"), Some("http://localhost.evil.example"), None));
        assert!(!from_page(Some("printer.local"), None, None), "mDNS names are not this machine");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://localhost:3000/world"), None), "an origin never carries a path");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://user@localhost:3000"), None));
        assert_eq!(bare_host("[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(url_host("https://Loka.example.dev:8443/path"), Some("Loka.example.dev"));
    }

    #[test]
    fn public_and_lan_addresses_are_not_local() {
        assert!(!from_page(Some("8.8.8.8:8081"), None, None), "a public address in Host");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://8.8.8.8:3000"), None), "a public address in Origin");
        assert!(!from_page(Some("192.168.1.30:8081"), None, None), "a LAN address in Host");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://192.168.1.30:3000"), None), "a LAN origin nobody listed");
        assert!(!from_page(Some("[::ffff:7f00:1]:8081"), None, None), "an IPv4 mapped address");
        assert!(!from_page(Some("0.0.0.0:8081"), None, None));
    }

    #[test]
    fn origins_must_match_scheme_host_and_port() {
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://localhost:5173"), Some("same-site")), "another local dev server");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://localhost:3001"), None), "a wrong port");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("https://localhost:3000"), None), "a wrong scheme");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://loka.example.dev:9999"), None), "a named host on a port nobody listed");
        assert!(from_page(Some("127.0.0.1:8081"), Some("http://LOCALHOST:3000"), None), "hosts compare case-insensitively");
        assert!(from_page(Some("127.0.0.1:8081"), Some("https://loka.example.dev:443"), None), "the default port is the same origin");
        assert_eq!(normalize_origin("HTTP://LocalHost:80"), Some("http://localhost".to_string()));
        assert_eq!(normalize_origin("http://[::1]:3000"), Some("http://[::1]:3000".to_string()));
        assert_eq!(normalize_origin("ftp://localhost"), None);
        assert_eq!(normalize_origin("http://localhost:99999"), None);
        assert_eq!(normalize_origin("http://a@localhost"), None);
    }

    #[test]
    fn cross_site_with_an_origin_is_refused() {
        assert!(!from_page(Some("127.0.0.1:8081"), Some("http://localhost:3000"), Some("cross-site")), "even an allowed origin");
        assert!(from_page(Some("127.0.0.1:8081"), Some("http://localhost:3000"), Some("same-origin")));
    }

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn with_headers(headers: &[(&str, &str)]) -> Request {
        let mut builder = axum::http::Request::builder().method(Method::POST).uri("/api/world/agents");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn owner_access_needs_the_app_session() {
        assert!(!app_session_ok(&with_headers(&[("host", "127.0.0.1:8081")]), SECRET), "a bare loopback request");
        assert!(!app_session_ok(&with_headers(&[(APP_SESSION_HEADER, "wrong")]), SECRET));
        assert!(!app_session_ok(&with_headers(&[(APP_SESSION_HEADER, &SECRET[1..])]), SECRET), "a prefix of the secret");
        assert!(!app_session_ok(&with_headers(&[("authorization", "Bearer wld_abc")]), SECRET), "an external MCP token");
        assert!(!app_session_ok(&with_headers(&[("authorization", "Runner abc")]), SECRET), "a runner token");
        assert!(!app_session_ok(&with_headers(&[("authorization", &format!("Bearer {SECRET}"))]), SECRET), "the secret is never a bearer token");
        assert!(!app_session_ok(&with_headers(&[(APP_SESSION_HEADER, "")]), ""), "an empty secret never matches");
        assert!(app_session_ok(&with_headers(&[(APP_SESSION_HEADER, SECRET)]), SECRET), "the app's header");
        let cookie = format!("authjs.csrf-token=x; {APP_SESSION_COOKIE}={SECRET}");
        assert!(app_session_ok(&with_headers(&[("cookie", &cookie)]), SECRET), "the handed-off cookie");
        assert!(app_session_ok(&with_headers(&[("cookie", &cookie), ("sec-fetch-site", "same-origin")]), SECRET));
        assert!(!app_session_ok(&with_headers(&[("cookie", &cookie), ("sec-fetch-site", "same-site")]), SECRET), "another localhost port riding the cookie");
        assert!(!app_session_ok(&with_headers(&[("cookie", &format!("x{APP_SESSION_COOKIE}={SECRET}"))]), SECRET));
    }

    #[test]
    fn handoff_only_redirects_inside_the_app() {
        assert_eq!(safe_next(None), "/world");
        assert_eq!(safe_next(Some("/world?quest=1")), "/world?quest=1");
        assert_eq!(safe_next(Some("//evil.example")), "/world");
        assert_eq!(safe_next(Some("https://evil.example")), "/world");
        assert_eq!(safe_next(Some("/\\evil.example")), "/world");
        assert_eq!(safe_next(Some("/world\r\nSet-Cookie: x=y")), "/world");
    }

    #[test]
    fn login_codes_work_once_and_expire() {
        let now = 1_900_000_000;
        let code = login_code(SECRET, now);
        assert!(!code.contains(SECRET), "the link never carries the secret");
        assert!(!login_code_ok("ffffffffffffffffffffffffffffffff", &code, now), "a code from another launch");
        assert!(login_code_ok(SECRET, &code, now + 5));
        assert!(!login_code_ok(SECRET, &code, now + 6), "a code works once");
        let late = login_code(SECRET, now + 10);
        assert!(!login_code_ok(SECRET, &late, now + 10 + LOGIN_CODE_SECS + 1), "an expired code");
        let far = format!("{}.{}", now + 10_000, login_code_mac(SECRET, now + 10_000));
        assert!(!login_code_ok(SECRET, &far, now), "a code that claims to live too long");
        assert!(!login_code_ok(SECRET, "garbage", now));
        assert!(!login_code_ok(SECRET, SECRET, now), "the bare secret is not a code");
    }

    #[test]
    fn app_session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("loka-app-session-{}", Uuid::new_v4().simple()));
        let path = dir.join("nested").join("app-session.key");
        write_private(&path, SECRET).unwrap();
        write_private(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn request(method: Method, site: Option<&str>, bearer: bool) -> Request {
        let mut builder = axum::http::Request::builder().method(method).uri("/api/world/runs/x/stop");
        if let Some(site) = site {
            builder = builder.header("sec-fetch-site", site);
        }
        if bearer {
            builder = builder.header("authorization", "Bearer t");
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn cookie_writes_from_other_sites_are_refused() {
        assert!(cross_site_write(&request(Method::POST, Some("cross-site"), false)));
        assert!(cross_site_write(&request(Method::DELETE, Some("same-site"), false)));
        assert!(!cross_site_write(&request(Method::POST, Some("same-origin"), false)));
        assert!(!cross_site_write(&request(Method::POST, None, false)));
        assert!(!cross_site_write(&request(Method::GET, Some("cross-site"), false)));
        assert!(salt_is_cookie(&request(Method::POST, None, false)));
        assert!(!salt_is_cookie(&request(Method::POST, None, true)));
    }

    #[test]
    fn impersonation_header_needs_a_loopback_peer() {
        let wanted = "11111111-1111-1111-1111-111111111111";
        let from = |peer: Option<&str>| {
            let mut req = axum::http::Request::builder().uri("/").header("x-world-user", wanted).body(Body::empty()).unwrap();
            if let Some(peer) = peer {
                req.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            }
            disabled_auth_user(&req).to_string()
        };
        assert_eq!(from(Some("127.0.0.1:5000")), wanted);
        assert_eq!(from(Some("[::1]:5000")), wanted);
        assert_eq!(from(Some("10.0.0.7:5000")), DEFAULT_USER_ID);
        assert_eq!(from(None), DEFAULT_USER_ID);
    }
}
