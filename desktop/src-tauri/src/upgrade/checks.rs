use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

pub(crate) const PRO_SITE: &str = "https://loka.ygpt.dev";
pub(crate) const RETURN_LIMIT: usize = 2048;
pub(crate) const ORG_LIMIT: usize = 80;

pub(crate) fn pro_site() -> String {
    if cfg!(debug_assertions) {
        if let Some(site) = std::env::var("LOKA_PRO_URL").ok().map(|v| v.trim().trim_end_matches('/').to_string()).filter(|v| !v.is_empty()) {
            return site;
        }
    }
    PRO_SITE.to_string()
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Ticket {
    pub(crate) ticket: String,
    pub(crate) api: String,
    pub(crate) app: String,
    pub(crate) org: String,
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ticket").field("ticket", &"***").field("api", &self.api).field("app", &self.app).field("org", &self.org).finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Arrival {
    NotOurs,
    WrongState,
    Invalid,
    Ticket(Ticket),
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn random_hex(len: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = vec![0u8; len];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

pub(crate) fn same_secret(given: &str, wanted: &str) -> bool {
    let (given, wanted) = (given.as_bytes(), wanted.as_bytes());
    if given.len() != wanted.len() || wanted.is_empty() {
        return false;
    }
    given.iter().zip(wanted).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

pub(crate) fn return_url(port: u16) -> Option<String> {
    if port == 0 {
        return None;
    }
    let url = format!("http://127.0.0.1:{port}/moved");
    (url.len() <= RETURN_LIMIT).then_some(url)
}

pub(crate) fn upgrade_url(site: &str, port: u16, state: &str) -> Option<String> {
    let mut url = Url::parse(site).ok()?.join("/upgrade").ok()?;
    url.query_pairs_mut().append_pair("return", &return_url(port)?).append_pair("state", state);
    Some(url.to_string())
}

fn host_of(site: &str) -> Option<String> {
    Url::parse(site).ok()?.host_str().map(str::to_ascii_lowercase)
}

fn bare_origin(url: &Url) -> bool {
    url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none() && matches!(url.path(), "" | "/")
}

fn origin_text(url: &Url) -> String {
    url.origin().ascii_serialization()
}

pub(crate) fn api_origin(raw: &str, site: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;
    if !bare_origin(&url) {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if url.scheme() == "https" {
        let pro = host_of(site)?;
        if host == pro || host.ends_with(&format!(".{pro}")) {
            return Some(origin_text(&url));
        }
        return None;
    }
    if cfg!(debug_assertions) && url.scheme() == "http" && host == "127.0.0.1" && url.port().is_some() {
        return Some(origin_text(&url));
    }
    None
}

pub(crate) fn app_origin(raw: &str, site: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;
    let wanted = Url::parse(site).ok()?;
    if !bare_origin(&url) || origin_text(&url) != origin_text(&wanted) {
        return None;
    }
    if url.scheme() != "https" && !cfg!(debug_assertions) {
        return None;
    }
    Some(origin_text(&url))
}

pub(crate) fn valid_ticket(ticket: &str) -> bool {
    ticket.len() == 67 && ticket.starts_with("mv_") && ticket[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn org_name(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = clean.trim();
    let name: String = trimmed.chars().take(ORG_LIMIT).collect();
    if name.is_empty() {
        "Loka Pro".to_string()
    } else {
        name
    }
}

pub(crate) fn request_target(head: &str) -> Option<(&str, &str)> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    Some((method, target))
}

pub(crate) fn arrival(method: &str, target: &str, state: &str, site: &str) -> Arrival {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method != "GET" || path != "/moved" {
        return Arrival::NotOurs;
    }
    let mut fields: HashMap<String, String> = HashMap::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if fields.insert(name.into_owned(), value.into_owned()).is_some() {
            return Arrival::Invalid;
        }
    }
    if !fields.get("state").is_some_and(|given| same_secret(given, state)) {
        return Arrival::WrongState;
    }
    let ticket = fields.get("ticket").filter(|t| valid_ticket(t));
    let api = fields.get("api").and_then(|raw| api_origin(raw, site));
    let app = fields.get("app").and_then(|raw| app_origin(raw, site));
    match (ticket, api, app) {
        (Some(ticket), Some(api), Some(app)) => Arrival::Ticket(Ticket {
            ticket: ticket.clone(),
            api,
            app,
            org: org_name(fields.get("org").map(String::as_str).unwrap_or_default()),
        }),
        _ => Arrival::Invalid,
    }
}

const PAGE_DONE: &str = "Loka on your Mac is taking it from here. You can close this tab.";
const PAGE_WRONG: &str = "This link does not match the upgrade Loka started on this Mac. Go back to Loka and try again.";
const PAGE_MISSING: &str = "Nothing here.";

pub(crate) fn http_page(status: u16, text: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Loka</title></head><body style=\"font-family:-apple-system,system-ui,sans-serif;margin:4rem auto;max-width:32rem;line-height:1.5;padding:0 1rem\"><h1 style=\"font-size:1.25rem\">Loka</h1><p>{text}</p></body></html>"
    );
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

pub(crate) fn reply_for(arrival: &Arrival) -> Vec<u8> {
    match arrival {
        Arrival::Ticket(_) => http_page(200, PAGE_DONE),
        Arrival::WrongState | Arrival::Invalid => http_page(400, PAGE_WRONG),
        Arrival::NotOurs => http_page(404, PAGE_MISSING),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ProRelease {
    pub(crate) version: String,
    pub(crate) sha256: String,
    #[serde(default)]
    pub(crate) size: Option<u64>,
    #[serde(default)]
    pub(crate) team_id: Option<String>,
    pub(crate) bundle_id: String,
    pub(crate) download_url: String,
    #[serde(default)]
    pub(crate) download_until: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Launch {
    #[serde(default)]
    pub(crate) bundle_id: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ProInfo {
    #[serde(default)]
    pub(crate) server: String,
    #[serde(default)]
    pub(crate) app_url: String,
    #[serde(default)]
    pub(crate) download_page: String,
    #[serde(default)]
    pub(crate) moved: bool,
    #[serde(default)]
    pub(crate) pro: Option<ProRelease>,
    #[serde(default)]
    pub(crate) launch: Launch,
}

pub(crate) fn download_page(info: &ProInfo, site: &str) -> String {
    Url::parse(&info.download_page)
        .ok()
        .filter(|url| url.scheme() == "https" && host_of(url.as_str()) == host_of(site))
        .map(|url| url.to_string())
        .unwrap_or_else(|| format!("{site}/download/pro"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MoveCounts {
    #[serde(default)]
    pub(crate) agents: u64,
    #[serde(default)]
    pub(crate) reports: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MoveSummary {
    #[serde(default)]
    pub(crate) counts: MoveCounts,
    #[serde(default)]
    pub(crate) keys_moved: Vec<String>,
    #[serde(default)]
    pub(crate) renamed: Vec<String>,
    #[serde(default)]
    pub(crate) needs_values: Vec<String>,
    #[serde(default)]
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Moved {
    #[serde(default)]
    pub(crate) summary: MoveSummary,
    pub(crate) next: ProInfo,
}

pub(crate) fn server_error(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let text: String = value.get("error")?.as_str()?.chars().filter(|c| !c.is_control()).take(400).collect();
    Some(text.trim().to_string()).filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "6f1c0b7a9d2e4f3812ab34cd56ef7890a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn ticket() -> String {
        format!("mv_{}", "ab12".repeat(16))
    }

    fn target(fields: &[(&str, &str)]) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in fields {
            query.append_pair(name, value);
        }
        format!("/moved?{}", query.finish())
    }

    #[test]
    fn the_ticket_comes_back_only_with_our_state() {
        let t = ticket();
        let good = target(&[("ticket", &t), ("api", "https://api.loka.ygpt.dev"), ("app", "https://loka.ygpt.dev"), ("org", "Acme"), ("state", STATE)]);
        match arrival("GET", &good, STATE, PRO_SITE) {
            Arrival::Ticket(found) => {
                assert_eq!(found.ticket, t);
                assert_eq!(found.api, "https://api.loka.ygpt.dev");
                assert_eq!(found.app, "https://loka.ygpt.dev");
                assert_eq!(found.org, "Acme");
                assert!(!format!("{found:?}").contains(&t));
            }
            other => panic!("expected a ticket, got {other:?}"),
        }
        let wrong = target(&[("ticket", &t), ("api", "https://loka.ygpt.dev"), ("app", "https://loka.ygpt.dev"), ("state", &STATE.replace('6', "7"))]);
        assert_eq!(arrival("GET", &wrong, STATE, PRO_SITE), Arrival::WrongState);
        let none = target(&[("ticket", &t), ("api", "https://loka.ygpt.dev"), ("app", "https://loka.ygpt.dev")]);
        assert_eq!(arrival("GET", &none, STATE, PRO_SITE), Arrival::WrongState);
        assert_eq!(arrival("POST", &good, STATE, PRO_SITE), Arrival::NotOurs);
        assert_eq!(arrival("GET", "/favicon.ico", STATE, PRO_SITE), Arrival::NotOurs);
        assert_eq!(arrival("GET", "/movedx?state=1", STATE, PRO_SITE), Arrival::NotOurs);
        let twice = format!("{good}&state={STATE}");
        assert_eq!(arrival("GET", &twice, STATE, PRO_SITE), Arrival::Invalid);
        let bad_ticket = target(&[("ticket", "mv_ABC"), ("api", "https://loka.ygpt.dev"), ("app", "https://loka.ygpt.dev"), ("state", STATE)]);
        assert_eq!(arrival("GET", &bad_ticket, STATE, PRO_SITE), Arrival::Invalid);
        let bad_app = target(&[("ticket", &t), ("api", "https://loka.ygpt.dev"), ("app", "https://evil.example"), ("state", STATE)]);
        assert_eq!(arrival("GET", &bad_app, STATE, PRO_SITE), Arrival::Invalid);
    }

    #[test]
    fn api_origins_stay_on_the_pro_site() {
        assert_eq!(api_origin("https://loka.ygpt.dev", PRO_SITE).as_deref(), Some("https://loka.ygpt.dev"));
        assert_eq!(api_origin("https://api.loka.ygpt.dev/", PRO_SITE).as_deref(), Some("https://api.loka.ygpt.dev"));
        assert_eq!(api_origin("https://API.Loka.ygpt.dev:8443", PRO_SITE).as_deref(), Some("https://api.loka.ygpt.dev:8443"));
        for bad in [
            "http://loka.ygpt.dev",
            "https://evilloka.ygpt.dev",
            "https://loka.ygpt.dev.evil.example",
            "https://user:pw@loka.ygpt.dev",
            "https://loka.ygpt.dev/api",
            "https://loka.ygpt.dev/?x=1",
            "https://ygpt.dev",
            "javascript:alert(1)",
            "",
        ] {
            assert_eq!(api_origin(bad, PRO_SITE), None, "{bad}");
        }
        assert_eq!(api_origin("http://127.0.0.1:8081", PRO_SITE).is_some(), cfg!(debug_assertions));
        assert_eq!(api_origin("http://127.0.0.1", PRO_SITE), None);
        assert_eq!(api_origin("http://localhost:8081", PRO_SITE), None);
    }

    #[test]
    fn app_origin_is_the_pro_site_itself() {
        assert_eq!(app_origin("https://loka.ygpt.dev/", PRO_SITE).as_deref(), Some("https://loka.ygpt.dev"));
        assert_eq!(app_origin("https://api.loka.ygpt.dev", PRO_SITE), None);
        assert_eq!(app_origin("http://loka.ygpt.dev", PRO_SITE), None);
    }

    #[test]
    fn tickets_are_mv_and_64_lowercase_hex() {
        assert!(valid_ticket(&ticket()));
        assert!(!valid_ticket(&ticket().to_uppercase()));
        assert!(!valid_ticket(&format!("mv_{}", "a".repeat(63))));
        assert!(!valid_ticket(&format!("mv_{}", "g".repeat(64))));
        assert!(!valid_ticket(&format!("wld_{}", "a".repeat(64))));
    }

    #[test]
    fn org_names_are_short_and_plain() {
        assert_eq!(org_name(&"a".repeat(200)).chars().count(), ORG_LIMIT);
        assert_eq!(org_name("  Acme\n\tTeam "), "AcmeTeam");
        assert_eq!(org_name(""), "Loka Pro");
    }

    #[test]
    fn the_upgrade_page_gets_a_loopback_return() {
        let url = upgrade_url(PRO_SITE, 51234, STATE).unwrap();
        assert_eq!(url, format!("https://loka.ygpt.dev/upgrade?return=http%3A%2F%2F127.0.0.1%3A51234%2Fmoved&state={STATE}"));
        assert_eq!(return_url(0), None);
        assert_eq!(return_url(65535).as_deref(), Some("http://127.0.0.1:65535/moved"));
    }

    #[test]
    fn request_lines_are_read_strictly() {
        assert_eq!(request_target("GET /moved?a=1 HTTP/1.1\r\nHost: x\r\n\r\n"), Some(("GET", "/moved?a=1")));
        assert_eq!(request_target("GET /moved HTTP/2\r\n"), None);
        assert_eq!(request_target("GET /a b HTTP/1.1\r\n"), None);
        assert_eq!(request_target(""), None);
    }

    #[test]
    fn replies_never_leak_the_referrer_or_cache() {
        let page = String::from_utf8(http_page(200, PAGE_DONE)).unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.contains("Cache-Control: no-store\r\n"));
        assert!(page.contains("Referrer-Policy: no-referrer\r\n"));
        assert!(page.contains(PAGE_DONE));
        assert!(String::from_utf8(reply_for(&Arrival::WrongState)).unwrap().starts_with("HTTP/1.1 400"));
        assert!(String::from_utf8(reply_for(&Arrival::NotOurs)).unwrap().starts_with("HTTP/1.1 404"));
        assert!(!page.contains('\u{2014}'));
    }

    #[test]
    fn secrets_compare_in_full() {
        assert!(same_secret(STATE, STATE));
        assert!(!same_secret(&STATE[1..], STATE));
        assert!(!same_secret("", ""));
        assert_eq!(random_hex(32).unwrap().len(), 64);
    }

    #[test]
    fn pro_info_reads_the_server_shape() {
        let info: ProInfo = serde_json::from_value(serde_json::json!({
            "server": "https://loka.ygpt.dev",
            "app_url": "https://loka.ygpt.dev/world",
            "download_page": "https://loka.ygpt.dev/download/pro",
            "moved": false,
            "pro": {"version": "0.3.1", "sha256": "a".repeat(64), "size": null, "team_id": "ABCDE12345", "bundle_id": "dev.ygpt.loka.pro", "download_url": "https://loka.ygpt.dev/api/move/pro-dmg?version=0.3.1", "download_until": "2026-11-23T08:15:00Z"},
            "launch": {"bundle_id": "dev.ygpt.loka.pro", "args": ["--loka-handover", "https://loka.ygpt.dev", "--loka-handover-id", "0123"]},
        }))
        .unwrap();
        assert_eq!(download_page(&info, PRO_SITE), "https://loka.ygpt.dev/download/pro");
        let mut elsewhere = info.clone();
        elsewhere.download_page = "https://evil.example/pro".into();
        assert_eq!(download_page(&elsewhere, PRO_SITE), "https://loka.ygpt.dev/download/pro");
    }

    #[test]
    fn server_errors_are_shown_as_given() {
        assert_eq!(server_error(br#"{"error":"Could not move your Loka. Nothing was changed."}"#).as_deref(), Some("Could not move your Loka. Nothing was changed."));
        assert_eq!(server_error(b"Payload Too Large"), None);
        assert_eq!(server_error(br#"{"error":""}"#), None);
    }
}
