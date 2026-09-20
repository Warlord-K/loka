use std::time::Duration;

use anyhow::{anyhow, bail, Result};
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

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15")
        .build()?)
}

async fn oembed(client: &reqwest::Client, id: &str) -> Result<Track> {
    let res = client
        .get("https://www.youtube.com/oembed")
        .query(&[("url", format!("https://www.youtube.com/watch?v={id}")), ("format", "json".into())])
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("that video does not exist or does not allow embedding");
    }
    let body: Value = serde_json::from_str(&res.text().await?)?;
    let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>();
    Ok(Track { id: id.to_string(), title: text("title"), by: text("author_name") })
}

async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<String>> {
    let html = client
        .get("https://www.youtube.com/results")
        .query(&[("search_query", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let mut ids: Vec<String> = Vec::new();
    for part in html.split("\"videoId\":\"").skip(1) {
        let id: String = part.chars().take(11).collect();
        if is_id(&id) && !ids.contains(&id) {
            ids.push(id);
        }
        if ids.len() >= 4 {
            break;
        }
    }
    Ok(ids)
}

pub async fn resolve(input: &str) -> Result<Track> {
    let input = input.trim();
    if input.is_empty() || input.chars().count() > 300 {
        bail!("give a YouTube link or a search of up to 300 characters");
    }
    let client = http()?;
    if let Some(id) = video_id(input) {
        return oembed(&client, &id).await;
    }
    for id in search(&client, input).await? {
        if let Ok(track) = oembed(&client, &id).await {
            return Ok(track);
        }
    }
    Err(anyhow!("found nothing playable on YouTube for \"{input}\""))
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
    use super::video_id;

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
