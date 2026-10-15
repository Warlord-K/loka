use anyhow::{Context, Result};
use md5::{Digest, Md5};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::path::{Path, PathBuf};

const STORE_FILES: [&str; 4] = ["store.db", "store.db-wal", "store.db-shm", "meta.json"];

pub(crate) fn data_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|dir| !dir.is_empty()).map(PathBuf::from);
    set("CURSOR_CONFIG_DIR")
        .or_else(|| set("XDG_CONFIG_HOME").map(|dir| dir.join("cursor")))
        .or_else(|| set("HOME").map(|home| home.join(".cursor")))
}

fn workspace_key(working_dir: &Path) -> String {
    let dir = std::fs::canonicalize(working_dir).unwrap_or_else(|_| working_dir.to_path_buf());
    hex(&Md5::digest(dir.to_string_lossy().as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub(crate) fn find(root: &Path, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    let chats = std::fs::read_dir(root.join("chats")).into_iter().flatten().flatten().map(|entry| entry.path().join(id));
    std::iter::once(root.join("acp-sessions").join(id))
        .chain(chats)
        .filter(|dir| dir.join("store.db").is_file())
        .max_by_key(|dir| modified(dir))
}

async fn open(dir: &Path) -> Result<sqlx::SqliteConnection> {
    SqliteConnectOptions::new()
        .filename(dir.join("store.db"))
        .create_if_missing(false)
        .connect()
        .await
        .with_context(|| format!("Failed to open the Cursor chat store in {}", dir.display()))
}

async fn read_meta(conn: &mut sqlx::SqliteConnection) -> Result<Value> {
    let raw: String = sqlx::query_scalar("SELECT value FROM meta WHERE key = '0'")
        .fetch_one(&mut *conn)
        .await
        .context("Cursor chat store has no metadata")?;
    let bytes = (0..raw.len())
        .step_by(2)
        .map(|i| raw.get(i..i + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .context("Cursor chat metadata is not hex")?;
    serde_json::from_slice(&bytes).context("Cursor chat metadata is not JSON")
}

pub(crate) async fn root_of(id: &str) -> Option<String> {
    let dir = find(&data_dir()?, id)?;
    let mut conn = open(&dir).await.ok()?;
    let meta = read_meta(&mut conn).await.ok()?;
    let _ = conn.close().await;
    meta.get("latestRootBlobId").and_then(Value::as_str).map(String::from)
}

fn modified(dir: &Path) -> Option<std::time::SystemTime> {
    ["store.db", "store.db-wal"].iter().filter_map(|name| std::fs::metadata(dir.join(name)).ok()?.modified().ok()).max()
}

async fn copy_store(from: &Path, to: &Path) -> Result<()> {
    tokio::fs::create_dir_all(to).await?;
    for name in &STORE_FILES[..3] {
        match tokio::fs::remove_file(to.join(name)).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    let mut source = SqliteConnectOptions::new()
        .filename(from.join("store.db"))
        .read_only(true)
        .connect()
        .await
        .with_context(|| format!("Failed to open the Cursor chat store in {}", from.display()))?;
    sqlx::query("VACUUM INTO ?")
        .bind(to.join("store.db").display().to_string())
        .execute(&mut source)
        .await
        .context("Failed to snapshot the Cursor chat store")?;
    source.close().await?;
    Ok(())
}

pub(crate) async fn to_acp(id: &str) -> Result<()> {
    let root = data_dir().context("Cursor has no data directory")?;
    anyhow::ensure!(valid_id(id), "invalid Cursor chat id");
    let acp = root.join("acp-sessions").join(id);
    let chat = std::fs::read_dir(root.join("chats"))
        .ok()
        .and_then(|entries| entries.flatten().map(|e| e.path().join(id)).find(|dir| dir.join("store.db").is_file()));
    let Some(chat) = chat else {
        return Ok(());
    };
    if acp.join("store.db").is_file() && modified(&acp) >= modified(&chat) {
        return Ok(());
    }
    copy_store(&chat, &acp).await?;
    let cwd = std::fs::read(chat.join("meta.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|m| m.get("cwd").cloned());
    tokio::fs::write(acp.join("meta.json"), serde_json::json!({ "schemaVersion": 1, "cwd": cwd }).to_string()).await?;
    Ok(())
}

pub(crate) async fn from_acp(id: &str, working_dir: &Path) -> Result<()> {
    let root = data_dir().context("Cursor has no data directory")?;
    anyhow::ensure!(valid_id(id), "invalid Cursor chat id");
    let acp = root.join("acp-sessions").join(id);
    anyhow::ensure!(acp.join("store.db").is_file(), "the warm Cursor session has no store");
    let chat = root.join("chats").join(workspace_key(working_dir)).join(id);
    copy_store(&acp, &chat).await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default();
    let cwd = std::fs::canonicalize(working_dir).unwrap_or_else(|_| working_dir.to_path_buf());
    let meta = serde_json::json!({ "schemaVersion": 1, "createdAtMs": now, "hasConversation": true, "updatedAtMs": now, "cwd": cwd.display().to_string() });
    tokio::fs::write(chat.join("meta.json"), meta.to_string()).await?;
    Ok(())
}

pub(crate) async fn set_mode(id: &str, mode: &str) -> Result<()> {
    let root = data_dir().context("Cursor has no data directory (set HOME or CURSOR_CONFIG_DIR)")?;
    let dir = find(&root, id).with_context(|| format!("Cursor chat {id} was not found on this device"))?;
    let mut conn = open(&dir).await?;
    let mut meta = read_meta(&mut conn).await?;
    if meta.get("mode").and_then(Value::as_str) != Some(mode) {
        meta["mode"] = Value::String(mode.to_string());
        sqlx::query("UPDATE meta SET value = ? WHERE key = '0'")
            .bind(hex(meta.to_string().as_bytes()))
            .execute(&mut conn)
            .await?;
    }
    conn.close().await?;
    Ok(())
}

pub(crate) async fn branch(source: &str, working_dir: &Path, at: Option<&str>) -> Result<String> {
    let root = data_dir().context("Cursor has no data directory (set HOME or CURSOR_CONFIG_DIR)")?;
    let from = find(&root, source).with_context(|| format!("Cursor chat {source} was not found on this device"))?;
    let id = uuid::Uuid::new_v4().to_string();
    let parent = root.join("chats").join(workspace_key(working_dir));
    let to = parent.join(&id);
    tokio::fs::create_dir_all(&to).await.with_context(|| format!("Failed to create {}", to.display()))?;
    let copied = async {
        copy_store(&from, &to).await?;
        if let Err(error) = tokio::fs::copy(from.join("meta.json"), to.join("meta.json")).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error).context("Failed to copy the Cursor chat metadata");
            }
        }
        let mut conn = open(&to).await?;
        let mut meta = read_meta(&mut conn).await?;
        meta["agentId"] = Value::String(id.clone());
        if let Some(at) = at {
            anyhow::ensure!(at.chars().all(|c| c.is_ascii_hexdigit()), "Cursor checkpoint is not a blob id");
            let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs WHERE id = ?")
                .bind(at)
                .fetch_one(&mut conn)
                .await?;
            anyhow::ensure!(known > 0, "the Cursor chat no longer holds that checkpoint");
            meta["latestRootBlobId"] = Value::String(at.to_string());
        }
        sqlx::query("UPDATE meta SET value = ? WHERE key = '0'")
            .bind(hex(meta.to_string().as_bytes()))
            .execute(&mut conn)
            .await?;
        conn.close().await?;
        let meta_file = to.join("meta.json");
        if let Ok(text) = tokio::fs::read(&meta_file).await {
            if let Ok(mut file_meta) = serde_json::from_slice::<Value>(&text) {
                let cwd = std::fs::canonicalize(working_dir).unwrap_or_else(|_| working_dir.to_path_buf());
                file_meta["cwd"] = Value::String(cwd.display().to_string());
                tokio::fs::write(&meta_file, file_meta.to_string()).await?;
            }
        }
        let files = STORE_FILES.iter().map(|name| to.join(name));
        for path in [parent.clone(), to.clone()].into_iter().chain(files).filter(|path| path.exists()) {
            crate::cli::give_to_job_user(&path)?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = copied {
        let _ = tokio::fs::remove_dir_all(&to).await;
        return Err(error);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_keys_hash_the_resolved_path() {
        let dir = std::env::temp_dir();
        assert_eq!(workspace_key(&dir).len(), 32);
        assert_eq!(workspace_key(&dir), workspace_key(&std::fs::canonicalize(&dir).unwrap()));
    }

    #[test]
    fn chat_ids_cannot_escape_the_store() {
        let root = std::env::temp_dir();
        assert!(find(&root, "../etc").is_none());
        assert!(find(&root, "").is_none());
        assert!(valid_id("53fc29f7-54ad-4a4c-a440-ce6926cae77f"));
    }

    #[tokio::test]
    async fn branches_a_chat_at_an_earlier_root() {
        let base = std::env::temp_dir().join(format!("cursor-chats-{}", uuid::Uuid::new_v4().simple()));
        let source = base.join("chats").join("abc").join("11111111-2222-3333-4444-555555555555");
        std::fs::create_dir_all(&source).unwrap();
        let mut conn = SqliteConnectOptions::new().filename(source.join("store.db")).create_if_missing(true).connect().await.unwrap();
        sqlx::query("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)").execute(&mut conn).await.unwrap();
        sqlx::query("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB)").execute(&mut conn).await.unwrap();
        sqlx::query("INSERT INTO blobs (id, data) VALUES ('aa11', x'00'), ('bb22', x'00')").execute(&mut conn).await.unwrap();
        let meta = serde_json::json!({ "agentId": "11111111-2222-3333-4444-555555555555", "latestRootBlobId": "bb22", "mode": "search" });
        sqlx::query("INSERT INTO meta (key, value) VALUES ('0', ?)").bind(hex(meta.to_string().as_bytes())).execute(&mut conn).await.unwrap();
        conn.close().await.unwrap();
        std::fs::write(source.join("meta.json"), r#"{"cwd":"/old"}"#).unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::env::set_var("CURSOR_CONFIG_DIR", &base);
        let id = branch("11111111-2222-3333-4444-555555555555", &work, Some("aa11")).await.unwrap();
        assert_eq!(root_of(&id).await.as_deref(), Some("aa11"));
        assert_eq!(root_of("11111111-2222-3333-4444-555555555555").await.as_deref(), Some("bb22"));
        assert!(branch("11111111-2222-3333-4444-555555555555", &work, Some("ff99")).await.is_err());
        let copied = find(&base, &id).unwrap();
        assert!(copied.starts_with(base.join("chats").join(workspace_key(&work))));
        let file: Value = serde_json::from_slice(&std::fs::read(copied.join("meta.json")).unwrap()).unwrap();
        assert_eq!(file["cwd"], std::fs::canonicalize(&work).unwrap().display().to_string());
        set_mode(&id, "default").await.unwrap();
        let mode_of = |dir: PathBuf| async move {
            let mut conn = open(&dir).await.unwrap();
            let meta = read_meta(&mut conn).await.unwrap();
            meta["mode"].as_str().map(String::from)
        };
        assert_eq!(mode_of(copied.clone()).await.as_deref(), Some("default"));
        assert_eq!(mode_of(source.clone()).await.as_deref(), Some("search"), "the main chat keeps ask mode");
        std::env::remove_var("CURSOR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }
}
