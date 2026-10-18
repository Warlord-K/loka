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

pub(super) fn wants_files(request: &Value) -> bool {
    request.get("method").and_then(Value::as_str) == Some("tools/call")
        && request.pointer("/params/name").and_then(Value::as_str) == Some("show_media")
        && request.pointer("/params/arguments/files").and_then(Value::as_array).is_some_and(|files| !files.is_empty())
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

pub(super) async fn upload_files(api: &RunnerApi, lease_id: Uuid, token: &str, roots: &[PathBuf], request: &mut Value) -> Result<(), String> {
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
    fn only_show_media_with_files_is_handled() {
        let call = |args: Value| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "show_media", "arguments": args } });
        assert!(wants_files(&call(json!({ "files": ["a.png"] }))));
        assert!(!wants_files(&call(json!({ "urls": ["https://x.test/a.png"] }))));
        assert!(!wants_files(&json!({ "method": "tools/call", "params": { "name": "list_tasks", "arguments": { "files": ["a"] } } })));
    }
}
