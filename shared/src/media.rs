use anyhow::{bail, Result};
use uuid::Uuid;

use crate::world::{Upload, WorldAgent, WorldStore};

pub const MAX_IMAGE: usize = 10 * 1024 * 1024;
pub const MAX_VIDEO: usize = 25 * 1024 * 1024;
pub const MAX_PER_CALL: usize = 8;

pub const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
pub const VIDEO_TYPES: [&str; 3] = ["video/mp4", "video/webm", "video/quicktime"];

pub fn is_media(mime: &str) -> bool {
    IMAGE_TYPES.contains(&mime) || VIDEO_TYPES.contains(&mime)
}

pub fn is_video(mime: &str) -> bool {
    VIDEO_TYPES.contains(&mime)
}

pub fn limit_for(mime: &str) -> usize {
    if is_video(mime) || mime == crate::canvas::MODEL_MIME {
        MAX_VIDEO
    } else {
        MAX_IMAGE
    }
}

pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"glTF") {
        return Some(crate::canvas::MODEL_MIME);
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(if &bytes[8..10] == b"qt" { "video/quicktime" } else { "video/mp4" });
    }
    None
}

pub fn checked(bytes: &[u8]) -> Result<&'static str> {
    let Some(mime) = sniff(bytes) else {
        bail!("only PNG, JPEG, GIF, WebP, MP4, MOV, WebM and GLB files can be shown");
    };
    let limit = limit_for(mime);
    if bytes.len() > limit {
        bail!("the file is larger than {} MB", limit / (1024 * 1024));
    }
    Ok(mime)
}

pub fn file_name(raw: &str, mime: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").split(['?', '#']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| c.is_ascii_alphanumeric() || "._- ".contains(*c)).take(100).collect();
    let clean = clean.trim().trim_start_matches('.');
    if !clean.is_empty() && clean.contains('.') {
        return clean.to_string();
    }
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        _ => "mp4",
    };
    let stem = if clean.is_empty() { if is_video(mime) { "recording" } else { "screenshot" } } else { clean };
    format!("{stem}.{ext}")
}

impl WorldStore {
    pub async fn store_agent_media(&self, agent: &WorldAgent, name: &str, bytes: &[u8]) -> Result<Upload> {
        let Some(owner) = agent.owner_user_id else {
            bail!("only agents that work for someone can show media");
        };
        let mime = checked(bytes)?;
        let name = file_name(name, mime);
        Ok(sqlx::query_as::<_, Upload>(
            "INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by, org_id) VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, mime, name, size",
        )
        .bind(mime)
        .bind(&name)
        .bind(bytes.len() as i32)
        .bind(bytes)
        .bind(owner)
        .bind(agent.org_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn agent_media(&self, agent: &WorldAgent, ids: &[Uuid]) -> Result<Vec<Upload>> {
        let Some(owner) = agent.owner_user_id else {
            return Ok(Vec::new());
        };
        let found: Vec<Upload> = sqlx::query_as::<_, Upload>(
            "SELECT id, mime, name, size FROM world_uploads
             WHERE id = ANY($1) AND uploaded_by = $2 AND org_id IS NOT DISTINCT FROM $3 AND mime = ANY($4)",
        )
        .bind(ids)
        .bind(owner)
        .bind(agent.org_id)
        .bind(IMAGE_TYPES.iter().chain(VIDEO_TYPES.iter()).map(|m| m.to_string()).collect::<Vec<_>>())
        .fetch_all(self.pool())
        .await?;
        Ok(ids.iter().filter_map(|id| found.iter().find(|u| u.id == *id).cloned()).collect())
    }
}

pub async fn fetch(url: &str) -> Result<(String, Vec<u8>)> {
    let mut resp = crate::feeds::get_public(url).await?;
    if resp.content_length().is_some_and(|len| len > MAX_VIDEO as u64) {
        bail!("the file is larger than {} MB", MAX_VIDEO / (1024 * 1024));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > MAX_VIDEO {
            bail!("the file is larger than {} MB", MAX_VIDEO / (1024 * 1024));
        }
        body.extend_from_slice(&chunk);
    }
    let name = resp.url().path().to_string();
    Ok((name, body))
}

pub fn parse_range(header: &str, len: usize) -> Option<(usize, usize)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: usize = suffix.parse().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => (s.parse().ok()?, len - 1),
        (s, e) => (s.parse().ok()?, e.parse::<usize>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_media_and_refuses_the_rest() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxx"), Some("image/png"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(&[0x1a, 0x45, 0xdf, 0xa3, 1]), Some("video/webm"));
        assert_eq!(sniff(b"\0\0\0\x18ftypmp42xxxx"), Some("video/mp4"));
        assert_eq!(sniff(b"\0\0\0\x14ftypqt  xxxx"), Some("video/quicktime"));
        assert_eq!(sniff(b"glTF\x02\0\0\0"), Some(crate::canvas::MODEL_MIME));
        assert_eq!(limit_for(crate::canvas::MODEL_MIME), MAX_VIDEO);
        assert_eq!(sniff(b"<svg xmlns"), None);
        assert!(checked(b"#!/bin/sh\necho hi").is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(file_name("/tmp/loka-browser-abc.png", "image/png"), "loka-browser-abc.png");
        assert_eq!(file_name("https://x.test/a/b?sig=1", "image/jpeg"), "b.jpg");
        assert_eq!(file_name("", "video/webm"), "recording.webm");
        assert_eq!(file_name("../../.env", "image/png"), "env.png");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=2-4", 10), Some((2, 4)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=5-100", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=0-1,3-4", 10), None);
    }
}
