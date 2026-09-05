use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Request},
    http::{Method, StatusCode},
    middleware::Next,
    response::Response,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct UserId(pub Uuid);

const DEFAULT_USER_ID: &str = "00000000-0000-0000-0000-000000000000";

const SECURE_COOKIE: &str = "__Secure-authjs.session-token";
const INSECURE_COOKIE: &str = "authjs.session-token";

pub async fn auth_middleware(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let auth_disabled = std::env::var("AUTH_DISABLED").unwrap_or_default() == "true";

    if auth_disabled {
        if req.headers().contains_key("cf-connecting-ip") {
            return Err(StatusCode::UNAUTHORIZED);
        }
        if !local_caller(&req) {
            tracing::warn!(
                host = ?req.headers().get("host"),
                origin = ?req.headers().get("origin"),
                "AUTH_DISABLED=true refused a request that did not come from a local page; list extra hosts in AUTH_DISABLED_HOSTS if this was you"
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
        let uid = disabled_auth_user(&req);
        req.extensions_mut().insert(UserId(uid));
        return Ok(next.run(req).await);
    }

    let (token, salt) = extract_token(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    if salt_is_cookie(&req) && cross_site_write(&req) {
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

    req.extensions_mut().insert(UserId(uid));
    Ok(next.run(req).await)
}

pub fn session_secret() -> Option<String> {
    std::env::var("AUTH_SECRET").ok().filter(|secret| secret.trim().len() >= 16)
}

pub fn default_bind_host() -> &'static str {
    let auth_disabled = std::env::var("AUTH_DISABLED").unwrap_or_default() == "true";
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

fn local_host(raw: &str, extra: &[String]) -> bool {
    let host = bare_host(raw).trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !host.contains('.')
        || [".localhost", ".local", ".internal", ".lan", ".home.arpa", ".svc", ".cluster.local"].iter().any(|suffix| host.ends_with(suffix))
        || extra.contains(&host)
}

fn allowed_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = std::env::var("AUTH_DISABLED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|h| bare_host(h).to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    for name in ["WORLD_APP_URL", "WORLD_PUBLIC_API_URL", "AUTH_URL", "NEXTAUTH_URL"] {
        if let Some(host) = std::env::var(name).ok().as_deref().and_then(url_host) {
            hosts.push(host.to_lowercase());
        }
    }
    hosts
}

fn local_caller(req: &Request) -> bool {
    came_from_a_local_page(req, &allowed_hosts())
}

fn came_from_a_local_page(req: &Request, extra: &[String]) -> bool {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    if header("host").is_some_and(|host| !local_host(host, extra)) {
        return false;
    }
    match header("origin") {
        Some(origin) => url_host(origin).is_some_and(|host| local_host(host, extra)),
        None => header("sec-fetch-site") != Some("cross-site"),
    }
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

    fn from_page(host: Option<&str>, origin: Option<&str>, site: Option<&str>) -> bool {
        let mut builder = axum::http::Request::builder().method(Method::POST).uri("/api/world/tokens");
        for (name, value) in [("host", host), ("origin", origin), ("sec-fetch-site", site)] {
            if let Some(value) = value {
                builder = builder.header(name, value);
            }
        }
        came_from_a_local_page(&builder.body(Body::empty()).unwrap(), &["loka.example.dev".to_string()])
    }

    #[test]
    fn auth_disabled_only_answers_local_pages() {
        assert!(from_page(Some("localhost:8081"), None, None), "curl and same-origin reads");
        assert!(from_page(Some("127.0.0.1:8081"), Some("http://localhost:3000"), Some("same-site")));
        assert!(from_page(Some("api:8081"), Some("http://192.168.1.20:3000"), None), "the compose frontend and a LAN browser");
        assert!(from_page(Some("[::1]:8081"), Some("http://[::1]:3000"), None));
        assert!(from_page(Some("loka.example.dev"), Some("https://loka.example.dev"), None), "hosts named in the environment");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("https://evil.example"), Some("cross-site")), "another website calling the local API");
        assert!(!from_page(Some("127.0.0.1:8081"), Some("null"), None));
        assert!(!from_page(Some("127.0.0.1:8081"), None, Some("cross-site")), "a cross-site read without an origin");
        assert!(!from_page(Some("evil.example:8081"), None, None), "a rebound DNS name pointing at this machine");
        assert!(!from_page(Some("localhost.evil.example"), Some("http://localhost.evil.example"), None));
        assert_eq!(bare_host("[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(url_host("https://Loka.example.dev:8443/path"), Some("Loka.example.dev"));
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
