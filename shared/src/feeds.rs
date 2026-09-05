use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::feed_intake::{self, Intake, Outcome};
use crate::world::{emit_in, NewEvent, WorldStore};

pub const FEED_EVENT: &str = "feed.item";
pub const KINDS: &[&str] = &["rss", "webhook"];
pub const FORMATS: &[&str] = &["auto", "alertmanager", "json", "text"];
pub const MAX_ITEMS_PER_DELIVERY: usize = 20;
pub const INBOX_ITEMS_CAP: usize = 50;
pub const SEEN_CAP: usize = 1000;
pub const FEEDS_PER_AGENT: i64 = 50;
pub const DEFAULT_POLL_SECS: i32 = 900;
pub const MIN_POLL_SECS: i32 = 60;
pub const MAX_POLL_SECS: i32 = 86400;
pub const WEBHOOK_BODY_LIMIT: usize = 256 * 1024;
const FETCH_BODY_LIMIT: usize = 16 * 1024 * 1024;
const SUMMARY_CHARS: usize = 700;
const TITLE_CHARS: usize = 300;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct AgentFeed {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub owner_user_id: Option<Uuid>,
    pub created_by_agent: Option<Uuid>,
    pub kind: String,
    pub name: String,
    pub url: Option<String>,
    #[serde(skip)]
    pub token: Option<String>,
    pub format: String,
    pub filter: Option<String>,
    pub poll_secs: i32,
    pub next_poll_at: Option<DateTime<Utc>>,
    pub last_polled_at: Option<DateTime<Utc>>,
    pub last_item_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    #[serde(skip)]
    pub etag: Option<String>,
    #[serde(skip)]
    pub last_modified: Option<String>,
    #[serde(skip)]
    pub seen: Vec<String>,
    pub primed: bool,
    pub item_count: i64,
    pub paused: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip)]
    pub secret_ciphertext: Option<Vec<u8>>,
    #[serde(skip)]
    pub secret_nonce: Option<Vec<u8>>,
    pub secret_set_at: Option<DateTime<Utc>>,
    pub secret_shown_at: Option<DateTime<Utc>>,
    pub allowed_ips: Vec<String>,
    pub auth_grace_until: Option<DateTime<Utc>>,
    pub authed_at: Option<DateTime<Utc>>,
    pub unauth_at: Option<DateTime<Utc>>,
    pub auth_mode: String,
    pub intake: Value,
    pub last_fields: Vec<String>,
    pub turned_away: i64,
    pub turned_away_at: Option<DateTime<Utc>>,
    pub turned_away_why: Option<String>,
}

impl AgentFeed {
    pub fn webhook_url(&self) -> Option<String> {
        let token = self.token.as_deref()?;
        Some(format!("{}/webhooks/feeds/{token}", public_base().unwrap_or_else(|| "<your Loka API address>".into())))
    }

    pub fn view(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let (Some(url), Some(fields)) = (self.webhook_url(), v.as_object_mut()) {
            fields.insert("webhook_url".into(), json!(url));
            fields.insert("auth".into(), json!(self.auth_state(Utc::now())));
            fields.insert("has_secret".into(), json!(self.secret_ciphertext.is_some()));
            fields.insert("secret_unseen".into(), json!(self.secret_ciphertext.is_some() && self.secret_shown_at.is_none()));
            fields.insert("intake".into(), json!(self.intake()));
            fields.insert("checks".into(), json!(self.intake().describe()));
        }
        v
    }

    pub fn intake(&self) -> Intake {
        Intake::from_value(&self.intake)
    }

    pub fn by_address(&self) -> bool {
        self.auth_mode == "address"
    }

