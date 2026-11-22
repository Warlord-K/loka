use anyhow::{bail, Result};
use uuid::Uuid;

use crate::world::{Upload, WorldAgent, WorldStore};

pub const MAX_IMAGE: usize = 10 * 1024 * 1024;
pub const MAX_VIDEO: usize = 25 * 1024 * 1024;
pub const MAX_PER_CALL: usize = 8;
pub const MAX_ATTACHED_FILE: usize = 10 * 1024 * 1024;
pub const MAX_ATTACHED_TOTAL: usize = 24 * 1024 * 1024;
pub const MAX_ATTACHED_FILES: usize = 8;

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

pub fn attachment_extension(mime: &str) -> &'static str {
    match mime {
        "audio/x-m4a" | "audio/m4a" | "audio/mp4" | "audio/x-mp4" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/webm" => "webm",
        "audio/aac" => "aac",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        _ => "bin",
    }
}

pub fn attachment_file_name(id: Uuid, raw: &str, mime: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| c.is_ascii_alphanumeric() || "._- ".contains(*c)).take(64).collect();
    let clean = clean.trim().trim_start_matches('.').trim_end_matches('.');
    let named = match clean.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => clean.to_string(),
        _ if clean.is_empty() => format!("attachment.{}", attachment_extension(mime)),
        _ => format!("{clean}.{}", attachment_extension(mime)),
    };
    format!("{id}-{named}")
}

pub fn fits_caps(total: &mut usize, size: usize) -> bool {
    if size > MAX_ATTACHED_FILE || *total + size > MAX_ATTACHED_TOTAL {
        return false;
    }
    *total += size;
    true
}

pub fn attached_files_note(saved: &[(String, String)], skipped: &[String]) -> Option<String> {
    if saved.is_empty() && skipped.is_empty() {
        return None;
    }
    let mut note = String::new();
    if !saved.is_empty() {
        note.push_str("[attached files saved on disk] Your own tools cannot open them. A quest session can: hand the work to a quest and give it the upload id, which is the first 36 characters of each file name below, to fetch with get_upload.");
        for (path, mime) in saved {
            note.push_str(&format!("\n- {path} ({mime})"));
        }
    }
    for name in skipped {
        note.push_str(&format!("\n(attached file {name} was too large to save here, over {} MB)", MAX_ATTACHED_FILE / (1024 * 1024)));
    }
    Some(note.trim_start().to_string())
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
    fn attachment_names_keep_the_name_and_stay_inside_their_folder() {
        let id = Uuid::parse_str("7cee963f-0000-4000-8000-000000000000").unwrap();
        let prefix = "7cee963f-0000-4000-8000-000000000000-";
        assert_eq!(attachment_file_name(id, "New Recording.m4a", "audio/x-m4a"), format!("{prefix}New Recording.m4a"));
        assert_eq!(attachment_file_name(id, "New Recording", "audio/x-m4a"), format!("{prefix}New Recording.m4a"));
        assert_eq!(attachment_file_name(id, "", "audio/mpeg"), format!("{prefix}attachment.mp3"));
        assert_eq!(attachment_file_name(id, "", "application/x-unknown"), format!("{prefix}attachment.bin"));
        assert_eq!(attachment_file_name(id, "../../etc/passwd", "audio/wav"), format!("{prefix}passwd.wav"));
        assert_eq!(attachment_file_name(id, "..\\..\\evil.sh", "audio/wav"), format!("{prefix}evil.sh"));
        assert_eq!(attachment_file_name(id, "/abs/olute/clip.mov", "video/quicktime"), format!("{prefix}clip.mov"));
        assert_eq!(attachment_file_name(id, ".hidden.mp3", "audio/mpeg"), format!("{prefix}hidden.mp3"));
        assert_eq!(attachment_file_name(id, "a\0b$(x).m4a", "audio/x-m4a"), format!("{prefix}abx.m4a"));
        assert_eq!(attachment_file_name(id, "weird.toolongextension", "audio/ogg"), format!("{prefix}weird.toolongextension.ogg"));
        for name in ["..", "../..", "a/../..", "...", "\\", "/"] {
            let made = attachment_file_name(id, name, "audio/wav");
            assert!(!made.contains('/') && !made.contains('\\') && made.starts_with(prefix), "{name} gave {made}");
            assert!(!std::path::Path::new(&made).components().any(|c| matches!(c, std::path::Component::ParentDir)));
        }
        assert!(attachment_file_name(id, &"a".repeat(500), "audio/wav").len() <= 120);
        assert!(attachment_file_name(id, &format!("{}.m4a", "a".repeat(500)), "audio/x-m4a").len() <= 120);
    }

    #[test]
    fn caps_count_each_file_and_the_total() {
        let mut total = 0;
        assert!(fits_caps(&mut total, MAX_ATTACHED_FILE));
        assert!(fits_caps(&mut total, MAX_ATTACHED_FILE));
        assert!(!fits_caps(&mut total, MAX_ATTACHED_FILE + 1));
        assert!(!fits_caps(&mut total, MAX_ATTACHED_FILE));
        assert_eq!(total, 2 * MAX_ATTACHED_FILE);
        let rest = MAX_ATTACHED_TOTAL - total;
        assert!(fits_caps(&mut total, rest));
        assert!(!fits_caps(&mut total, 1));
    }

    #[test]
    fn the_file_note_lists_paths_and_what_was_left_out() {
        assert_eq!(attached_files_note(&[], &[]), None);
        let note = attached_files_note(&[("/w/.uploads/a.m4a".into(), "audio/x-m4a".into())], &["big.mov".into()]).unwrap();
        assert!(note.starts_with("[attached files saved on disk]"));
        assert!(note.contains("hand the work to a quest") && note.contains("get_upload"));
        assert!(note.contains("- /w/.uploads/a.m4a (audio/x-m4a)"));
        assert!(note.contains("big.mov was too large"));
        assert!(attached_files_note(&[], &["big.mov".into()]).unwrap().starts_with("(attached file big.mov"));
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
