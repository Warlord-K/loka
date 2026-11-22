use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::world::{NewEvent, WorldStore};

pub const MAX_TRACKS: usize = 12;
const ACTIONS: &[&str] = &["play", "queue", "stop", "default"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub by: String,
}

fn is_id(s: &str) -> bool {
    s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn video_id(input: &str) -> Option<String> {
    let s = input.trim();
    if is_id(s) {
        return Some(s.to_string());
    }
    let url = reqwest::Url::parse(s).ok()?;
    let host = url.host_str()?.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let id = match host {
        "youtu.be" => url.path_segments()?.next().map(str::to_string),
        "youtube.com" | "youtube-nocookie.com" => {
            let mut parts = url.path_segments()?;
            match parts.next() {
                Some("watch") => url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned()),
                Some("embed" | "shorts" | "live" | "v") => parts.next().map(str::to_string),
                _ => None,
            }
        }
        _ => None,
    }?;
    is_id(&id).then_some(id)
}

const LOOKUP_SECS: u64 = 12;
const SEARCH_RESULTS: usize = 4;
const BROWSER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const CONSENT_COOKIE: &str = "SOCS=CAI; CONSENT=PENDING+987";
const SEARCH_CLIENT_VERSION: &str = "2.20251120.01.00";
const ONLY_VIDEOS: &str = "EgIQAQ==";

#[derive(Debug, Clone, PartialEq)]
pub enum MusicError {
    Invalid,
    Unreachable(String),
    Missing,
    NotEmbeddable,
    NotFound(String),
}

impl std::fmt::Display for MusicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => write!(f, "give a YouTube link or a search of up to 300 characters"),
            Self::Unreachable(why) => write!(f, "could not reach YouTube ({why})"),
            Self::Missing => write!(f, "that video does not exist or is private"),
            Self::NotEmbeddable => write!(f, "that video does not allow playing outside YouTube"),
            Self::NotFound(query) => write!(f, "found nothing playable on YouTube for \"{query}\""),
        }
    }
}

impl std::error::Error for MusicError {}

fn http() -> Result<reqwest::Client, MusicError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::ACCEPT_LANGUAGE, reqwest::header::HeaderValue::from_static("en-US,en;q=0.9"));
    headers.insert(reqwest::header::COOKIE, reqwest::header::HeaderValue::from_static(CONSENT_COOKIE));
    reqwest::Client::builder()
        .http1_only()
        .timeout(Duration::from_secs(LOOKUP_SECS))
        .connect_timeout(Duration::from_secs(6))
        .user_agent(BROWSER_AGENT)
        .default_headers(headers)
        .build()
        .map_err(|e| unreachable(&e))
}

fn unreachable(error: &reqwest::Error) -> MusicError {
    let why = if error.is_timeout() {
        format!("no answer within {LOOKUP_SECS} seconds")
    } else {
        let mut parts: Vec<String> = Vec::new();
        let mut source = std::error::Error::source(error);
        while let Some(e) = source {
            let text = e.to_string();
            if !parts.iter().any(|p| p.contains(&text)) {
                parts.push(text);
            }
            source = e.source();
        }
        if parts.is_empty() {
            "the request failed".to_string()
        } else {
            parts.join(": ")
        }
    };
    tracing::warn!(url = ?error.url().map(|u| u.as_str()), error = %why, "YouTube lookup failed");
    MusicError::Unreachable(why)
}

fn oembed_refusal(status: reqwest::StatusCode) -> Option<MusicError> {
    match status.as_u16() {
        200..=299 => None,
        401 | 403 => Some(MusicError::NotEmbeddable),
        400 | 404 => Some(MusicError::Missing),
        code => Some(MusicError::Unreachable(format!("YouTube answered {code}"))),
    }
}

async fn oembed(client: &reqwest::Client, id: &str) -> Result<Track, MusicError> {
    let res = client
        .get("https://www.youtube.com/oembed")
        .query(&[("url", format!("https://www.youtube.com/watch?v={id}")), ("format", "json".into())])
        .send()
        .await
        .map_err(|e| unreachable(&e))?;
    if let Some(refusal) = oembed_refusal(res.status()) {
        return Err(refusal);
    }
    let body = json_of(res).await?;
    let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>();
    Ok(Track { id: id.to_string(), title: text("title"), by: text("author_name") })
}

async fn json_of(res: reqwest::Response) -> Result<Value, MusicError> {
    let text = res.text().await.map_err(|e| unreachable(&e))?;
    serde_json::from_str(&text).map_err(|_| MusicError::Unreachable("YouTube sent an unreadable answer".into()))
}

