use anyhow::{anyhow, bail, Result};
use loka_shared::music::{self, MusicError, Track, MAX_TRACKS};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};

pub const MUSIC_TOOLS: &[&str] = &["play_music", "stop_music"];

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    vec![
        tool(
            "play_music",
            "Play background music in your person's Loka through YouTube. songs are YouTube links, video ids or plain searches like \"Joe Hisaishi One Summer's Day\" (each search plays the top playable result). By default the first song starts now and the rest follow; queue true adds them after what is playing. Their own default playlist resumes once yours ends. Only when your person asks for music or clearly wants a change of mood.",
            json!({
                "songs": { "type": "array", "items": { "type": "string" }, "description": "Up to 12 YouTube links, ids or searches, in play order" },
                "queue": { "type": "boolean", "description": "true adds after the current song instead of playing now" }
            }),
            &["songs"],
        ),
        tool(
            "stop_music",
            "Stop the background music in your person's Loka, or with default true go back to their default playlist.",
            json!({ "default": { "type": "boolean", "description": "true resumes the default playlist instead of stopping" } }),
            &[],
        ),
    ]
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, name: &str, args: &Value) -> Result<String> {
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("you have no person to play music for"))?;
    match name {
        "play_music" => {
            let songs: Vec<&str> = args.get("songs").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
            if songs.is_empty() {
                bail!("give at least one YouTube link or search in songs");
            }
            let total = songs.len().min(MAX_TRACKS);
            let mut tracks: Vec<Track> = Vec::new();
            let mut missed = Vec::new();
            let mut cut_off: Option<MusicError> = None;
            for song in songs.into_iter().take(MAX_TRACKS) {
                match music::resolve(song).await {
                    Ok(t) => tracks.push(t),
                    Err(e @ MusicError::Unreachable(_)) => {
                        cut_off = Some(e);
                        break;
                    }
                    Err(e) => missed.push(format!("{song}: {e}")),
                }
            }
            let untried = total - tracks.len() - missed.len();
            if let Some(e) = &cut_off {
                missed.push(format!("Loka {e}, so {untried} song(s) were not looked up. Try again in a few minutes"));
            }
            if tracks.is_empty() {
                bail!("nothing played. {}", missed.join("; "));
            }
            let queue = args.get("queue").and_then(Value::as_bool).unwrap_or(false);
            store.music_event(owner, Some(caller.id), if queue { "queue" } else { "play" }, &tracks).await?;
            let list = tracks.iter().map(|t| format!("- {} ({})", t.title, t.by)).collect::<Vec<_>>().join("\n");
            let tail = if missed.is_empty() { String::new() } else { format!("\nSkipped: {}", missed.join("; ")) };
            Ok(format!("{} in Loka (it plays if their music is on):\n{list}{tail}", if queue { "Queued" } else { "Playing" }))
        }
        "stop_music" => {
            let back = args.get("default").and_then(Value::as_bool).unwrap_or(false);
            store.music_event(owner, Some(caller.id), if back { "default" } else { "stop" }, &[]).await?;
            Ok(if back { "Back to the default playlist." } else { "Music stopped." }.into())
        }
        _ => bail!("unknown music tool {name}"),
    }
}
