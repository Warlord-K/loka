use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::StreamExt;
use gcp_auth::TokenProvider;
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::{info, warn};
use uuid::Uuid;

const VERSION: u32 = 1;
const MAX_COMPRESSED: usize = 24 * 1024 * 1024;
const MAX_EXPANDED: usize = 32 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
const PRUNE_AFTER: Duration = Duration::from_secs(3600);
const GCS_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";

pub struct RunnerStorage {
    client: Client,
    bucket: Bucket,
    auth: Auth,
    runner_id: Uuid,
    root: PathBuf,
    claude_dir: PathBuf,
    codex_dir: Option<PathBuf>,
    cursor_dir: Option<PathBuf>,
    memory_dir: PathBuf,
    checkpoint_lock: tokio::sync::Mutex<()>,
}

enum Auth {
    Keys(Credentials),
    Adc(Arc<dyn TokenProvider>),
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    runner_id: Uuid,
    root: String,
    claude_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_dir: Option<String>,
    files: Vec<SnapshotFile>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotFile {
    scope: Scope,
    path: String,
    data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modified: Option<u64>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Claude,
    Memory,
    Omp,
    Codex,
    Cursor,
}

const OMP_SESSIONS: &str = "omp/sessions";
const CODEX_SESSIONS: &str = "sessions";
const CURSOR_CHATS: &str = "chats";
const CURSOR_FILES: [&str; 3] = ["store.db", "store.db-wal", "meta.json"];

struct RestoredFile {
    scope: Scope,
    path: PathBuf,
    data: Vec<u8>,
    modified: Option<u64>,
}

impl RunnerStorage {
    pub async fn from_env(root: &Path, runner_id: Uuid) -> Result<Option<Self>> {
        let names = [
            "WORLD_RUNNER_STORAGE_ENDPOINT",
            "WORLD_RUNNER_STORAGE_REGION",
            "WORLD_RUNNER_STORAGE_BUCKET",
            "WORLD_RUNNER_STORAGE_ACCESS_KEY_ID",
            "WORLD_RUNNER_STORAGE_SECRET_ACCESS_KEY",
        ];
        let values = names
            .iter()
            .map(|name| {
                std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .collect::<Vec<_>>();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        if values[..3].iter().any(Option::is_none) || values[3].is_some() != values[4].is_some() {
            bail!("runner storage configuration must set endpoint, region and bucket, and both access keys or neither");
        }
        let endpoint = values[0].as_deref().unwrap();
        let region = values[1].as_deref().unwrap();
        let bucket_name = values[2].as_deref().unwrap();
        let auth = match (&values[3], &values[4]) {
            (Some(access_key), Some(secret_key)) => Auth::Keys(Credentials::new(access_key.clone(), secret_key.clone())),
            _ => Auth::Adc(
                gcp_auth::provider()
                    .await
                    .context("runner storage without access keys needs Google application default credentials")?,
            ),
        };
        let claude_dir = PathBuf::from(
            std::env::var("CLAUDE_CONFIG_DIR")
                .context("CLAUDE_CONFIG_DIR is required when runner storage is enabled")?,
        );
        let codex_dir = std::env::var_os("CODEX_HOME").filter(|dir| !dir.is_empty()).map(PathBuf::from);
        let cursor_dir = std::env::var_os("CURSOR_CONFIG_DIR").filter(|dir| !dir.is_empty()).map(PathBuf::from);
        validate_absolute_path(root)?;
        validate_absolute_path(&claude_dir)?;
        let overlaps = |a: &Path, b: &Path| a == b || a.starts_with(b) || b.starts_with(a);
        if overlaps(&claude_dir, root) {
            bail!("runner storage paths must be distinct absolute paths");
        }
        if let Some(codex_dir) = &codex_dir {
            validate_absolute_path(codex_dir)?;
            if overlaps(codex_dir, root) || overlaps(codex_dir, &claude_dir) {
                bail!("runner storage paths must be distinct absolute paths");
            }
        }
        if let Some(cursor_dir) = &cursor_dir {
            validate_absolute_path(cursor_dir)?;
            if overlaps(cursor_dir, root) || overlaps(cursor_dir, &claude_dir) || codex_dir.as_deref().is_some_and(|codex| overlaps(cursor_dir, codex)) {
                bail!("runner storage paths must be distinct absolute paths");
            }
        }
        let mut storage = Self::new(
            endpoint,
            region,
            bucket_name,
            auth,
            runner_id,
            root.to_path_buf(),
            claude_dir,
            false,
        )?;
        storage.codex_dir = codex_dir;
        storage.cursor_dir = cursor_dir;
        Ok(Some(storage))
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        endpoint: &str,
        region: &str,
        bucket_name: &str,
        auth: Auth,
        runner_id: Uuid,
        root: PathBuf,
        claude_dir: PathBuf,
        allow_http: bool,
    ) -> Result<Self> {
        let mut endpoint = Url::parse(endpoint).context("invalid runner storage endpoint")?;
        if endpoint.scheme() != "https" && !(allow_http && endpoint.scheme() == "http") {
            bail!("runner storage endpoint must use HTTPS");
        }
        if endpoint.query().is_some() || endpoint.fragment().is_some() {
            bail!("runner storage endpoint cannot contain a query or fragment");
        }
        if matches!(auth, Auth::Adc(_)) && endpoint.host_str() != Some("storage.googleapis.com") {
            bail!("runner storage without access keys only supports storage.googleapis.com");
        }
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        validate_bucket_name(bucket_name)?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            bucket_name.to_string(),
            region.to_string(),
        )?;
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        let memory_dir = root.join("memory");
        Ok(Self {
            client,
            bucket,
            auth,
            runner_id,
            root,
            claude_dir,
            codex_dir: None,
            cursor_dir: None,
            memory_dir,
            checkpoint_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn has_transcript(&self, cli: &str, session_id: &str) -> bool {
        let named = |name: String| name.ends_with(&format!("_{session_id}.jsonl"));
        match cli {
            "omp" => plain_entries(&self.root.join(OMP_SESSIONS))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|path| file_name(&path))
                .any(named),
            "codex" => match &self.codex_dir {
                Some(codex) => crate::codex::find_rollout(&codex.join(CODEX_SESSIONS), session_id).is_some(),
                None => true,
            },
            "cursor" => match &self.cursor_dir {
                Some(cursor) => crate::cursor::chats::find(cursor, session_id).is_some(),
                None => true,
            },
            "opencode" => opencode_database().is_none_or(|db| db.is_file()),
            _ => claude_transcript_in(&self.claude_dir, session_id),
        }
    }

    pub fn keeps_sessions_of(&self, cli: &str) -> bool {
        match cli {
            "claude" | "omp" => true,
            "codex" => self.codex_dir.is_some(),
            "cursor" => self.cursor_dir.is_some(),
            _ => false,
        }
    }

    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }

    fn key(&self) -> String {
        format!("{}/state-v1.json.gz", self.runner_id)
    }

    fn credentials(&self) -> Option<&Credentials> {
        match &self.auth {
            Auth::Keys(credentials) => Some(credentials),
            Auth::Adc(_) => None,
        }
    }

    async fn authorized(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        match &self.auth {
            Auth::Keys(_) => Ok(request),
            Auth::Adc(provider) => {
                let token = provider
                    .token(&[GCS_SCOPE])
                    .await
                    .context("runner storage could not get a Google access token")?;
                Ok(request.bearer_auth(token.as_str()))
            }
        }
    }

    pub async fn restore(&self) -> Result<()> {
        ensure_absolute_dir(&self.claude_dir)?;
        ensure_absolute_dir(&self.memory_dir)?;
        if let Some(codex) = &self.codex_dir {
            ensure_absolute_dir(codex)?;
        }
        if let Some(cursor) = &self.cursor_dir {
            ensure_absolute_dir(cursor)?;
        }
        let head_url = self
            .bucket
            .head_bucket(self.credentials())
            .sign(Duration::from_secs(60));
        let response = self
            .authorized(self.client.head(head_url))
            .await?
            .send()
            .await
            .map_err(without_url)
            .context("runner storage bucket check failed")?;
        if !response.status().is_success() {
            bail!("runner storage bucket check returned {}", response.status());
        }
        let get_url = self
            .bucket
            .get_object(self.credentials(), &self.key())
            .sign(Duration::from_secs(60));
        let response = self
            .authorized(self.client.get(get_url))
            .await?
            .send()
            .await
            .map_err(without_url)
            .context("runner storage restore failed")?;
        if response.status() == StatusCode::NOT_FOUND {
            let root = self.root.clone();
            let claude_dir = self.claude_dir.clone();
            let codex_dir = self.codex_dir.clone();
            let cursor_dir = self.cursor_dir.clone();
            tokio::task::spawn_blocking(move || replace_snapshot_trees(&root, &claude_dir, codex_dir.as_deref(), cursor_dir.as_deref(), vec![]))
                .await??;
            info!(runner_id = %self.runner_id, "No runner checkpoint found");
            return Ok(());
        }
        if !response.status().is_success() {
            bail!("runner storage restore returned {}", response.status());
        }
        let bytes = bounded_body(response).await?;
        let root = self.root.clone();
        let claude_dir = self.claude_dir.clone();
        let codex_dir = self.codex_dir.clone();
        let cursor_dir = self.cursor_dir.clone();
        let runner_id = self.runner_id;
        tokio::task::spawn_blocking(move || {
            restore_snapshot(&bytes, runner_id, &root, &claude_dir, codex_dir.as_deref(), cursor_dir.as_deref())
        })
        .await??;
        info!(runner_id = %self.runner_id, "Runner checkpoint restored");
        Ok(())
    }

    pub async fn checkpoint(
        &self,
        required_session_id: Option<&str>,
        live_sessions: Option<(&[String], SystemTime)>,
    ) -> Result<()> {
        let _serialized = self.checkpoint_lock.lock().await;
        let root = self.root.clone();
        let claude_dir = self.claude_dir.clone();
        let codex_dir = self.codex_dir.clone();
        let cursor_dir = self.cursor_dir.clone();
        let runner_id = self.runner_id;
        let required_session_id = required_session_id.map(str::to_string);
        let live_sessions = live_sessions.map(|(ids, listed_at)| {
            (ids.iter().cloned().collect::<HashSet<_>>(), listed_at - PRUNE_AFTER)
        });
        let body = tokio::task::spawn_blocking(move || {
            if let Some((live, cutoff)) = &live_sessions {
                let codex_sessions = codex_dir.as_ref().map(|codex| codex.join(CODEX_SESSIONS));
                let cursor_chats = cursor_dir.as_ref().map(|cursor| cursor.join(CURSOR_CHATS));
                match prune_sessions(&claude_dir.join("projects"), &root.join(OMP_SESSIONS), codex_sessions.as_deref(), cursor_chats.as_deref(), live, *cutoff) {
                    Ok(0) => {}
                    Ok(pruned) => info!(pruned, "Pruned transcripts of sessions that are no longer live"),
                    Err(error) => warn!(%error, "Failed to prune old session transcripts"),
                }
            }
            build_snapshot(
                runner_id,
                &root,
                &claude_dir,
                codex_dir.as_deref(),
                cursor_dir.as_deref(),
                required_session_id.as_deref(),
            )
        })
        .await??;
        let mut delay = Duration::from_secs(1);
        loop {
            let put_url = self
                .bucket
                .put_object(self.credentials(), &self.key())
                .sign(Duration::from_secs(60));
            let request = self
                .client
                .put(put_url)
                .header("Content-Type", "application/gzip")
                .header("Cache-Control", "no-store")
                .body(body.clone());
            let result = match self.authorized(request).await {
                Ok(request) => request.send().await.map_err(without_url),
                Err(error) => Err(error),
            };
            match result {
                Ok(response) if response.status().is_success() => {
                    info!(runner_id = %self.runner_id, "Runner checkpoint uploaded");
                    return Ok(());
                }
                Ok(response) => {
                    warn!(status = %response.status(), "Runner checkpoint upload failed")
                }
                Err(error) => warn!(%error, "Runner checkpoint upload failed"),
            }
            sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(15));
        }
    }
}

fn without_url(error: reqwest::Error) -> anyhow::Error {
    anyhow!(error.without_url())
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_COMPRESSED as u64)
    {
        bail!("runner checkpoint exceeds compressed size limit");
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(without_url)?;
        if body.len() + chunk.len() > MAX_COMPRESSED {
            bail!("runner checkpoint exceeds compressed size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn build_snapshot(
    runner_id: Uuid,
    root: &Path,
    claude_dir: &Path,
    codex_dir: Option<&Path>,
    cursor_dir: Option<&Path>,
    required_session_id: Option<&str>,
) -> Result<Vec<u8>> {
    let root = fs::canonicalize(root)?;
    let claude_dir = fs::canonicalize(claude_dir)?;
    let codex_dir = codex_dir.map(fs::canonicalize).transpose()?;
    let cursor_dir = cursor_dir.map(fs::canonicalize).transpose()?;
    let mut files = Vec::new();
    let mut total = 0;
    collect_files(
        &claude_dir.join("projects"),
        &claude_dir.join("projects"),
        Scope::Claude,
        &mut files,
        &mut total,
    )?;
    collect_files(
        &root.join(OMP_SESSIONS),
        &root.join(OMP_SESSIONS),
        Scope::Omp,
        &mut files,
        &mut total,
    )?;
    if let Some(codex_dir) = &codex_dir {
        let sessions = codex_dir.join(CODEX_SESSIONS);
        collect_files(&sessions, &sessions, Scope::Codex, &mut files, &mut total)?;
    }
    if let Some(cursor_dir) = &cursor_dir {
        let chats = cursor_dir.join(CURSOR_CHATS);
        collect_files(&chats, &chats, Scope::Cursor, &mut files, &mut total)?;
    }
    if let Some(session_id) = required_session_id {
        if !files.iter().any(|file| holds_session(file, session_id)) {
            bail!("started CLI session is missing from the checkpoint");
        }
    }
    collect_files(
        &root.join("memory"),
        &root.join("memory"),
        Scope::Memory,
        &mut files,
        &mut total,
    )?;
    let snapshot = Snapshot {
        version: VERSION,
        runner_id,
        root: root.to_string_lossy().to_string(),
        claude_dir: claude_dir.to_string_lossy().to_string(),
        codex_dir: codex_dir.map(|dir| dir.to_string_lossy().to_string()),
        cursor_dir: cursor_dir.map(|dir| dir.to_string_lossy().to_string()),
        files,
    };
    let json = serde_json::to_vec(&snapshot)?;
    if json.len() > MAX_EXPANDED {
        bail!("runner checkpoint exceeds expanded size limit");
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;
    if compressed.len() > MAX_COMPRESSED {
        bail!("runner checkpoint exceeds compressed size limit");
    }
    Ok(compressed)
}

fn prune_codex_rollouts(dir: &Path, live: &HashSet<String>, cutoff: SystemTime) -> Result<usize> {
    let mut pruned = 0;
    for entry in plain_entries(dir)? {
        let metadata = fs::symlink_metadata(&entry)?;
        if metadata.is_dir() {
            pruned += prune_codex_rollouts(&entry, live, cutoff)?;
            if plain_entries(&entry)?.is_empty() {
                let _ = fs::remove_dir(&entry);
            }
            continue;
        }
        let id = file_name(&entry).and_then(|name| crate::codex::rollout_thread_id(&name).map(str::to_string));
        if let Some(id) = id {
            if prune_transcript(&entry, &id, live, cutoff)? {
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

fn prune_cursor_chats(chats: &Path, live: &HashSet<String>, cutoff: SystemTime) -> Result<usize> {
    let mut pruned = 0;
    for workspace in plain_entries(chats)? {
        if !fs::symlink_metadata(&workspace)?.is_dir() {
            continue;
        }
        for chat in plain_entries(&workspace)? {
            let Some(id) = file_name(&chat) else {
                continue;
            };
            if live.contains(&id) || !fs::symlink_metadata(&chat)?.is_dir() {
                continue;
            }
            let newest = plain_entries(&chat)?
                .iter()
                .filter_map(|file| fs::symlink_metadata(file).ok()?.modified().ok())
                .max();
            if newest.is_some_and(|modified| modified > cutoff) {
                continue;
            }
            fs::remove_dir_all(&chat)?;
            pruned += 1;
        }
        if plain_entries(&workspace)?.is_empty() {
            let _ = fs::remove_dir(&workspace);
        }
    }
    Ok(pruned)
}

fn opencode_database() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|home| !home.is_empty()).map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data.join("opencode/opencode.db"))
}

fn prune_sessions(
    claude_projects: &Path,
    omp_sessions: &Path,
    codex_sessions: Option<&Path>,
    cursor_chats: Option<&Path>,
    live: &HashSet<String>,
    cutoff: SystemTime,
) -> Result<usize> {
    let mut pruned = match codex_sessions {
        Some(dir) => prune_codex_rollouts(dir, live, cutoff)?,
        None => 0,
    };
    if let Some(dir) = cursor_chats {
        pruned += prune_cursor_chats(dir, live, cutoff)?;
    }
    for project in plain_entries(claude_projects)? {
        if !project.is_dir() || project.file_name().is_some_and(|name| name == "memory") {
            continue;
        }
        for file in plain_entries(&project)? {
            let Some(id) = file_name(&file).and_then(|name| name.strip_suffix(".jsonl").map(str::to_string)) else {
                continue;
            };
            if prune_transcript(&file, &id, live, cutoff)? {
                let subagents = project.join(&id);
                if fs::symlink_metadata(&subagents).is_ok_and(|metadata| metadata.is_dir()) {
                    fs::remove_dir_all(&subagents)?;
                }
                pruned += 1;
            }
        }
    }
    for file in plain_entries(omp_sessions)? {
        let id = file_name(&file).and_then(|name| {
            name.strip_suffix(".jsonl")
                .and_then(|stem| stem.rsplit_once('_'))
                .map(|(_, id)| id.to_string())
        });
        if let Some(id) = id {
            if prune_transcript(&file, &id, live, cutoff)? {
                pruned += 1;
            }
        }
    }
    Ok(pruned)
}

pub(crate) fn claude_transcript_in(claude_dir: &Path, session_id: &str) -> bool {
    let wanted = format!("{session_id}.jsonl");
    plain_entries(&claude_dir.join("projects")).unwrap_or_default().iter().any(|dir| {
        plain_entries(dir).unwrap_or_default().iter().filter_map(|path| file_name(path)).any(|name| name == wanted)
    })
}

fn plain_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_symlink() {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name().and_then(|name| name.to_str()).map(str::to_string)
}

fn prune_transcript(path: &Path, id: &str, live: &HashSet<String>, cutoff: SystemTime) -> Result<bool> {
    if live.contains(id) {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(path)?;
    let stale = metadata.modified().is_ok_and(|modified| modified <= cutoff);
    if !metadata.is_file() || !stale {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

fn holds_session(file: &SnapshotFile, session_id: &str) -> bool {
    let Some(name) = Path::new(&file.path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match file.scope {
        Scope::Claude => name == format!("{session_id}.jsonl"),
        Scope::Omp => name.ends_with(&format!("_{session_id}.jsonl")),
        Scope::Codex => crate::codex::rollout_thread_id(name) == Some(session_id),
        Scope::Cursor => name == "store.db" && cursor_chat_id(Path::new(&file.path)) == Some(session_id),
        Scope::Memory => false,
    }
}

fn drop_partial_line(data: &mut Vec<u8>) {
    let complete = data.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if complete < data.len() && serde_json::from_slice::<serde_json::Value>(&data[complete..]).is_err() {
        data.truncate(complete);
    }
}

fn open_snapshot_source(path: &Path) -> Result<fs::File> {
    #[cfg(unix)]
    if crate::cli::job_user().is_some() {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || !crate::cli::is_job_uid(metadata.uid()) {
            bail!("runner checkpoint source is not owned by the job user");
        }
        return Ok(file);
    }
    Ok(fs::File::open(path)?)
}

fn collect_files(
    base: &Path,
    current: &Path,
    scope: Scope,
    files: &mut Vec<SnapshotFile>,
    total: &mut usize,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(current) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        tracing::warn!(path = %current.display(), "Skipping a symlink in the runner checkpoint");
        return Ok(());
    }
    if metadata.is_file() {
        let relative = current.strip_prefix(base)?;
        if !allowed_file(scope, relative) {
            return Ok(());
        }
        let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if files.len() >= MAX_FILES || total.checked_add(length).is_none_or(|length| length > MAX_FILE_BYTES) {
            tracing::warn!(path = %current.display(), length, "Skipping a file that does not fit in the runner checkpoint");
            return Ok(());
        }
        let remaining = MAX_FILE_BYTES - *total;
        let mut data = Vec::with_capacity(length.min(remaining));
        open_snapshot_source(current)?
            .take((remaining + 1) as u64)
            .read_to_end(&mut data)?;
        if data.len() > remaining {
            bail!("runner checkpoint exceeds file size limit");
        }
        if relative.extension().is_some_and(|ext| ext == "jsonl") {
            drop_partial_line(&mut data);
        }
        *total = total
            .checked_add(data.len())
            .ok_or_else(|| anyhow!("runner checkpoint size overflow"))?;
        if *total > MAX_FILE_BYTES {
            bail!("runner checkpoint exceeds file size limit");
        }
        files.push(SnapshotFile {
            scope,
            path: relative_path(relative)?,
            data: STANDARD.encode(data),
            modified: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|age| age.as_secs()),
        });
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!("runner checkpoint source contains a special file");
    }
    let mut entries = fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        collect_files(base, &entry.path(), scope, files, total)?;
    }
    Ok(())
}

fn allowed_file(scope: Scope, path: &Path) -> bool {
    match scope {
        Scope::Claude => is_claude_file(path),
        Scope::Omp => path.extension().is_some_and(|extension| extension == "jsonl"),
        Scope::Codex => path.file_name().and_then(|name| name.to_str()).and_then(crate::codex::rollout_thread_id).is_some(),
        Scope::Cursor => cursor_chat_id(path).is_some(),
        Scope::Memory => true,
    }
}

fn cursor_chat_id(path: &Path) -> Option<&str> {
    let parts: Vec<&str> = path.components().map(|c| c.as_os_str().to_str()).collect::<Option<_>>()?;
    let [workspace, id, name] = parts.as_slice() else {
        return None;
    };
    let workspace_ok = workspace.len() == 32 && workspace.bytes().all(|b| b.is_ascii_hexdigit());
    let id_ok = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    (workspace_ok && id_ok && CURSOR_FILES.contains(name)).then_some(*id)
}

fn is_claude_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        || path
            .components()
            .any(|component| component.as_os_str() == "memory")
}

fn restore_snapshot(bytes: &[u8], runner_id: Uuid, root: &Path, claude_dir: &Path, codex_dir: Option<&Path>, cursor_dir: Option<&Path>) -> Result<()> {
    restore_snapshot_with_limits(
        bytes,
        runner_id,
        root,
        claude_dir,
        codex_dir,
        cursor_dir,
        MAX_EXPANDED,
        MAX_FILES,
        MAX_FILE_BYTES,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_snapshot_with_limits(
    bytes: &[u8],
    runner_id: Uuid,
    root: &Path,
    claude_dir: &Path,
    codex_dir: Option<&Path>,
    cursor_dir: Option<&Path>,
    max_expanded: usize,
    max_files: usize,
    max_file_bytes: usize,
) -> Result<()> {
    let mut decoder = GzDecoder::new(bytes);
    let mut json = Vec::new();
    decoder
        .by_ref()
        .take((max_expanded + 1) as u64)
        .read_to_end(&mut json)?;
    if json.len() > max_expanded {
        bail!("runner checkpoint exceeds expanded size limit");
    }
    let snapshot: Snapshot = serde_json::from_slice(&json).context("invalid runner checkpoint")?;
    let root = fs::canonicalize(root)?;
    let claude_dir = fs::canonicalize(claude_dir)?;
    let codex_dir = codex_dir.map(fs::canonicalize).transpose()?;
    let cursor_dir = cursor_dir.map(fs::canonicalize).transpose()?;
    let matches = |saved: &Option<String>, current: &Option<PathBuf>| match (saved, current) {
        (None, _) => true,
        (Some(saved), Some(current)) => *saved == current.to_string_lossy(),
        (Some(_), None) => false,
    };
    let codex_matches = matches(&snapshot.codex_dir, &codex_dir) && matches(&snapshot.cursor_dir, &cursor_dir);
    if snapshot.version != VERSION
        || snapshot.runner_id != runner_id
        || snapshot.root != root.to_string_lossy()
        || snapshot.claude_dir != claude_dir.to_string_lossy()
        || !codex_matches
    {
        bail!("runner checkpoint identity does not match this runner");
    }
    if snapshot.files.len() > max_files {
        bail!("runner checkpoint exceeds file count limit");
    }
    let mut restored = Vec::with_capacity(snapshot.files.len());
    let mut seen = HashSet::new();
    let mut total = 0usize;
    for file in snapshot.files {
        let path = validate_snapshot_path(&file.path)?;
        if !seen.insert((file.scope as u8, path.clone())) {
            bail!("runner checkpoint contains a duplicate path");
        }
        if !allowed_file(file.scope, &path)
            || matches!(file.scope, Scope::Codex) && codex_dir.is_none()
            || matches!(file.scope, Scope::Cursor) && cursor_dir.is_none()
        {
            bail!("runner checkpoint contains a disallowed file");
        }
        let data = STANDARD
            .decode(file.data)
            .context("invalid runner checkpoint data")?;
        total = total
            .checked_add(data.len())
            .ok_or_else(|| anyhow!("runner checkpoint size overflow"))?;
        if total > max_file_bytes {
            bail!("runner checkpoint exceeds file size limit");
        }
        restored.push(RestoredFile {
            scope: file.scope,
            path,
            data,
            modified: file.modified,
        });
    }
    replace_snapshot_trees(&root, &claude_dir, codex_dir.as_deref(), cursor_dir.as_deref(), restored)
}

fn replace_snapshot_trees(root: &Path, claude_dir: &Path, codex_dir: Option<&Path>, cursor_dir: Option<&Path>, files: Vec<RestoredFile>) -> Result<()> {
    let root = fs::canonicalize(root)?;
    let claude_dir = fs::canonicalize(claude_dir)?;
    let omp_sessions = root.join(OMP_SESSIONS);
    if let Some(parent) = omp_sessions.parent() {
        clear_tree_target(parent)?;
        fs::create_dir_all(parent)?;
    }
    let mut scoped = vec![
        (Scope::Claude, claude_dir.join("projects")),
        (Scope::Memory, root.join("memory")),
        (Scope::Omp, omp_sessions),
    ];
    if let Some(codex_dir) = codex_dir {
        fs::create_dir_all(codex_dir)?;
        scoped.push((Scope::Codex, fs::canonicalize(codex_dir)?.join(CODEX_SESSIONS)));
    }
    if let Some(cursor_dir) = cursor_dir {
        fs::create_dir_all(cursor_dir)?;
        scoped.push((Scope::Cursor, fs::canonicalize(cursor_dir)?.join(CURSOR_CHATS)));
    }
    let scopes: Vec<u8> = scoped.iter().map(|(scope, _)| *scope as u8).collect();
    let targets: Vec<PathBuf> = scoped.into_iter().map(|(_, target)| target).collect();
    for target in &targets {
        clear_tree_target(target)?;
    }

    let nonce = Uuid::new_v4();
    let mut stages: Vec<PathBuf> = Vec::with_capacity(targets.len());
    let staged = (|| {
        for target in &targets {
            let stage = staging_path(target, "restore", nonce);
            fs::create_dir(&stage)?;
            crate::cli::share_with_jobs(&stage, 0o2770)?;
            stages.push(stage);
        }
        for file in files {
            let base = scopes
                .iter()
                .position(|scope| *scope == file.scope as u8)
                .and_then(|at| stages.get(at))
                .context("runner checkpoint contains a disallowed file")?;
            let target = secure_target(base, &file.path)?;
            fs::write(&target, file.data)?;
            fs::File::options()
                .write(true)
                .open(&target)?
                .set_modified(UNIX_EPOCH + Duration::from_secs(file.modified.unwrap_or(0)))?;
            crate::cli::share_with_jobs(&target, 0o660)?;
            crate::cli::give_to_job_user(&target)?;
        }
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = staged {
        for stage in &stages {
            let _ = fs::remove_dir_all(stage);
        }
        return Err(error);
    }

    swap_snapshot_trees(&targets, &stages, nonce)
}

fn staging_path(target: &Path, purpose: &str, nonce: Uuid) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{purpose}-{nonce}"))
}

fn clear_tree_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Ok(fs::remove_file(path)?),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn swap_snapshot_trees(targets: &[PathBuf], stages: &[PathBuf], nonce: Uuid) -> Result<()> {
    let backups: Vec<PathBuf> = targets
        .iter()
        .map(|target| staging_path(target, "backup", nonce))
        .collect();
    let mut moved: Vec<bool> = Vec::with_capacity(targets.len());
    let mut installed = 0;
    let result = (|| {
        for (target, backup) in targets.iter().zip(&backups) {
            let exists = target.exists();
            if exists {
                fs::rename(target, backup)?;
            }
            moved.push(exists);
        }
        for (target, stage) in targets.iter().zip(stages) {
            fs::rename(stage, target)?;
            installed += 1;
        }
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = result {
        for target in targets.iter().take(installed) {
            fs::remove_dir_all(target)?;
        }
        for ((target, backup), was_moved) in targets.iter().zip(&backups).zip(&moved) {
            if *was_moved {
                fs::rename(backup, target)?;
            }
        }
        for stage in stages.iter().skip(installed) {
            let _ = fs::remove_dir_all(stage);
        }
        return Err(error);
    }
    for (backup, was_moved) in backups.iter().zip(&moved) {
        if *was_moved {
            fs::remove_dir_all(backup)?;
        }
    }
    Ok(())
}

fn relative_path(path: &Path) -> Result<String> {
    validate_snapshot_path(&path.to_string_lossy())?;
    Ok(path.to_string_lossy().to_string())
}

fn validate_snapshot_path(value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.contains('\\') {
        bail!("invalid runner checkpoint path");
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid runner checkpoint path");
    }
    Ok(path.to_path_buf())
}

fn ensure_absolute_dir(path: &Path) -> Result<()> {
    validate_absolute_path(path)?;
    let mut current = PathBuf::from("/");
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("runner storage path contains a symlink")
            }
            Ok(metadata) if !metadata.is_dir() => bail!("runner storage path is not a directory"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        bail!("runner storage path must be an absolute normalized path");
    }
    Ok(())
}

fn secure_target(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = fs::canonicalize(base)?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        let Component::Normal(name) = component else {
            bail!("invalid runner checkpoint path");
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("runner checkpoint target contains a symlink")
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!("runner checkpoint target is not a directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                crate::cli::share_with_jobs(&current, 0o2770)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let target = current.join(relative.file_name().unwrap());
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("runner checkpoint target contains a symlink")
        }
        Ok(metadata) if !metadata.is_file() => bail!("runner checkpoint target is not a file"),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(target)
}

fn validate_bucket_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("invalid runner storage bucket");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_paths() -> (PathBuf, PathBuf) {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("loka-storage-{}", Uuid::new_v4()));
        let root = base.join("root");
        let claude = base.join("claude");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&claude).unwrap();
        (root, claude)
    }

    async fn mock_s3(statuses: Vec<u16>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for status in statuses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16 * 1024];
                let length = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                requests.push(request.lines().next().unwrap().to_string());
                let reason = match status {
                    200 => "OK",
                    403 => "Forbidden",
                    404 => "Not Found",
                    500 => "Internal Server Error",
                    _ => "Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}/storage/v1/s3"), server)
    }

    async fn test_storage(
        statuses: Vec<u16>,
    ) -> (RunnerStorage, tokio::task::JoinHandle<Vec<String>>) {
        let (endpoint, server) = mock_s3(statuses).await;
        let (root, claude) = temp_paths();
        let storage = RunnerStorage::new(
            &endpoint,
            "ap-southeast-1",
            "test-bucket",
            Auth::Keys(Credentials::new("access", "secret")),
            Uuid::new_v4(),
            root,
            claude,
            true,
        )
        .unwrap();
        (storage, server)
    }

    #[test]
    fn snapshot_round_trip_preserves_only_sessions_and_memory() {
        let (root, claude) = temp_paths();
        fs::create_dir_all(claude.join("projects/a/memory")).unwrap();
        fs::create_dir_all(claude.join("cache")).unwrap();
        fs::create_dir_all(root.join("memory")).unwrap();
        fs::create_dir_all(root.join("workspaces/task")).unwrap();
        fs::write(claude.join("projects/a/session.jsonl"), br#"{"type":"user"}"#).unwrap();
        fs::write(claude.join("projects/a/memory/MEMORY.md"), b"auto").unwrap();
        fs::write(claude.join(".credentials.json"), b"secret").unwrap();
        fs::write(claude.join("cache/data"), b"cache").unwrap();
        fs::write(root.join("memory/notes.md"), b"notes").unwrap();
        fs::write(root.join("workspaces/task/file"), b"task").unwrap();
        let runner_id = Uuid::new_v4();
        let bytes = build_snapshot(runner_id, &root, &claude, None, None, None).unwrap();
        fs::remove_file(claude.join("projects/a/session.jsonl")).unwrap();
        fs::remove_file(claude.join("projects/a/memory/MEMORY.md")).unwrap();
        fs::remove_file(root.join("memory/notes.md")).unwrap();
        fs::write(claude.join("projects/stale.jsonl"), b"stale").unwrap();
        fs::write(root.join("memory/stale.md"), b"stale").unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, None, None).unwrap();
        assert_eq!(
            fs::read(claude.join("projects/a/session.jsonl")).unwrap(),
            br#"{"type":"user"}"#
        );
        assert_eq!(
            fs::read(claude.join("projects/a/memory/MEMORY.md")).unwrap(),
            b"auto"
        );
        assert_eq!(fs::read(root.join("memory/notes.md")).unwrap(), b"notes");
        assert!(!claude.join("projects/stale.jsonl").exists());
        assert!(!root.join("memory/stale.md").exists());
        let snapshot = decode(&bytes);
        assert_eq!(snapshot.files.len(), 3);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn snapshot_drops_a_half_written_session_line() {
        let mut data = b"{\"a\":1}\n{\"b\":2}\n{\"c\":".to_vec();
        super::drop_partial_line(&mut data);
        assert_eq!(data, b"{\"a\":1}\n{\"b\":2}\n");
        let mut whole = b"{\"a\":1}\n{\"b\":2}".to_vec();
        super::drop_partial_line(&mut whole);
        assert_eq!(whole, b"{\"a\":1}\n{\"b\":2}");
    }

    #[tokio::test]
    async fn restore_accepts_only_object_not_found() {
        let (storage, server) = test_storage(vec![200, 404]).await;
        fs::create_dir_all(storage.claude_dir.join("projects")).unwrap();
        fs::create_dir_all(storage.root.join("memory")).unwrap();
        fs::create_dir_all(storage.root.join("workspaces/keep")).unwrap();
        fs::write(storage.claude_dir.join("projects/stale.jsonl"), b"stale").unwrap();
        fs::write(storage.root.join("memory/stale.md"), b"stale").unwrap();
        fs::write(storage.root.join("workspaces/keep/file"), b"keep").unwrap();
        storage.restore().await.unwrap();
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("HEAD /storage/v1/s3/test-bucket"));
        assert!(requests[0].contains("X-Amz-Signature="));
        assert!(requests[1].starts_with("GET /storage/v1/s3/test-bucket/"));
        assert_eq!(
            fs::read_dir(storage.claude_dir.join("projects"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(storage.root.join("memory")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read(storage.root.join("workspaces/keep/file")).unwrap(),
            b"keep"
        );

        for statuses in [vec![403], vec![500], vec![200, 403], vec![200, 500]] {
            let (storage, server) = test_storage(statuses).await;
            assert!(storage.restore().await.is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn checkpoint_retries_the_same_state() {
        let (storage, server) = test_storage(vec![500, 200]).await;
        storage.checkpoint(None, None).await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.starts_with("PUT /storage/v1/s3/test-bucket/")));
    }

    #[test]
    fn restore_rejects_identity_corruption_and_traversal() {
        let (root, claude) = temp_paths();
        fs::create_dir_all(root.join("memory")).unwrap();
        let runner_id = Uuid::new_v4();
        let bytes = build_snapshot(runner_id, &root, &claude, None, None, None).unwrap();
        fs::write(root.join("memory/keep.md"), b"keep").unwrap();
        assert!(restore_snapshot(&bytes, Uuid::new_v4(), &root, &claude, None, None).is_err());
        assert!(restore_snapshot(b"bad", runner_id, &root, &claude, None, None).is_err());
        assert_eq!(fs::read(root.join("memory/keep.md")).unwrap(), b"keep");
        let mut snapshot = decode(&bytes);
        snapshot.version += 1;
        assert!(restore_snapshot(&encode(&snapshot), runner_id, &root, &claude, None, None).is_err());
        snapshot.version = VERSION;
        snapshot.files.push(SnapshotFile {
            scope: Scope::Memory,
            path: "../escape".to_string(),
            data: STANDARD.encode(b"bad"),
            modified: None,
        });
        let bytes = encode(&snapshot);
        assert!(restore_snapshot(&bytes, runner_id, &root, &claude, None, None).is_err());
        snapshot.files[0].path = "note".to_string();
        snapshot.files.push(SnapshotFile {
            scope: Scope::Memory,
            path: "note".to_string(),
            data: STANDARD.encode(b"bad"),
            modified: None,
        });
        assert!(restore_snapshot(&encode(&snapshot), runner_id, &root, &claude, None, None).is_err());
        snapshot.files.truncate(1);
        assert!(restore_snapshot_with_limits(
            &encode(&snapshot),
            runner_id,
            &root,
            &claude,
            None,
            None,
            MAX_EXPANDED,
            MAX_FILES,
            2,
        )
        .is_err());
        assert!(restore_snapshot_with_limits(
            &encode(&snapshot),
            runner_id,
            &root,
            &claude,
            None,
            None,
            8,
            MAX_FILES,
            MAX_FILE_BYTES,
        )
        .is_err());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_requires_the_observed_claude_transcript() {
        let (root, claude) = temp_paths();
        fs::create_dir_all(claude.join("projects/a")).unwrap();
        fs::create_dir_all(root.join("memory")).unwrap();
        assert!(build_snapshot(Uuid::new_v4(), &root, &claude, None, None, Some("started")).is_err());
        fs::write(claude.join("projects/a/started.jsonl"), b"session").unwrap();
        assert!(build_snapshot(Uuid::new_v4(), &root, &claude, None, None, Some("started")).is_ok());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn snapshot_round_trips_omp_sessions_only() {
        let (root, claude) = temp_paths();
        let sessions = root.join(super::OMP_SESSIONS);
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("2026-10-17T18-24-47Z_omp-id.jsonl"), b"{\"type\":\"session\"}\n").unwrap();
        fs::write(sessions.join(".2026-10-17T18-24-47Z_omp-id.jsonl.lock.os"), b"lock").unwrap();
        let runner_id = Uuid::new_v4();
        assert!(build_snapshot(runner_id, &root, &claude, None, None, Some("missing")).is_err());
        let bytes = build_snapshot(runner_id, &root, &claude, None, None, Some("omp-id")).unwrap();
        fs::remove_dir_all(&sessions).unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, None, None).unwrap();
        assert_eq!(
            fs::read(sessions.join("2026-10-17T18-24-47Z_omp-id.jsonl")).unwrap(),
            b"{\"type\":\"session\"}\n"
        );
        assert!(!sessions.join(".2026-10-17T18-24-47Z_omp-id.jsonl.lock.os").exists());
        assert!(claude.join("projects").is_dir());
        assert!(root.join("memory").is_dir());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    const THREAD: &str = "01a1cbc4-86c2-7963-b5ee-a33180afa3db";
    const OLD_THREAD: &str = "01a1cb00-0000-7000-8000-000000000000";

    #[test]
    fn snapshot_round_trips_codex_rollouts_only() {
        let (root, claude) = temp_paths();
        let codex = root.parent().unwrap().join("codex");
        let day = codex.join("sessions/2026/11/11");
        fs::create_dir_all(&day).unwrap();
        let rollout = format!("rollout-2026-06-11T23-30-17-{THREAD}.jsonl");
        fs::write(day.join(&rollout), b"{\"type\":\"session_meta\"}\n{\"type\":").unwrap();
        fs::write(day.join("notes.txt"), b"skip").unwrap();
        fs::write(codex.join("auth.json"), b"secret").unwrap();
        fs::write(codex.join("config.toml"), b"model = \"x\"").unwrap();
        let runner_id = Uuid::new_v4();
        assert!(build_snapshot(runner_id, &root, &claude, Some(&codex), None, Some("01a1cbc4-0000-7963-b5ee-a33180afa3db")).is_err());
        let bytes = build_snapshot(runner_id, &root, &claude, Some(&codex), None, Some(THREAD)).unwrap();
        let snapshot = decode(&bytes);
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot.files.iter().all(|file| !file.path.contains("auth") && !file.path.contains("config")));
        assert!(restore_snapshot(&bytes, runner_id, &root, &claude, None, None).is_err());
        fs::remove_dir_all(codex.join("sessions")).unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, Some(&codex), None).unwrap();
        assert_eq!(fs::read(day.join(&rollout)).unwrap(), b"{\"type\":\"session_meta\"}\n");
        assert!(!day.join("notes.txt").exists());
        assert_eq!(fs::read(codex.join("auth.json")).unwrap(), b"secret");
        assert_eq!(crate::codex::find_rollout(&codex.join("sessions"), THREAD), Some(fs::canonicalize(day.join(&rollout)).unwrap()));
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn prune_drops_stale_codex_rollouts_of_dead_threads() {
        let (root, claude) = temp_paths();
        let codex = root.parent().unwrap().join("codex/sessions");
        let old_day = codex.join("2026/05/01");
        let day = codex.join("2026/06/11");
        fs::create_dir_all(&old_day).unwrap();
        fs::create_dir_all(&day).unwrap();
        fs::write(old_day.join(format!("rollout-2026-05-01T00-00-00-{OLD_THREAD}.jsonl")), b"{}\n").unwrap();
        fs::write(day.join(format!("rollout-2026-06-11T23-30-17-{THREAD}.jsonl")), b"{}\n").unwrap();
        let live = HashSet::from([THREAD.to_string()]);
        let future = SystemTime::now() + Duration::from_secs(60);
        let pruned = prune_sessions(&claude.join("projects"), &root.join(OMP_SESSIONS), Some(&codex), None, &live, future).unwrap();
        assert_eq!(pruned, 1);
        assert!(!old_day.exists());
        assert!(day.join(format!("rollout-2026-06-11T23-30-17-{THREAD}.jsonl")).exists());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn reads_thread_ids_from_rollout_names() {
        assert_eq!(crate::codex::rollout_thread_id(&format!("rollout-2026-06-11T23-30-17-{THREAD}.jsonl")), Some(THREAD));
        assert_eq!(crate::codex::rollout_thread_id("rollout-short.jsonl"), None);
        assert_eq!(crate::codex::rollout_thread_id(&format!("other-{THREAD}.jsonl")), None);
        assert_eq!(crate::codex::rollout_thread_id(&format!("rollout-x-{THREAD}.json")), None);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let (root, claude) = temp_paths();
        fs::create_dir_all(root.join("memory")).unwrap();
        fs::write(root.join("memory/kept.md"), b"kept").unwrap();
        symlink("/etc", root.join("memory/link")).unwrap();
        let runner_id = Uuid::new_v4();
        let bytes = build_snapshot(runner_id, &root, &claude, None, None, None).unwrap();
        fs::remove_dir_all(root.join("memory")).unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, None, None).unwrap();
        assert_eq!(fs::read(root.join("memory/kept.md")).unwrap(), b"kept");
        assert!(fs::symlink_metadata(root.join("memory/link")).is_err());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restore_replaces_a_symlinked_target() {
        use std::os::unix::fs::symlink;

        let (root, claude) = temp_paths();
        let runner_id = Uuid::new_v4();
        let bytes = build_snapshot(runner_id, &root, &claude, None, None, None).unwrap();
        let outside = root.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, claude.join("projects")).unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, None, None).unwrap();
        assert!(fs::symlink_metadata(claude.join("projects")).unwrap().is_dir());
        assert!(outside.is_dir());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    fn decode(bytes: &[u8]) -> Snapshot {
        let mut decoder = GzDecoder::new(bytes);
        let mut json = Vec::new();
        decoder.read_to_end(&mut json).unwrap();
        serde_json::from_slice(&json).unwrap()
    }

    fn encode(snapshot: &Snapshot) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&serde_json::to_vec(snapshot).unwrap())
            .unwrap();
        encoder.finish().unwrap()
    }

    const CHAT: &str = "53fc29f7-54ad-4a4c-a440-ce6926cae77f";
    const OLD_CHAT: &str = "11111111-2222-3333-4444-555555555555";
    const WORKSPACE: &str = "b6d8b3b973db249414bb799527be54eb";

    #[test]
    fn snapshot_round_trips_cursor_chats_only() {
        let (root, claude) = temp_paths();
        let cursor = root.parent().unwrap().join("cursor");
        let chat = cursor.join("chats").join(WORKSPACE).join(CHAT);
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("store.db"), b"db").unwrap();
        fs::write(chat.join("store.db-wal"), b"wal").unwrap();
        fs::write(chat.join("store.db-shm"), b"shm").unwrap();
        fs::write(chat.join("meta.json"), b"{}").unwrap();
        fs::write(chat.join("notes.txt"), b"skip").unwrap();
        fs::create_dir_all(cursor.join("acp-sessions").join(CHAT)).unwrap();
        fs::write(cursor.join("acp-sessions").join(CHAT).join("store.db"), b"acp").unwrap();
        fs::write(cursor.join("cli-config.json"), b"secret").unwrap();
        fs::create_dir_all(cursor.join("chats/not-a-hash").join(CHAT)).unwrap();
        fs::write(cursor.join("chats/not-a-hash").join(CHAT).join("store.db"), b"x").unwrap();
        let runner_id = Uuid::new_v4();
        assert!(build_snapshot(runner_id, &root, &claude, None, Some(&cursor), Some(OLD_CHAT)).is_err());
        let bytes = build_snapshot(runner_id, &root, &claude, None, Some(&cursor), Some(CHAT)).unwrap();
        let snapshot = decode(&bytes);
        let mut names: Vec<&str> = snapshot.files.iter().map(|file| file.path.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                format!("{WORKSPACE}/{CHAT}/meta.json").as_str(),
                format!("{WORKSPACE}/{CHAT}/store.db").as_str(),
                format!("{WORKSPACE}/{CHAT}/store.db-wal").as_str(),
            ]
        );
        assert!(restore_snapshot(&bytes, runner_id, &root, &claude, None, None).is_err());
        fs::remove_dir_all(cursor.join("chats")).unwrap();
        restore_snapshot(&bytes, runner_id, &root, &claude, None, Some(&cursor)).unwrap();
        assert_eq!(fs::read(chat.join("store.db")).unwrap(), b"db");
        assert_eq!(fs::read(chat.join("store.db-wal")).unwrap(), b"wal");
        assert!(!chat.join("store.db-shm").exists());
        assert!(!chat.join("notes.txt").exists());
        assert_eq!(fs::read(cursor.join("cli-config.json")).unwrap(), b"secret");
        assert_eq!(fs::read(cursor.join("acp-sessions").join(CHAT).join("store.db")).unwrap(), b"acp");
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn cursor_paths_must_be_a_chat_file() {
        assert_eq!(cursor_chat_id(Path::new(&format!("{WORKSPACE}/{CHAT}/store.db"))), Some(CHAT));
        assert_eq!(cursor_chat_id(Path::new(&format!("{WORKSPACE}/{CHAT}/store.db-shm"))), None);
        assert_eq!(cursor_chat_id(Path::new(&format!("{WORKSPACE}/{CHAT}"))), None);
        assert_eq!(cursor_chat_id(Path::new(&format!("{WORKSPACE}/{CHAT}/x/store.db"))), None);
        assert_eq!(cursor_chat_id(Path::new(&format!("nothex/{CHAT}/store.db"))), None);
        assert_eq!(cursor_chat_id(Path::new(&format!("{WORKSPACE}/a b/store.db"))), None);
    }

    #[test]
    fn prune_drops_stale_cursor_chats_of_dead_sessions() {
        let (root, claude) = temp_paths();
        let chats = root.parent().unwrap().join("cursor/chats");
        let live_chat = chats.join(WORKSPACE).join(CHAT);
        let old_workspace = chats.join("0123456789abcdef0123456789abcdef");
        let dead_chat = old_workspace.join(OLD_CHAT);
        fs::create_dir_all(&live_chat).unwrap();
        fs::create_dir_all(&dead_chat).unwrap();
        fs::write(live_chat.join("store.db"), b"db").unwrap();
        fs::write(dead_chat.join("store.db"), b"db").unwrap();
        let live = HashSet::from([CHAT.to_string()]);
        let past = SystemTime::now() - Duration::from_secs(3600);
        assert_eq!(prune_sessions(&claude.join("projects"), &root.join(OMP_SESSIONS), None, Some(&chats), &live, past).unwrap(), 0);
        let future = SystemTime::now() + Duration::from_secs(60);
        assert_eq!(prune_sessions(&claude.join("projects"), &root.join(OMP_SESSIONS), None, Some(&chats), &live, future).unwrap(), 1);
        assert!(!old_workspace.exists());
        assert!(live_chat.join("store.db").exists());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn finds_cursor_chats_only_when_their_dir_is_kept() {
        let (root, claude) = temp_paths();
        let cursor = root.parent().unwrap().join("cursor");
        let chat = cursor.join("chats").join(WORKSPACE).join(CHAT);
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("store.db"), b"db").unwrap();
        let mut storage = RunnerStorage::new(
            "http://127.0.0.1:9/",
            "ap-southeast-1",
            "test-bucket",
            Auth::Keys(Credentials::new("access", "secret")),
            Uuid::new_v4(),
            root.clone(),
            claude,
            true,
        )
        .unwrap();
        assert!(!storage.keeps_sessions_of("cursor"));
        assert!(storage.has_transcript("cursor", OLD_CHAT));
        storage.cursor_dir = Some(cursor);
        assert!(storage.keeps_sessions_of("cursor"));
        assert!(storage.has_transcript("cursor", CHAT));
        assert!(!storage.has_transcript("cursor", OLD_CHAT));
        assert!(!storage.keeps_sessions_of("opencode"));
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