fn rendered_ids(value: &Value, out: &mut Vec<String>) {
    if out.len() >= SEARCH_RESULTS {
        return;
    }
    match value {
        Value::Object(map) => {
            if let Some(id) = map.get("videoRenderer").and_then(|v| v.get("videoId")).and_then(Value::as_str) {
                if is_id(id) && !out.iter().any(|o| o == id) {
                    out.push(id.to_string());
                }
                return;
            }
            for v in map.values() {
                rendered_ids(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                rendered_ids(v, out);
            }
        }
        _ => {}
    }
}

fn scanned_ids(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for part in text.split("\"videoId\":\"").skip(1) {
        let id: String = part.chars().take(11).collect();
        if is_id(&id) && part[id.len()..].starts_with('"') && !ids.contains(&id) {
            ids.push(id);
        }
        if ids.len() >= SEARCH_RESULTS {
            break;
        }
    }
    ids
}

pub fn ids_from_search(body: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    rendered_ids(body, &mut ids);
    ids
}

pub fn ids_from_results_page(html: &str) -> Vec<String> {
    let initial = ["var ytInitialData = ", "window[\"ytInitialData\"] = ", "ytInitialData = "]
        .iter()
        .find_map(|marker| html.find(marker).map(|at| &html[at + marker.len()..]))
        .and_then(|rest| serde_json::Deserializer::from_str(rest).into_iter::<Value>().next())
        .and_then(Result::ok);
    let ids = initial.as_ref().map(ids_from_search).unwrap_or_default();
    if ids.is_empty() {
        scanned_ids(html)
    } else {
        ids
    }
}

async fn search_api(client: &reqwest::Client, query: &str) -> Result<Vec<String>, MusicError> {
    let res = client
        .post("https://www.youtube.com/youtubei/v1/search")
        .query(&[("prettyPrint", "false")])
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            json!({
                "context": { "client": { "clientName": "WEB", "clientVersion": SEARCH_CLIENT_VERSION, "hl": "en", "gl": "US" } },
                "query": query,
                "params": ONLY_VIDEOS,
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|e| unreachable(&e))?;
    if !res.status().is_success() {
        return Err(MusicError::Unreachable(format!("YouTube search answered {}", res.status().as_u16())));
    }
    Ok(ids_from_search(&json_of(res).await?))
}

async fn search_page(client: &reqwest::Client, query: &str) -> Result<Vec<String>, MusicError> {
    let res = client.get("https://www.youtube.com/results").query(&[("search_query", query)]).send().await.map_err(|e| unreachable(&e))?;
    if !res.status().is_success() {
        return Err(MusicError::Unreachable(format!("YouTube search answered {}", res.status().as_u16())));
    }
    let html = res.text().await.map_err(|e| unreachable(&e))?;
    Ok(ids_from_results_page(&html))
}

fn pick_search(api: Result<Vec<String>, MusicError>, page: Option<Result<Vec<String>, MusicError>>) -> Result<Vec<String>, MusicError> {
    match (api, page) {
        (Ok(ids), _) if !ids.is_empty() => Ok(ids),
        (_, Some(Ok(ids))) if !ids.is_empty() => Ok(ids),
        (_, Some(Err(e))) | (Err(e), None) => Err(e),
        _ => Ok(Vec::new()),
    }
}

async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<String>, MusicError> {
    let api = search_api(client, query).await;
    let page = match &api {
        Ok(ids) if !ids.is_empty() => None,
        _ => Some(search_page(client, query).await),
    };
    pick_search(api, page)
}

pub async fn resolve(input: &str) -> Result<Track, MusicError> {
    let input = input.trim();
    if input.is_empty() || input.chars().count() > 300 {
        return Err(MusicError::Invalid);
    }
    let client = http()?;
    if let Some(id) = video_id(input) {
        return oembed(&client, &id).await;
    }
    for id in search(&client, input).await? {
        match oembed(&client, &id).await {
            Ok(track) => return Ok(track),
            Err(e @ MusicError::Unreachable(_)) => return Err(e),
            Err(_) => {}
        }
    }
    Err(MusicError::NotFound(input.to_string()))
}

impl WorldStore {
    pub async fn music_event(&self, owner: Uuid, agent: Option<Uuid>, action: &str, tracks: &[Track]) -> Result<()> {
        if !ACTIONS.contains(&action) {
            bail!("action must be one of {}", ACTIONS.join(", "));
        }
        let mut event = NewEvent::new("music.play").payload(json!({
            "owner_user_id": owner,
            "action": action,
            "tracks": tracks.iter().take(MAX_TRACKS).collect::<Vec<_>>(),
            "quiet": true,
        }));
        if let Some(a) = agent {
            event = event.agent(a);
        }
        self.emit(event, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{http, ids_from_results_page, ids_from_search, oembed_refusal, pick_search, resolve, search_api, search_page, unreachable, video_id, MusicError};
    use serde_json::Value;

    const SEARCH_JSON: &str = include_str!("fixtures/youtube_search.json");
    const RESULTS_HTML: &str = include_str!("fixtures/youtube_results.html");
    const TOP: [&str; 3] = ["aBkTkxKDduc", "y5XXmI7EL-M", "xIQrC4CerB8"];

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn cut_off() -> MusicError {
        MusicError::Unreachable("error sending request".into())
    }

    #[test]
    fn an_empty_search_with_an_unreachable_results_page_says_youtube_could_not_be_reached() {
        assert_eq!(pick_search(Ok(Vec::new()), Some(Err(cut_off()))), Err(cut_off()));
        assert!(pick_search(Ok(Vec::new()), Some(Err(cut_off()))).unwrap_err().to_string().starts_with("could not reach YouTube"));
    }

    #[test]
    fn search_results_win_over_the_results_page_and_the_page_fills_in_after_a_failed_search() {
        assert_eq!(pick_search(Ok(ids(&["a"])), None), Ok(ids(&["a"])));
        assert_eq!(pick_search(Err(cut_off()), Some(Ok(ids(&["b"])))), Ok(ids(&["b"])));
        assert_eq!(pick_search(Ok(Vec::new()), Some(Ok(ids(&["c"])))), Ok(ids(&["c"])));
    }

    #[test]
    fn a_failed_page_reports_its_error_and_a_page_that_found_nothing_stays_empty() {
        let api = MusicError::Unreachable("YouTube search answered 500".into());
        assert_eq!(pick_search(Err(api.clone()), Some(Err(cut_off()))), Err(cut_off()));
        assert_eq!(pick_search(Err(api.clone()), Some(Ok(Vec::new()))), Ok(Vec::new()));
        assert_eq!(pick_search(Ok(Vec::new()), Some(Ok(Vec::new()))), Ok(Vec::new()));
    }

    #[test]
    fn search_answers_give_the_top_videos_without_ads_or_shorts() {
        let body: Value = serde_json::from_str(SEARCH_JSON).unwrap();
        assert_eq!(ids_from_search(&body), TOP);
    }

    #[test]
    fn results_pages_give_the_same_videos_from_their_initial_data() {
        assert_eq!(ids_from_results_page(RESULTS_HTML), TOP);
    }

    #[test]
    fn results_pages_without_initial_data_fall_back_to_scanning() {
        let page = r#"<script>render({"videoId":"aBkTkxKDduc","x":{"videoId":"aBkTkxKDduc"}},{"videoId":"y5XXmI7EL-M"},{"videoId":"tooShort"})</script>"#;
        assert_eq!(ids_from_results_page(page), ["aBkTkxKDduc", "y5XXmI7EL-M"]);
        assert!(ids_from_results_page("<html>consent</html>").is_empty());
    }

    #[test]
    fn oembed_answers_map_to_plain_reasons() {
        let refusal = |code: u16| oembed_refusal(reqwest::StatusCode::from_u16(code).unwrap());
        assert_eq!(refusal(200), None);
        assert_eq!(refusal(401), Some(MusicError::NotEmbeddable));
        assert_eq!(refusal(403), Some(MusicError::NotEmbeddable));
        assert_eq!(refusal(404), Some(MusicError::Missing));
        assert_eq!(refusal(400), Some(MusicError::Missing));
        assert_eq!(refusal(503), Some(MusicError::Unreachable("YouTube answered 503".into())));
    }

    #[tokio::test]
    async fn network_failures_name_their_cause() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = http().unwrap().get(format!("http://127.0.0.1:{port}/results")).send().await.unwrap_err();
        let MusicError::Unreachable(why) = unreachable(&error) else { panic!("expected an unreachable error") };
        assert!(why.to_lowercase().contains("connect"), "{why}");
        let shown = MusicError::Unreachable(why).to_string();
        assert!(shown.starts_with("could not reach YouTube ("), "{shown}");
        assert!(!shown.contains("error sending request"), "{shown}");
    }

    #[tokio::test]
    async fn bad_input_is_refused_before_any_request() {
        assert_eq!(resolve("  ").await, Err(MusicError::Invalid));
        assert_eq!(resolve(&"a".repeat(301)).await, Err(MusicError::Invalid));
    }

    #[tokio::test]
    #[ignore]
    async fn live_searches_and_links_resolve() {
        let track = resolve("C418 Sweden").await.unwrap();
        assert!(!track.title.is_empty(), "{track:?}");
        let client = http().unwrap();
        assert!(!search_api(&client, "Nujabes Aruarian Dance").await.unwrap().is_empty());
        assert!(!search_page(&client, "Nujabes Aruarian Dance").await.unwrap().is_empty());
        let linked = resolve("https://youtu.be/aBkTkxKDduc").await.unwrap();
        assert_eq!(linked.id, "aBkTkxKDduc");
    }

    #[test]
    fn reads_video_ids_from_links() {
        assert_eq!(video_id("es9MQweZzvc").as_deref(), Some("es9MQweZzvc"));
        assert_eq!(video_id("https://www.youtube.com/watch?v=es9MQweZzvc&t=3").as_deref(), Some("es9MQweZzvc"));
        assert_eq!(video_id("https://youtu.be/es9MQweZzvc?si=x").as_deref(), Some("es9MQweZzvc"));
        assert_eq!(video_id("https://music.youtube.com/watch?v=es9MQweZzvc").as_deref(), Some("es9MQweZzvc"));
        assert_eq!(video_id("https://www.youtube.com/embed/es9MQweZzvc").as_deref(), Some("es9MQweZzvc"));
        assert_eq!(video_id("frieren time flows ever onward"), None);
        assert_eq!(video_id("https://example.com/watch?v=es9MQweZzvc"), None);
    }
}
