use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use uuid::Uuid;

use super::{gzip, RunnerApi};

const KEEP_FOR: Duration = Duration::from_secs(24 * 3600);

pub(super) fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

const CANVAS_FILE_TOOLS: [&str; 2] = ["create_canvas", "canvas_add_items"];
const MAX_CANVAS_FILES: usize = 24;

fn item_files(request: &Value) -> bool {
    request
        .pointer("/params/arguments/items")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.get("file").and_then(Value::as_str).is_some_and(|f| !f.trim().is_empty())))
}

pub(super) fn wants_files(request: &Value) -> bool {
    if request.get("method").and_then(Value::as_str) != Some("tools/call") {
        return false;
    }
    match request.pointer("/params/name").and_then(Value::as_str) {
        Some("show_media") => request.pointer("/params/arguments/files").and_then(Value::as_array).is_some_and(|files| !files.is_empty()),
        Some(name) => CANVAS_FILE_TOOLS.contains(&name) && item_files(request),
        None => false,
    }
}

pub(super) fn wants_upload(request: &Value) -> bool {
    request.get("method").and_then(Value::as_str) == Some("tools/call") && request.pointer("/params/name").and_then(Value::as_str) == Some("get_upload")
}

pub(super) fn upload_id(request: &Value) -> Result<Uuid, String> {
    let raw = request.pointer("/params/arguments/upload_id").and_then(Value::as_str).unwrap_or("").trim();
    raw.trim_start_matches("upload:").parse().map_err(|_| format!("{raw:?} is not an upload id"))
}

pub(super) fn answer(request: &Value, text: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null), "result": { "content": [{ "type": "text", "text": text }] } })
}

pub(super) async fn save_upload(api: &RunnerApi, lease_id: Uuid, token: &str, roots: &[PathBuf], request: &Value) -> Result<String, String> {
    let id = upload_id(request)?;
    let dir = roots.get(1).ok_or_else(|| "this job has no media folder to save into: get_upload works only in quests on your owner's own device, not on cloud runners".to_string())?;
    let response = api
        .client
        .get(api.lease_url(lease_id, &format!("/uploads/{id}")))
        .header("Authorization", format!("RunnerMcp {token}"))
        .send()
        .await
        .map_err(|e| format!("could not fetch the upload: {e}"))?;
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(answer.get("message").and_then(Value::as_str).unwrap_or("the upload could not be fetched").to_string());
    }
    let text = |key: &str| answer.get(key).and_then(Value::as_str).unwrap_or("");
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text("data_base64")).map_err(|_| "the upload came back unreadable".to_string())?;
    if bytes.len() > loka_shared::media::MAX_VIDEO {
        return Err(format!("the upload is larger than {} MB", loka_shared::media::MAX_VIDEO / (1024 * 1024)));
    }
    let name = loka_shared::media::attachment_file_name(id, text("name"), text("mime"));
    let path = dir.join(&name);
    if path.parent() != Some(dir.as_path()) {
        return Err("the upload name is not usable".into());
    }
    tokio::fs::write(&path, &bytes).await.map_err(|e| format!("could not save the upload: {e}"))?;
    Ok(format!("Saved {} ({}, {} bytes) to {}", text("name"), text("mime"), bytes.len(), path.display()))
}

pub(super) fn refusal(request: &Value, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(Value::Null), "result": {
        "content": [{ "type": "text", "text": message }], "isError": true
    }})
}

pub(super) fn resolve(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let raw = raw.trim();
    let path = Path::new(raw);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        roots.first().ok_or_else(|| "this job has no work folder".to_string())?.join(path)
    };
    let real = candidate.canonicalize().map_err(|_| format!("{raw} does not exist"))?;
    let inside = roots.iter().filter_map(|root| root.canonicalize().ok()).any(|root| real.starts_with(&root));
    if !inside {
        return Err(format!("{raw} is outside your work folder and the folder the browser and screen tools save to"));
    }
    let meta = std::fs::metadata(&real).map_err(|_| format!("{raw} cannot be read"))?;
    if !meta.is_file() {
        return Err(format!("{raw} is not a file"));
    }
    if meta.len() > loka_shared::media::MAX_VIDEO as u64 {
        return Err(format!("{raw} is larger than {} MB", loka_shared::media::MAX_VIDEO / (1024 * 1024)));
    }
    Ok(real)
}

async fn upload(api: &RunnerApi, lease_id: Uuid, token: &str, path: &Path) -> Result<String, String> {
    let bytes = tokio::fs::read(path).await.map_err(|_| format!("{} cannot be read", path.display()))?;
    loka_shared::media::checked(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let body = gzip(&bytes).map_err(|e| e.to_string())?;
    let response = api
        .client
        .post(api.lease_url(lease_id, "/media"))
        .query(&[("name", name)])
        .header("Authorization", format!("RunnerMcp {token}"))
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("could not upload {name}: {e}"))?;
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let why = answer.get("message").and_then(Value::as_str).unwrap_or("the upload was refused");
        return Err(format!("{name}: {why}"));
    }
    answer.get("id").and_then(Value::as_str).map(String::from).ok_or_else(|| format!("{name}: the upload gave no id"))
}