    pub fn source(&self) -> String {
        match self.kind.as_str() {
            "rss" => self.url.clone().unwrap_or_default(),
            _ => match self.intake().source {
                sender if sender.is_empty() => "webhook".into(),
                sender => format!("webhook from {sender}"),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedItem {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<DateTime<Utc>>,
}

pub struct NewFeed<'a> {
    pub agent_id: Uuid,
    pub owner_user_id: Option<Uuid>,
    pub created_by_agent: Option<Uuid>,
    pub kind: &'a str,
    pub name: &'a str,
    pub url: Option<&'a str>,
    pub format: &'a str,
    pub filter: Option<&'a str>,
    pub poll_secs: i32,
    pub seen: Vec<String>,
    pub primed: bool,
    pub allowed_ips: Vec<String>,
    pub by_address: bool,
    pub intake: Intake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    Secret,
    Address,
    Unverified,
}

impl Checked {
    pub fn label(self) -> &'static str {
        match self {
            Checked::Secret => "secret",
            Checked::Address => "address",
            Checked::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Intaken {
    Delivered(usize),
    Skipped(String),
    Invalid(String),
}

pub enum Fetched {
    NotModified,
    Feed {
        title: Option<String>,
        items: Vec<FeedItem>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

pub fn public_base() -> Option<String> {
    ["WORLD_PUBLIC_API_URL", "AGENT_WEBHOOK_URL"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().trim_end_matches('/').trim_end_matches("/webhooks").to_string())
        .find(|v| !v.is_empty())
}

pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn check_ranges(ranges: &[String]) -> Result<()> {
    crate::feed_auth::clean_ranges(ranges).map(|_| ())
}

pub fn clamp_poll_secs(secs: i64) -> i32 {
    secs.clamp(MIN_POLL_SECS as i64, MAX_POLL_SECS as i64) as i32
}

pub fn matches_filter(filter: Option<&str>, item: &FeedItem) -> bool {
    let Some(filter) = filter.map(str::trim).filter(|f| !f.is_empty()) else {
        return true;
    };
    let hay = format!("{} {} {}", item.title, item.summary, item.link.as_deref().unwrap_or("")).to_lowercase();
    let mut wanted = Vec::new();
    for term in filter.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        match term.strip_prefix('-').map(str::trim) {
            Some(bad) if !bad.is_empty() => {
                if hay.contains(bad) {
                    return false;
                }
            }
            Some(_) => {}
            None => wanted.push(term),
        }
    }
    wanted.is_empty() || wanted.iter().any(|t| hay.contains(t.as_str()))
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}...", s[..cut].trim_end()),
        None => s.to_string(),
    }
}

pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_feed(bytes: &[u8]) -> Result<(Option<String>, Vec<FeedItem>)> {
    let feed = feed_rs::parser::parse(bytes).map_err(|e| anyhow!("not an RSS, Atom or JSON feed: {e}"))?;
    let mut items: Vec<FeedItem> = feed
        .entries
        .into_iter()
        .map(|e| {
            let link = e.links.iter().map(|l| l.href.trim()).find(|h| !h.is_empty()).map(String::from);
            let title = e.title.map(|t| plain_text(&t.content)).filter(|t| !t.is_empty());
            let body = e
                .summary
                .map(|t| t.content)
                .or_else(|| e.content.and_then(|c| c.body))
                .map(|b| plain_text(&b))
                .unwrap_or_default();
            let id = Some(e.id.trim().to_string())
                .filter(|i| !i.is_empty())
                .or_else(|| link.clone())
                .or_else(|| title.clone())
                .unwrap_or_default();
            FeedItem {
                id,
                title: truncate(&title.unwrap_or_else(|| truncate(&body, 80)), TITLE_CHARS),
                link,
                summary: truncate(&body, SUMMARY_CHARS),
                published: e.published.or(e.updated),
            }
        })
        .filter(|i| !i.id.is_empty())
        .collect();
    if items.iter().all(|i| i.published.is_some()) {
        items.sort_by_key(|i| std::cmp::Reverse(i.published));
    }
    Ok((feed.title.map(|t| plain_text(&t.content)).filter(|t| !t.is_empty()), items))
}

fn allow_private() -> bool {
    std::env::var("WORLD_FEEDS_ALLOW_PRIVATE").map(|v| v == "true" || v == "1").unwrap_or(false)
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (18..20).contains(&o[1])))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| IpAddr::V4(std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8));
            if seg[..6].iter().all(|s| *s == 0) {
                return public_ip(embedded(seg[6], seg[7]));
            }
            if seg[0] == 0x64 && seg[1] == 0xff9b {
                return public_ip(embedded(seg[6], seg[7]));
            }
            if seg[0] == 0x2002 {
                return public_ip(embedded(seg[1], seg[2]));
            }
            let first = seg[0];
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80)
        }
    }
}

struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|a| public_ip(a.ip())).collect();
            if addrs.is_empty() {
                return Err(format!("{host} resolves only to private addresses").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn host_allowed(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => public_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => public_ip(IpAddr::V6(ip)),
        Some(url::Host::Domain(d)) => !matches!(d.to_ascii_lowercase().as_str(), "localhost") && !d.ends_with(".localhost") && !d.ends_with(".internal"),
        None => false,
    }
}

pub fn public_only_client(connect_timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(connect_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(PublicResolver))
        .build()?)
}

pub fn public_url(raw: &str) -> bool {
    reqwest::Url::parse(raw.trim())
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && host_allowed(&url))
}

pub fn check_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|e| anyhow!("{raw:?} is not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http or https URLs can be fetched");
    }
    if !allow_private() && !host_allowed(&url) {
        bail!("the URL must be on the public internet, not a private or local address");
    }
    Ok(url)
}

fn client() -> Result<reqwest::Client> {
    let private = allow_private();
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Loka-feed-reader/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if !private && !host_allowed(attempt.url()) {
                attempt.error("redirected to a private address")
            } else {
                attempt.follow()
            }
        }));
    if !private {
        builder = builder.dns_resolver(Arc::new(PublicResolver));
    }
    Ok(builder.build()?)
}

