use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use loka_shared::github_feed::{self, GithubNotification, BATCH_MAX};
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::{ensure_success, send_json, RunnerApi};

pub(super) const UNKNOWN: u8 = 0;
pub(super) const OFF: u8 = 1;
pub(super) const ON: u8 = 2;

const STATE_FILE: &str = "github-feed.json";
const API: &str = "https://api.github.com";
const MIN_INTERVAL: u64 = 60;
const IDLE_CHECK: Duration = Duration::from_secs(15);
const TOKEN_RECHECK: Duration = Duration::from_secs(300);
const MAX_PAGES: usize = 5;
const SNIPPETS_PER_POLL: usize = 6;
const SINCE_OVERLAP: i64 = 120;

static STATUS: Mutex<Option<Value>> = Mutex::new(None);

pub(super) fn supported() -> bool {
    cfg!(target_os = "macos") && !matches!(std::env::var("WORLD_GITHUB_FEED").as_deref(), Ok("off" | "false" | "0"))
}

pub(super) fn capability() -> Value {
    if !supported() {
        return Value::Null;
    }
    STATUS.lock().ok().and_then(|s| s.clone()).unwrap_or(Value::Null)
}

fn set_status(available: bool, login: Option<&str>, problem: Option<&str>) {
    if let Ok(mut status) = STATUS.lock() {
        *status = Some(json!({
            "available": available,
            "login": login,
            "problem": problem,
            "checked_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }));
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FeedState {
    #[serde(default)]
    since: Option<DateTime<Utc>>,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    primed: bool,
}

fn load(root: &Path) -> FeedState {
    std::fs::read(root.join(STATE_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save(root: &Path, state: &FeedState) {
    let path = root.join(STATE_FILE);
    let tmp = root.join(format!("{STATE_FILE}.tmp"));
    let written = serde_json::to_vec(state).map_err(anyhow::Error::from).and_then(|bytes| {
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    });
    if let Err(error) = written {
        warn!(error = %format!("{error:#}"), "Could not save the GitHub feed state");
    }
}

fn token() -> Option<String> {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    crate::claude::sandbox::github_token(&BTreeMap::new())
}

async fn host_token() -> Option<String> {
    tokio::task::spawn_blocking(token).await.ok().flatten()
}

#[derive(Debug)]
enum PollError {
    Auth(String),
    Wait(Duration, String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for PollError {
    fn from(error: anyhow::Error) -> Self {
        PollError::Other(error)
    }
}

impl From<reqwest::Error> for PollError {
    fn from(error: reqwest::Error) -> Self {
        PollError::Other(error.into())
    }
}

#[derive(Deserialize, Default)]
struct Reply {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    primed: bool,
    #[serde(default)]
    created: usize,
}

impl RunnerApi {
    async fn post_github(&self, notifications: &[GithubNotification]) -> Result<Option<Reply>> {
        let response = send_json(|| self.authorized(self.client.post(self.runner_url("/github"))), &json!({ "notifications": notifications }))
            .await
            .context("GitHub feed upload failed")?;
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) {
            return Ok(None);
        }
        ensure_success(response.status(), "GitHub feed upload")?;
        Ok(Some(response.json().await.unwrap_or_default()))
    }
}

pub(super) fn spawn(api: RunnerApi, root: PathBuf, switch: Arc<AtomicU8>) {
    if !supported() {
        return;
    }
    tokio::spawn(run(api, root, switch));
}

async fn run(api: RunnerApi, root: PathBuf, switch: Arc<AtomicU8>) {
    let client = match Client::builder().user_agent("loka-runner").timeout(Duration::from_secs(30)).build() {
        Ok(client) => client,
        Err(error) => {
            warn!(error = %error, "GitHub feed could not start");
            return;
        }
    };
    let mut state = load(&root);
    let mut was_on = false;
    let mut login: Option<(String, String)> = None;
    let mut checked: Option<Instant> = None;
    let mut wait = Duration::from_secs(5);
    let mut failures: u32 = 0;
    loop {
        tokio::time::sleep(wait).await;
        let on = switch.load(Ordering::Relaxed) == ON;
        if !on {
            if was_on && switch.load(Ordering::Relaxed) == OFF {
                state = FeedState::default();
                save(&root, &state);
                was_on = false;
                info!("GitHub feed turned off");
            }
            if checked.is_none_or(|at| at.elapsed() >= TOKEN_RECHECK) {
                checked = Some(Instant::now());
                let token = host_token().await;
                let known = login.as_ref().filter(|(t, _)| token.as_deref() == Some(t.as_str())).map(|(_, l)| l.as_str());
                match &token {
                    Some(_) => set_status(true, known, None),
                    None => set_status(false, None, Some("gh is not signed in on this Mac")),
                }
            }
            wait = IDLE_CHECK;
            continue;
        }
        if !was_on {
            info!("GitHub feed turned on");
            was_on = true;
        }
        let Some(token) = host_token().await else {
            set_status(false, None, Some("gh is not signed in on this Mac"));
            wait = TOKEN_RECHECK;
            continue;
        };
        if login.as_ref().is_none_or(|(t, _)| *t != token) {
            match whoami(&client, &token).await {
                Ok(name) => login = Some((token.clone(), name)),
                Err(error) => warn!(error = %format!("{error:#}"), "Could not read the GitHub login"),
            }
        }
        let name = login.as_ref().filter(|(t, _)| *t == token).map(|(_, l)| l.clone());
        match poll(&client, &api, &token, &mut state).await {
            Ok(interval) => {
                failures = 0;
                set_status(true, name.as_deref(), None);
                wait = Duration::from_secs(interval.max(MIN_INTERVAL));
            }
            Err(PollError::Auth(why)) => {
                set_status(true, name.as_deref(), Some(&why));
                warn!(problem = %why, "GitHub feed cannot read notifications");
                wait = TOKEN_RECHECK;
            }
            Err(PollError::Wait(pause, why)) => {
                info!(secs = pause.as_secs(), reason = %why, "GitHub feed backing off");
                wait = pause.max(Duration::from_secs(MIN_INTERVAL));
            }
            Err(PollError::Other(error)) => {
                failures += 1;
                let pause = Duration::from_secs((MIN_INTERVAL << failures.min(4)).min(900));
                warn!(error = %format!("{error:#}"), failures, retry_secs = pause.as_secs(), "GitHub feed poll failed");
                wait = pause;
            }
        }
        save(&root, &state);
    }
}

fn github(client: &Client, url: &str, token: &str) -> reqwest::RequestBuilder {
    client
        .get(url)
        .bearer_auth(token)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
}

async fn whoami(client: &Client, token: &str) -> Result<String> {
    let response = github(client, &format!("{API}/user"), token).send().await?;
    ensure_success(response.status(), "GitHub user lookup")?;
    let body: Value = response.json().await?;
    body.get("login").and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("GitHub returned no login"))
}

fn header_text(response: &reqwest::Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn limited(response: &reqwest::Response) -> Option<Duration> {
    if let Some(secs) = header_text(response, "retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(Duration::from_secs(secs));
    }
    if header_text(response, "x-ratelimit-remaining").as_deref() == Some("0") {
        let reset = header_text(response, "x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let secs = (reset - Utc::now().timestamp()).clamp(60, 3600);
        return Some(Duration::from_secs(secs as u64));
    }
    None
}

pub(super) fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| url.trim().trim_start_matches('<').trim_end_matches('>').to_string())
    })
    .filter(|url| url.starts_with(API))
}

pub(super) fn feed_url(since: Option<DateTime<Utc>>) -> String {
    let mut url = format!("{API}/notifications?all=false&per_page=50");
    if let Some(since) = since {
        url.push_str(&format!("&since={}", since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
    }
    url
}

async fn poll(client: &Client, api: &RunnerApi, token: &str, state: &mut FeedState) -> Result<u64, PollError> {
    let started = Utc::now();
    let mut request = github(client, &feed_url(state.since), token);
    if let Some(modified) = &state.last_modified {
        request = request.header(header::IF_MODIFIED_SINCE, modified);
    }
    let response = request.send().await?;
    let interval = header_text(&response, "x-poll-interval").and_then(|v| v.parse::<u64>().ok()).unwrap_or(MIN_INTERVAL);
    match response.status() {
        StatusCode::NOT_MODIFIED if state.primed => return Ok(interval),
        StatusCode::UNAUTHORIZED => return Err(PollError::Auth("GitHub refused the gh login on this Mac".into())),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            return Err(match limited(&response) {
                Some(pause) => PollError::Wait(pause, "rate limited".into()),
                None => PollError::Auth("the gh login on this Mac cannot read notifications (it needs the notifications or repo scope)".into()),
            })
        }
        status if status.is_server_error() => return Err(PollError::Wait(Duration::from_secs(300), format!("GitHub returned {status}"))),
        StatusCode::NOT_MODIFIED => {}
        status => ensure_success(status, "GitHub notifications")?,
    }
    let modified = header_text(&response, "last-modified");
    let mut raw: Vec<Value> = Vec::new();
    if response.status() != StatusCode::NOT_MODIFIED {
        let mut next = header_text(&response, "link").and_then(|l| next_link(&l));
        raw.extend(response.json::<Vec<Value>>().await.context("GitHub sent unreadable notifications")?);
        let pages = if state.since.is_some() { MAX_PAGES } else { 1 };
        for _ in 1..pages {
            let Some(url) = next.take() else { break };
            let page = github(client, &url, token).send().await?;
            if !page.status().is_success() {
                break;
            }
            next = header_text(&page, "link").and_then(|l| next_link(&l));
            raw.extend(page.json::<Vec<Value>>().await.unwrap_or_default());
        }
    }
    let mut items: Vec<GithubNotification> = raw.iter().filter_map(github_feed::from_api).collect();
    if state.primed {
        enrich(client, token, &mut items).await;
    }
    for item in &mut items {
        item.comment_api_url = None;
    }
    if !items.is_empty() || !state.primed {
        let mut chunks: Vec<&[GithubNotification]> = items.chunks(BATCH_MAX).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for chunk in chunks {
            match api.post_github(chunk).await? {
                None => return Err(PollError::Wait(Duration::from_secs(3600), "the Loka server has no GitHub feed yet".into())),
                Some(reply) if !reply.enabled => return Ok(interval),
                Some(reply) => {
                    if reply.primed {
                        info!(items = chunk.len(), "GitHub feed primed, older notifications stay quiet");
                    } else if reply.created > 0 {
                        info!(created = reply.created, "GitHub feed sent new notifications");
                    }
                }
            }
        }
        state.primed = true;
    }
    state.last_modified = modified.or(state.last_modified.take());
    state.since = Some(started - chrono::Duration::seconds(SINCE_OVERLAP));
    Ok(interval)
}

async fn enrich(client: &Client, token: &str, items: &mut [GithubNotification]) {
    let mut budget = SNIPPETS_PER_POLL;
    for item in items.iter_mut() {
        if budget == 0 {
            break;
        }
        let Some(url) = item.comment_api_url.clone() else { continue };
        if !github_feed::wants_snippet(&item.reason) {
            continue;
        }
        budget -= 1;
        let Ok(response) = github(client, &url, token).send().await else { continue };
        if !response.status().is_success() {
            continue;
        }
        let Ok(body) = response.json::<Value>().await else { continue };
        if let Some(actor) = body.get("user").and_then(|u| u.get("login")).and_then(Value::as_str) {
            item.actor = actor.to_string();
        }
        if let Some(text) = body.get("body").and_then(Value::as_str) {
            item.snippet = text.chars().take(700).collect();
        }
        if let Some(link) = body.get("html_url").and_then(Value::as_str).filter(|l| l.starts_with("https://github.com/")) {
            item.url = link.to_string();
        }
        if item.validate().is_err() {
            item.actor.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_only_github_next_links() {
        let link = "<https://api.github.com/notifications?page=2>; rel=\"next\", <https://api.github.com/notifications?page=9>; rel=\"last\"";
        assert_eq!(next_link(link).as_deref(), Some("https://api.github.com/notifications?page=2"));
        assert_eq!(next_link("<https://evil.example/x>; rel=\"next\""), None);
        assert_eq!(next_link("<https://api.github.com/notifications?page=9>; rel=\"last\""), None);
    }

    #[test]
    fn first_poll_has_no_since_and_never_marks_read() {
        assert_eq!(feed_url(None), "https://api.github.com/notifications?all=false&per_page=50");
        let since = DateTime::parse_from_rfc3339("2026-11-14T10:00:00Z").unwrap().with_timezone(&Utc);
        assert!(feed_url(Some(since)).ends_with("&since=2026-11-14T10:00:00Z"));
        assert!(!feed_url(None).contains("read"));
    }

    #[test]
    fn state_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("loka-gh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = FeedState { since: Some(Utc::now()), last_modified: Some("Fri, 14 Nov 2026 10:00:00 GMT".into()), primed: true };
        save(&dir, &state);
        let back = load(&dir);
        assert!(back.primed);
        assert_eq!(back.last_modified, state.last_modified);
        std::fs::remove_dir_all(&dir).ok();
    }
}