async fn upload_item_files(api: &RunnerApi, lease_id: Uuid, token: &str, roots: &[PathBuf], request: &mut Value) -> Result<(), String> {
    let Some(items) = request.pointer_mut("/params/arguments/items").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    if items.iter().filter(|item| item.get("file").is_some()).count() > MAX_CANVAS_FILES {
        return Err(format!("place at most {MAX_CANVAS_FILES} files at a time"));
    }
    for item in items.iter_mut() {
        let Some(fields) = item.as_object_mut() else { continue };
        let Some(file) = fields.remove("file") else { continue };
        let file = file.as_str().ok_or_else(|| "file is a path".to_string())?.to_string();
        let path = resolve(&file, roots)?;
        fields.insert("upload_id".into(), Value::String(upload(api, lease_id, token, &path).await?));
    }
    Ok(())
}

pub(super) async fn upload_files(api: &RunnerApi, lease_id: Uuid, token: &str, roots: &[PathBuf], request: &mut Value) -> Result<(), String> {
    if request.pointer("/params/name").and_then(Value::as_str) != Some("show_media") {
        return upload_item_files(api, lease_id, token, roots, request).await;
    }
    let files: Vec<String> = request
        .pointer("/params/arguments/files")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    if files.len() > loka_shared::media::MAX_PER_CALL {
        return Err(format!("show at most {} at a time", loka_shared::media::MAX_PER_CALL));
    }
    let mut ids = Vec::new();
    for file in &files {
        let path = resolve(file, roots)?;
        ids.push(Value::String(upload(api, lease_id, token, &path).await?));
    }
    let Some(args) = request.pointer_mut("/params/arguments").and_then(Value::as_object_mut) else {
        return Err("show_media needs arguments".into());
    };
    args.remove("files");
    let mut all = args.get("upload_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    all.extend(ids);
    args.insert("upload_ids".into(), Value::Array(all));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_must_stay_inside_the_job_roots() {
        let base = std::env::temp_dir().join(format!("loka-media-test-{}", Uuid::new_v4().simple()));
        let work = base.join("work");
        let shots = base.join("media");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&shots).unwrap();
        std::fs::write(work.join("a.png"), b"x").unwrap();
        std::fs::write(shots.join("b.png"), b"x").unwrap();
        std::fs::write(base.join("secret.png"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("secret.png"), work.join("link.png")).unwrap();
        let roots = vec![work.clone(), shots.clone()];
        assert!(resolve("a.png", &roots).is_ok());
        assert!(resolve(shots.join("b.png").to_str().unwrap(), &roots).is_ok());
        assert!(resolve("../secret.png", &roots).is_err());
        assert!(resolve("link.png", &roots).is_err());
        assert!(resolve(base.join("secret.png").to_str().unwrap(), &roots).is_err());
        assert!(resolve("missing.png", &roots).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn get_upload_calls_are_recognised_and_their_ids_checked() {
        let call = |name: &str, args: Value| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
        let id = Uuid::new_v4();
        assert!(wants_upload(&call("get_upload", json!({ "upload_id": id.to_string() }))));
        assert!(!wants_upload(&call("show_media", json!({ "upload_id": id.to_string() }))));
        assert!(!wants_upload(&json!({ "method": "tools/list", "params": { "name": "get_upload" } })));
        assert_eq!(upload_id(&call("get_upload", json!({ "upload_id": id.to_string() }))), Ok(id));
        assert_eq!(upload_id(&call("get_upload", json!({ "upload_id": format!("upload:{id}") }))), Ok(id));
        assert!(upload_id(&call("get_upload", json!({ "upload_id": "../../etc/passwd" }))).is_err());
        assert!(upload_id(&call("get_upload", json!({}))).is_err());
    }

    #[test]
    fn only_show_media_with_files_is_handled() {
        let call = |args: Value| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "show_media", "arguments": args } });
        assert!(wants_files(&call(json!({ "files": ["a.png"] }))));
        assert!(!wants_files(&call(json!({ "urls": ["https://x.test/a.png"] }))));
        assert!(!wants_files(&json!({ "method": "tools/call", "params": { "name": "list_tasks", "arguments": { "files": ["a"] } } })));
        let canvas = |name: &str, items: Value| json!({ "method": "tools/call", "params": { "name": name, "arguments": { "items": items } } });
        assert!(wants_files(&canvas("canvas_add_items", json!([{ "kind": "image", "file": "a.png" }]))));
        assert!(wants_files(&canvas("create_canvas", json!([{ "kind": "note", "text": "x" }, { "kind": "model", "file": "m.glb" }]))));
        assert!(!wants_files(&canvas("canvas_add_items", json!([{ "kind": "note", "text": "x" }]))));
        assert!(!wants_files(&canvas("canvas_update_items", json!([{ "id": "a", "file": "a.png" }]))));
    }
}