pub async fn get_public_text(url: &str, limit: usize) -> Result<String> {
    let mut resp = get_public(url).await?;
    if resp.content_length().is_some_and(|len| len > limit as u64) {
        bail!("the file is larger than {} KB", limit / 1024);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > limit {
            bail!("the file is larger than {} KB", limit / 1024);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

pub async fn get_public(url: &str) -> Result<reqwest::Response> {
    let url = check_url(url)?;
    Ok(client()?.get(url).send().await?.error_for_status()?)
}

pub async fn fetch(url: &str, etag: Option<&str>, last_modified: Option<&str>) -> Result<Fetched> {
    let url = check_url(url)?;
    let mut req = client()?.get(url).header(
        reqwest::header::ACCEPT,
        "application/rss+xml, application/atom+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.9, */*;q=0.5",
    );
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    if let Some(at) = last_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, at);
    }
    let mut resp = req.send().await.map_err(|e| anyhow!("could not fetch the feed: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !resp.status().is_success() {
        bail!("the feed answered HTTP {}", resp.status());
    }
    let header = |name: reqwest::header::HeaderName| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let etag = header(reqwest::header::ETAG);
    let last_modified = header(reqwest::header::LAST_MODIFIED);
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| anyhow!("could not read the feed: {e}"))? {
        if body.len() + chunk.len() > FETCH_BODY_LIMIT {
            bail!("the feed is larger than {} MB", FETCH_BODY_LIMIT / 1024 / 1024);
        }
        body.extend_from_slice(&chunk);
    }
    let (title, items) = parse_feed(&body)?;
    Ok(Fetched::Feed { title, items, etag, last_modified })
}

fn label_list(v: Option<&Value>, skip: &[&str]) -> String {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| !skip.contains(&k.as_str()))
                .map(|(k, v)| format!("{k}={}", v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

pub fn is_alertmanager(v: &Value) -> bool {
    v.get("alerts").is_some_and(Value::is_array) && (v.get("receiver").is_some() || v.get("groupKey").is_some() || v.get("version").is_some())
}

pub fn alertmanager_items(v: &Value) -> Vec<FeedItem> {
    let s = |val: Option<&Value>| val.and_then(Value::as_str).unwrap_or("").trim().to_string();
    let common = v.get("commonLabels");
    let group = label_list(v.get("groupLabels"), &["alertname"]);
    let alertname = s(common.and_then(|c| c.get("alertname"))).to_string();
    let group_key = s(v.get("groupKey"));
    let external = Some(s(v.get("externalURL"))).filter(|u| !u.is_empty());
    let alerts: Vec<&Value> = v.get("alerts").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let mut out = Vec::new();
    for status in ["firing", "resolved"] {
        let mine: Vec<&&Value> = alerts.iter().filter(|a| s(a.get("status")) == status).collect();
        if mine.is_empty() {
            continue;
        }
        let mut lines = Vec::new();
        let mut stamp = String::new();
        for a in mine.iter().take(MAX_ITEMS_PER_DELIVERY) {
            let labels = label_list(a.get("labels"), &["alertname"]);
            let ann = a.get("annotations");
            let text = [s(ann.and_then(|x| x.get("summary"))), s(ann.and_then(|x| x.get("description")))]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" - ");
            let name = s(a.get("labels").and_then(|l| l.get("alertname")));
            let when = if status == "firing" { s(a.get("startsAt")) } else { s(a.get("endsAt")) };
            stamp.push_str(&s(a.get("fingerprint")));
            stamp.push_str(&when);
            let mut line = format!("{} [{}]", if name.is_empty() { "alert" } else { &name }, labels);
            if !text.is_empty() {
                line.push_str(&format!(": {text}"));
            }
            if !when.is_empty() {
                line.push_str(&format!(" ({} {when})", if status == "firing" { "since" } else { "ended" }));
            }
            let source = s(a.get("generatorURL"));
            if !source.is_empty() {
                line.push_str(&format!(" {source}"));
            }
            lines.push(line);
        }
        if mine.len() > MAX_ITEMS_PER_DELIVERY {
            lines.push(format!("and {} more", mine.len() - MAX_ITEMS_PER_DELIVERY));
        }
        let what = if alertname.is_empty() { "alerts".to_string() } else { alertname.clone() };
        out.push(FeedItem {
            id: format!("am:{group_key}:{status}:{:x}", fnv(&stamp)),
            title: truncate(&format!("[{}:{}] {}{}", status.to_uppercase(), mine.len(), what, if group.is_empty() { String::new() } else { format!(" ({group})") }), TITLE_CHARS),
            link: external.clone(),
            summary: truncate(&lines.join("\n"), 4000),
            published: Some(Utc::now()),
        });
    }
    out
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub(crate) fn json_item(v: &Value, n: usize) -> FeedItem {
    let pick = |keys: &[&str]| keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from));
    let title = pick(&["title", "subject", "summary", "message", "text", "name", "event", "action", "alert", "type"]).unwrap_or_else(|| format!("webhook payload {}", n + 1));
    let link = pick(&["url", "link", "html_url", "href"]);
    let raw = serde_json::to_string(v).unwrap_or_default();
    FeedItem {
        id: format!("json:{:x}", fnv(&raw)),
        title: truncate(&title, TITLE_CHARS),
        link,
        summary: truncate(&raw, 1500),
        published: Some(Utc::now()),
    }
}

pub fn webhook_items(format: &str, body: &[u8]) -> Vec<FeedItem> {
    let text = String::from_utf8_lossy(body);
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let format = match (format, &parsed) {
        ("auto", Some(v)) if is_alertmanager(v) => "alertmanager",
        ("auto", Some(_)) => "json",
        ("auto", None) => "text",
        (f, None) if f != "text" => "text",
        (f, _) => f,
    };
    match (format, parsed) {
        ("alertmanager", Some(v)) if is_alertmanager(&v) => alertmanager_items(&v),
        ("alertmanager", Some(v)) | ("json", Some(v)) => match v {
            Value::Array(list) => list.iter().take(MAX_ITEMS_PER_DELIVERY).enumerate().map(|(n, x)| json_item(x, n)).collect(),
            other => vec![json_item(&other, 0)],
        },
        _ => {
            let text = text.trim();
            if text.is_empty() {
                return Vec::new();
            }
            let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
            vec![FeedItem {
                id: format!("text:{:x}", fnv(text)),
                title: truncate(first, TITLE_CHARS),
                link: None,
                summary: truncate(rest, 1500),
                published: Some(Utc::now()),
            }]
        }
    }
}

pub(crate) const FEED_COLUMNS: &str = "id, agent_id, owner_user_id, created_by_agent, kind, name, url, token, format, filter, poll_secs, next_poll_at, last_polled_at, last_item_at, last_error, etag, last_modified, seen, primed, item_count, paused, created_at, updated_at, secret_ciphertext, secret_nonce, secret_set_at, secret_shown_at, allowed_ips, auth_grace_until, authed_at, unauth_at, auth_mode, intake, last_fields, turned_away, turned_away_at, turned_away_why";

impl WorldStore {
    pub async fn create_feed(&self, new: NewFeed<'_>) -> Result<AgentFeed> {
        if !KINDS.contains(&new.kind) {
            bail!("unknown feed kind {}; use rss or webhook", new.kind);
        }
        if !FORMATS.contains(&new.format) {
            bail!("unknown format {}; use {}", new.format, FORMATS.join(", "));
        }
        let name = new.name.trim();
        if name.is_empty() {
            bail!("feeds need a name");
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_feeds WHERE agent_id = $1")
            .bind(new.agent_id)
            .fetch_one(self.pool())
            .await?;
        if count >= FEEDS_PER_AGENT {
            bail!("an agent can watch at most {FEEDS_PER_AGENT} feeds; remove one first");
        }
        let id = Uuid::new_v4();
        let (url, token, sealed) = match new.kind {
            "rss" => (Some(check_url(new.url.unwrap_or(""))?.to_string()), None, None),
            _ => (None, Some(new_token()), Some(crate::feed_auth::seal_new_secret(id)?)),
        };
        let allowed_ips = crate::feed_auth::clean_ranges(&new.allowed_ips)?;
        if new.by_address {
            if new.kind != "webhook" {
                bail!("only webhook feeds can be checked by sender address");
            }
            crate::feed_auth::address_only_ranges(&allowed_ips)?;
        }
        let intake = if new.kind == "webhook" { new.intake.checked()? } else { Intake::default() };
        let (secret_ciphertext, secret_nonce) = sealed.unzip();
        let primed = new.kind == "webhook" || new.primed;
        let sql = format!(
            "INSERT INTO agent_feeds (agent_id, owner_user_id, created_by_agent, kind, name, url, token, format, filter, poll_secs, seen, primed, next_poll_at,
                                      id, secret_ciphertext, secret_nonce, secret_set_at, allowed_ips, auth_mode, intake)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     CASE WHEN $4 = 'rss' THEN NOW() + CASE WHEN $12 THEN make_interval(secs => $10) ELSE INTERVAL '0' END END,
                     $13, $14, $15, CASE WHEN $14 IS NULL THEN NULL ELSE NOW() END, $16, $17, $18)
             RETURNING {FEED_COLUMNS}"
        );
        Ok(sqlx::query_as::<_, AgentFeed>(&sql)
            .bind(new.agent_id)
            .bind(new.owner_user_id)
            .bind(new.created_by_agent)
            .bind(new.kind)
            .bind(name)
            .bind(url)
            .bind(token)
            .bind(new.format)
            .bind(new.filter.map(str::trim).filter(|f| !f.is_empty()))
            .bind(clamp_poll_secs(new.poll_secs as i64))
            .bind(&new.seen[..new.seen.len().min(SEEN_CAP)])
            .bind(primed)
            .bind(id)
            .bind(secret_ciphertext)
            .bind(secret_nonce)
            .bind(&allowed_ips)
            .bind(if new.by_address { "address" } else { "secret" })
            .bind(serde_json::to_value(&intake)?)
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn get_feed(&self, id: Uuid) -> Result<Option<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!("SELECT {FEED_COLUMNS} FROM agent_feeds WHERE id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn feed_by_token(&self, token: &str) -> Result<Option<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!("SELECT {FEED_COLUMNS} FROM agent_feeds WHERE kind = 'webhook' AND token = $1"))
            .bind(token)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn agent_feeds(&self, agent_id: Uuid) -> Result<Vec<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!("SELECT {FEED_COLUMNS} FROM agent_feeds WHERE agent_id = $1 ORDER BY created_at"))
            .bind(agent_id)
            .fetch_all(self.pool())
            .await?)
    }

    pub async fn owner_feeds(&self, owner: Option<Uuid>) -> Result<Vec<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "SELECT {FEED_COLUMNS} FROM agent_feeds f
             WHERE EXISTS (SELECT 1 FROM agent_profiles p WHERE p.id = f.agent_id AND p.owner_user_id IS NOT DISTINCT FROM $1)
             ORDER BY created_at"
        ))
        .bind(owner)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn set_feed_paused(&self, id: Uuid, paused: bool) -> Result<Option<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET paused = $2, updated_at = NOW(),
               next_poll_at = CASE WHEN kind = 'rss' AND NOT $2 THEN NOW() ELSE next_poll_at END
             WHERE id = $1 RETURNING {FEED_COLUMNS}"
        ))
        .bind(id)
        .bind(paused)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn update_feed(&self, id: Uuid, name: Option<&str>, filter: Option<Option<&str>>, poll_secs: Option<i32>, format: Option<&str>) -> Result<Option<AgentFeed>> {
        if let Some(f) = format {
            if !FORMATS.contains(&f) {
                bail!("unknown format {f}; use {}", FORMATS.join(", "));
            }
        }
        let name = name.map(str::trim);
        if name.is_some_and(|n| n.is_empty() || n.chars().count() > 120) {
            bail!("feed names are 1 to 120 characters");
        }
        let filter = filter.map(|f| f.map(str::trim).filter(|f| !f.is_empty()));
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET name = COALESCE($2, name),
               filter = CASE WHEN $3 THEN $4 ELSE filter END,
               poll_secs = COALESCE($5, poll_secs),
               format = CASE WHEN kind = 'webhook' THEN COALESCE($6, format) ELSE format END,
               next_poll_at = CASE WHEN kind = 'rss' AND $5 IS NOT NULL AND NOT paused THEN LEAST(COALESCE(next_poll_at, NOW()), NOW() + make_interval(secs => $5)) ELSE next_poll_at END,
               updated_at = NOW()
             WHERE id = $1 RETURNING {FEED_COLUMNS}"
        ))
        .bind(id)
        .bind(name)
        .bind(filter.is_some())
        .bind(filter.flatten())
        .bind(poll_secs.map(|s| clamp_poll_secs(s as i64)))
        .bind(format)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn delete_feed(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM agent_feeds WHERE id = $1").bind(id).execute(self.pool()).await?.rows_affected() > 0)
    }

    pub async fn next_feed_poll_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT MIN(COALESCE(f.next_poll_at, NOW())) FROM agent_feeds f
             JOIN agent_profiles p ON p.id = f.agent_id
             WHERE f.kind = 'rss' AND NOT f.paused AND p.paused_reason IS NULL",
        )
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn claim_due_feeds(&self, limit: i64) -> Result<Vec<AgentFeed>> {
        Ok(sqlx::query_as::<_, AgentFeed>(&format!(
            "UPDATE agent_feeds SET next_poll_at = NOW() + make_interval(secs => poll_secs), last_polled_at = NOW()
             WHERE id IN (
               SELECT f.id FROM agent_feeds f
               JOIN agent_profiles p ON p.id = f.agent_id
               WHERE f.kind = 'rss' AND NOT f.paused AND p.paused_reason IS NULL
                 AND COALESCE(f.next_poll_at, '-infinity') <= NOW()
               ORDER BY f.next_poll_at NULLS FIRST
               LIMIT $1
               FOR UPDATE OF f SKIP LOCKED
             ) RETURNING {FEED_COLUMNS}"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn record_feed_error(&self, id: Uuid, error: &str) -> Result<()> {
        sqlx::query("UPDATE agent_feeds SET last_error = $2 WHERE id = $1")
            .bind(id)
            .bind(crate::cli_errors::error_text(error))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn record_feed_fetch(&self, id: Uuid, fetched: Fetched) -> Result<usize> {
        let Fetched::Feed { items, etag, last_modified, .. } = fetched else {
            sqlx::query("UPDATE agent_feeds SET last_error = NULL WHERE id = $1").bind(id).execute(self.pool()).await?;
            return Ok(0);
        };
        let mut tx = self.pool().begin().await?;
        let Some(feed) = sqlx::query_as::<_, AgentFeed>(&format!("SELECT {FEED_COLUMNS} FROM agent_feeds WHERE id = $1 FOR UPDATE"))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(0);
        };
        let fresh: Vec<&FeedItem> = items.iter().filter(|i| !feed.seen.contains(&i.id)).collect();
        let mut seen: Vec<String> = fresh.iter().map(|i| i.id.clone()).collect();
        seen.extend(feed.seen.iter().cloned());
        seen.truncate(SEEN_CAP);
        let wanted: Vec<FeedItem> = if feed.primed {
            fresh.into_iter().filter(|i| matches_filter(feed.filter.as_deref(), i)).cloned().collect()
        } else {
            Vec::new()
        };
        sqlx::query(
            "UPDATE agent_feeds SET seen = $2, primed = TRUE, etag = $3, last_modified = $4, last_error = NULL, updated_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .bind(&seen)
        .bind(etag)
        .bind(last_modified)
        .execute(&mut *tx)
        .await?;
        let delivered = wanted.len();
        if delivered > 0 {
            deliver_in(&mut tx, &feed, wanted).await?;
        }
        tx.commit().await?;
        Ok(delivered)
    }

    pub async fn ingest_webhook(&self, feed: &AgentFeed, header: &(dyn Fn(&str) -> Option<String> + Send + Sync), body: &[u8], checked: Checked) -> Result<Intaken> {
        if feed.paused {
            return Ok(Intaken::Skipped("the feed is paused".into()));
        }
        let fields = feed_intake::field_names(body);
        let items = match feed_intake::apply(&feed.intake(), &feed.format, header, body) {
            Outcome::Items(items) => items,
            Outcome::Skipped(why) => {
                self.note_feed_call(feed.id, &fields, None).await?;
                return Ok(Intaken::Skipped(why));
            }
            Outcome::Invalid(why) => {
                self.note_feed_call(feed.id, &fields, Some(&why)).await?;
                return Ok(Intaken::Invalid(why));
            }
        };
        self.note_feed_call(feed.id, &fields, None).await?;
        let items: Vec<FeedItem> = items.into_iter().filter(|i| matches_filter(feed.filter.as_deref(), i)).collect();
        if items.is_empty() {
            return Ok(Intaken::Skipped("the feed's keyword filter left nothing".into()));
        }
        let n = items.len();
        let mut tx = self.pool().begin().await?;
        deliver_from(&mut tx, feed, items, Some(checked)).await?;
        tx.commit().await?;
        Ok(Intaken::Delivered(n))
    }
}

async fn deliver_in(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, feed: &AgentFeed, items: Vec<FeedItem>) -> Result<()> {
    deliver_from(tx, feed, items, None).await
}

async fn deliver_from(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, feed: &AgentFeed, mut items: Vec<FeedItem>, checked: Option<Checked>) -> Result<()> {
    let unverified = checked == Some(Checked::Unverified);
    let total = items.len();
    items.truncate(MAX_ITEMS_PER_DELIVERY);
    let more = total - items.len();
    let mut payload = json!({
        "feed_id": feed.id,
        "feed": feed.name,
        "feed_kind": feed.kind,
        "source": feed.source(),
        "owner_user_id": feed.owner_user_id,
        "items": items,
        "more": more,
    });
    if unverified {
        payload["unverified"] = json!(true);
    }
    if let Some(checked) = checked {
        payload["checked"] = json!(checked.label());
    }
    let seq = emit_in(tx, &NewEvent::new(FEED_EVENT).agent(feed.agent_id).payload(payload.clone()), None).await?;
    sqlx::query("UPDATE agent_feeds SET item_count = item_count + $2, last_item_at = NOW() WHERE id = $1")
        .bind(feed.id)
        .bind(total as i64)
        .execute(&mut **tx)
        .await?;
    let pending: Option<(i64, Value)> = sqlx::query_as(
        "SELECT id, payload FROM agent_inbox
         WHERE agent_id = $1 AND kind = $2 AND status = 'pending' AND payload->>'feed_id' = $3
         ORDER BY id DESC LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .bind(feed.agent_id)
    .bind(FEED_EVENT)
    .bind(feed.id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((inbox_id, mut existing)) = pending {
        let mut merged: Vec<Value> = existing.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        merged.extend(items.iter().map(|i| json!(i)));
        let mut dropped = existing.get("more").and_then(Value::as_u64).unwrap_or(0) as usize + more;
        if merged.len() > INBOX_ITEMS_CAP {
            dropped += merged.len() - INBOX_ITEMS_CAP;
            merged.drain(..merged.len() - INBOX_ITEMS_CAP);
        }
        if let Some(fields) = existing.as_object_mut() {
            fields.insert("items".into(), Value::Array(merged));
            fields.insert("more".into(), json!(dropped));
            if unverified {
                fields.insert("unverified".into(), json!(true));
            }
            if let Some(checked) = checked {
                let weakest = match (fields.get("checked").and_then(Value::as_str), checked) {
                    (Some("unverified"), _) | (_, Checked::Unverified) => Checked::Unverified,
                    (Some("address"), _) | (_, Checked::Address) => Checked::Address,
                    _ => Checked::Secret,
                };
                fields.insert("checked".into(), json!(weakest.label()));
            }
        }
        sqlx::query("UPDATE agent_inbox SET payload = $2, event_seq = $3 WHERE id = $1")
            .bind(inbox_id)
            .bind(existing)
            .bind(seq)
            .execute(&mut **tx)
            .await?;
    } else {
        sqlx::query("INSERT INTO agent_inbox (agent_id, kind, event_seq, payload) VALUES ($1, $2, $3, $4)")
            .bind(feed.agent_id)
            .bind(FEED_EVENT)
            .bind(seq)
            .bind(payload)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn poll_due(store: &WorldStore) -> Result<usize> {
    let mut total = 0;
    loop {
        let due = store.claim_due_feeds(8).await?;
        if due.is_empty() {
            return Ok(total);
        }
        let results = futures_util::future::join_all(due.iter().map(|f| fetch(f.url.as_deref().unwrap_or(""), f.etag.as_deref(), f.last_modified.as_deref()))).await;
        for (feed, result) in due.iter().zip(results) {
            match result {
                Ok(fetched) => match store.record_feed_fetch(feed.id, fetched).await {
                    Ok(n) => {
                        total += n;
                        if n > 0 {
                            tracing::info!(feed = %feed.name, agent = %feed.agent_id, items = n, "Feed delivered new items");
                        }
                    }
                    Err(e) => tracing::error!(feed = %feed.name, error = %e, "Could not record feed items"),
                },
                Err(e) => {
                    tracing::warn!(feed = %feed.name, error = %e, "Feed poll failed");
                    store.record_feed_error(feed.id, &e.to_string()).await?;
                }
            }
        }
    }
}

pub async fn poll_loop(store: WorldStore, wake: Arc<tokio::sync::Notify>) {
    const MAX_SLEEP: Duration = Duration::from_secs(3600);
    loop {
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Err(e) = poll_due(&store).await {
            tracing::error!(error = %e, "Feed poll pass failed");
        }
        let wait = match store.next_feed_poll_at().await {
            Ok(Some(at)) => (at - Utc::now()).to_std().unwrap_or(Duration::ZERO).min(MAX_SLEEP),
            Ok(None) => MAX_SLEEP,
            Err(e) => {
                tracing::error!(error = %e, "Could not read the next feed poll time");
                Duration::from_secs(30)
            }
        };
        let _ = tokio::time::timeout(wait.max(Duration::from_secs(1)), notified).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>CVE feed</title>
      <item><title>CVE-2026-0001 critical RCE</title><link>https://x.test/1</link><guid>cve-1</guid><description>&lt;p&gt;Remote &amp;amp; bad&lt;/p&gt;</description><pubDate>Mon, 26 Oct 2026 10:00:00 GMT</pubDate></item>
      <item><title>CVE-2026-0002 low wordpress</title><link>https://x.test/2</link><guid>cve-2</guid><pubDate>Tue, 27 Oct 2026 10:00:00 GMT</pubDate></item>
    </channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?><feed xmlns="http://www.w3.org/2005/Atom"><title>Atom</title><id>urn:a</id><updated>2026-10-30T00:00:00Z</updated>
      <entry><title>Hello</title><id>urn:a:1</id><link href="https://a.test/1"/><updated>2026-10-30T00:00:00Z</updated><summary>First</summary></entry></feed>"#;

    fn alertmanager() -> Value {
        json!({
            "version": "4", "receiver": "world", "status": "firing", "groupKey": "{}:{alertname=\"HighLatency\"}",
            "groupLabels": { "alertname": "HighLatency" },
            "commonLabels": { "alertname": "HighLatency", "severity": "critical" },
            "externalURL": "https://am.test",
            "alerts": [
                { "status": "firing", "labels": { "alertname": "HighLatency", "instance": "api-1" }, "annotations": { "summary": "p99 over 2s" }, "startsAt": "2026-10-31T10:00:00Z", "fingerprint": "a1", "generatorURL": "https://prom.test/g" },
                { "status": "firing", "labels": { "alertname": "HighLatency", "instance": "api-2" }, "annotations": { "description": "slow" }, "startsAt": "2026-10-31T10:01:00Z", "fingerprint": "a2" },
                { "status": "resolved", "labels": { "alertname": "HighLatency", "instance": "api-3" }, "annotations": {}, "endsAt": "2026-10-31T10:02:00Z", "fingerprint": "a3" }
            ]
        })
    }

    #[test]
    fn parses_rss_and_atom() {
        let (title, items) = parse_feed(RSS.as_bytes()).unwrap();
        assert_eq!(title.as_deref(), Some("CVE feed"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "cve-2");
        assert_eq!(items[1].summary, "Remote & bad");
        assert_eq!(items[1].link.as_deref(), Some("https://x.test/1"));
        let (_, atom) = parse_feed(ATOM.as_bytes()).unwrap();
        assert_eq!(atom[0].id, "urn:a:1");
        assert_eq!(atom[0].summary, "First");
        assert!(parse_feed(b"<html><body>nope</body></html>").is_err());
    }

    #[test]
    fn filters_include_and_exclude() {
        let (_, items) = parse_feed(RSS.as_bytes()).unwrap();
        let kept = |f: &str| items.iter().filter(|i| matches_filter(Some(f), i)).count();
        assert_eq!(kept(""), 2);
        assert_eq!(kept("critical"), 1);
        assert_eq!(kept("CVE, -wordpress"), 1);
        assert_eq!(kept("-wordpress"), 1);
        assert_eq!(kept("kernel"), 0);
    }

    #[test]
    fn groups_alertmanager_by_status() {
        let items = alertmanager_items(&alertmanager());
        assert_eq!(items.len(), 2);
        assert!(items[0].title.starts_with("[FIRING:2] HighLatency"));
        assert!(items[0].summary.contains("instance=api-1"));
        assert!(items[0].summary.contains("p99 over 2s"));
        assert!(items[0].summary.contains("https://prom.test/g"));
        assert!(items[1].title.starts_with("[RESOLVED:1]"));
        assert_eq!(items[0].link.as_deref(), Some("https://am.test"));
        let body = serde_json::to_vec(&alertmanager()).unwrap();
        assert_eq!(webhook_items("auto", &body).len(), 2);
    }

    #[test]
    fn webhook_fallbacks() {
        let one = webhook_items("auto", br#"{"title":"deploy done","url":"https://ci.test/1"}"#);
        assert_eq!(one[0].title, "deploy done");
        assert_eq!(one[0].link.as_deref(), Some("https://ci.test/1"));
        assert_eq!(webhook_items("auto", br#"[{"name":"a"},{"name":"b"}]"#).len(), 2);
        let text = webhook_items("auto", b"disk full\non db-1");
        assert_eq!(text[0].title, "disk full");
        assert_eq!(text[0].summary, "on db-1");
        assert!(webhook_items("auto", b"  ").is_empty());
        assert_eq!(webhook_items("json", b"not json")[0].title, "not json");
    }

    #[test]
    fn refuses_private_addresses() {
        assert!(check_url("https://nvd.nist.gov/feeds/xml/cve/misc/nvd-rss.xml").is_ok());
        for bad in ["http://127.0.0.1/x", "http://169.254.169.254/latest", "http://10.0.0.1/", "http://[::1]/", "http://localhost:9093/", "ftp://x.test/", "file:///etc/passwd"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
        assert!(!public_ip("100.64.1.1".parse().unwrap()));
        assert!(!public_ip("::ffff:192.168.0.1".parse().unwrap()));
        assert!(public_ip("1.1.1.1".parse().unwrap()));
        for blocked in ["::127.0.0.1", "64:ff9b::a9fe:a9fe", "2002:a00:1::1", "192.0.0.8", "240.0.0.1", "255.255.255.254"] {
            assert!(!public_ip(blocked.parse().unwrap()), "{blocked}");
        }
        assert!(public_ip("2606:4700::1111".parse().unwrap()));
        assert!(public_ip("64:ff9b::101:101".parse().unwrap()));
    }

    async fn inbox(pool: &sqlx::PgPool, agent: Uuid) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'feed.item' ORDER BY id")
            .bind(agent)
            .fetch_all(pool)
            .await?)
    }

    fn fetched(ids: &[&str]) -> Fetched {
        Fetched::Feed {
            title: None,
            items: ids.iter().map(|i| FeedItem { id: i.to_string(), title: format!("title {i}"), link: None, summary: String::new(), published: None }).collect(),
            etag: Some("\"v1\"".into()),
            last_modified: None,
        }
    }

    #[tokio::test]
    async fn feeds_dedupe_batch_and_route() -> Result<()> {
        let Ok(url) = std::env::var("FEEDS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        crate::environment::initialize_for_tests();
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut users = Vec::new();
        for who in ["a", "b"] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
                .bind(format!("{who}-{tag}@test.dev"))
                .fetch_one(&pool)
                .await?;
            users.push(id);
        }
        let mut agents = Vec::new();
        for (name, owner) in [("watcher", users[0]), ("friend", users[0]), ("stranger", users[1])] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id",
            )
            .bind(format!("{name}-{tag}"))
            .bind(name)
            .bind(owner)
            .fetch_one(&pool)
            .await?;
            agents.push(id);
        }
        let feed_event = vec![FEED_EVENT.to_string()];
        store.set_subscriptions(agents[1], &feed_event, &[]).await?;
        store.set_subscriptions(agents[2], &feed_event, &[]).await?;

        let rss = store
            .create_feed(NewFeed {
                agent_id: agents[0],
                owner_user_id: Some(users[0]),
                created_by_agent: Some(agents[0]),
                kind: "rss",
                name: "CVEs",
                url: Some("https://feeds.test/cve.xml"),
                format: "auto",
                filter: Some("-skip"),
                poll_secs: 600,
                seen: Vec::new(),
                primed: false,
                allowed_ips: Vec::new(),
                by_address: false,
                intake: Intake::default(),
            })
            .await?;
        assert!(!rss.primed);
        let claimed = store.claim_due_feeds(50).await?;
        assert!(claimed.iter().any(|f| f.id == rss.id));
        assert!(!store.claim_due_feeds(50).await?.iter().any(|f| f.id == rss.id));
        assert!(store.next_feed_poll_at().await?.is_some());

        assert_eq!(store.record_feed_fetch(rss.id, fetched(&["1", "2"])).await?, 0);
        assert!(inbox(&pool, agents[0]).await?.is_empty());

        assert_eq!(store.record_feed_fetch(rss.id, fetched(&["3", "skip", "1", "2"])).await?, 1);
        assert_eq!(store.record_feed_fetch(rss.id, fetched(&["4", "3", "1"])).await?, 1);
        assert_eq!(store.record_feed_fetch(rss.id, fetched(&["4", "3"])).await?, 0);
        let rows = inbox(&pool, agents[0]).await?;
        assert_eq!(rows.len(), 1, "a burst is merged into one pending inbox item");
        let ids: Vec<&str> = rows[0]["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["3", "4"]);
        assert_eq!(rows[0]["from_owner"], json!(false));
        let after = store.get_feed(rss.id).await?.unwrap();
        assert_eq!(after.item_count, 2);
        assert!(after.seen.contains(&"skip".to_string()));
        assert_eq!(after.etag.as_deref(), Some("\"v1\""));

        assert_eq!(inbox(&pool, agents[1]).await?.len(), 2, "same-owner subscribers hear about it");
        assert!(inbox(&pool, agents[2]).await?.is_empty(), "other owners never do");

        sqlx::query("UPDATE agent_inbox SET status = 'done' WHERE agent_id = $1").bind(agents[0]).execute(&pool).await?;
        let hook = store
            .create_feed(NewFeed {
                agent_id: agents[0],
                owner_user_id: Some(users[0]),
                created_by_agent: None,
                kind: "webhook",
                name: "Alertmanager",
                url: None,
                format: "auto",
                filter: None,
                poll_secs: 900,
                seen: Vec::new(),
                primed: false,
                allowed_ips: Vec::new(),
                by_address: false,
                intake: Intake::default(),
            })
            .await?;
        let token = hook.token.clone().unwrap();
        assert_eq!(token.len(), 64);
        let found = store.feed_by_token(&token).await?.unwrap();
        assert_eq!(found.id, hook.id);
        let body = serde_json::to_vec(&alertmanager())?;
        assert_eq!(store.ingest_webhook(&found, &|_| None, &body, Checked::Secret).await?, Intaken::Delivered(2));
        let rows = inbox(&pool, agents[0]).await?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["feed_kind"], json!("webhook"));
        assert_eq!(rows[1]["items"].as_array().unwrap().len(), 2);

        let paused = store.set_feed_paused(hook.id, true).await?.unwrap();
        assert!(matches!(store.ingest_webhook(&paused, &|_| None, &body, Checked::Secret).await?, Intaken::Skipped(_)));
        sqlx::query("UPDATE agent_profiles SET paused_reason = 'test' WHERE id = $1").bind(agents[0]).execute(&pool).await?;
        sqlx::query("UPDATE agent_feeds SET next_poll_at = NOW() - INTERVAL '1 minute' WHERE id = $1").bind(rss.id).execute(&pool).await?;
        assert!(!store.claim_due_feeds(50).await?.iter().any(|f| f.id == rss.id), "paused agents are not polled");
        assert!(store.delete_feed(rss.id).await?);
        sqlx::query("DELETE FROM agent_profiles WHERE id = ANY($1)").bind(&agents).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(&users).execute(&pool).await?;
        Ok(())
    }
}
