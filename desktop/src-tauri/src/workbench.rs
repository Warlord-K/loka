use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use notify::{RecursiveMode, Watcher};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{Manager, WebviewWindow};

use crate::native::{require_trusted, workspaces_root};

const READ_CAP: u64 = 5 * 1024 * 1024;
const SCROLLBACK: usize = 256 * 1024;
const FIND_CAP: usize = 500;
const FILES_CAP: usize = 50_000;
const TERMS_PER_KEY: usize = 8;
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub struct Workbench {
    watchers: Mutex<HashMap<String, notify::RecommendedWatcher>>,
    terms: Mutex<HashMap<String, Term>>,
}

pub struct Sink<T> {
    send: Box<dyn Fn(T) -> bool + Send + Sync>,
}

impl<T> Sink<T> {
    pub fn new(send: impl Fn(T) -> bool + Send + Sync + 'static) -> Self {
        Self { send: Box::new(send) }
    }

    fn send(&self, value: T) -> bool {
        (self.send)(value)
    }
}

impl<T: tauri::ipc::IpcResponse + Send + Sync + 'static> From<Channel<T>> for Sink<T> {
    fn from(channel: Channel<T>) -> Self {
        Sink::new(move |value| channel.send(value).is_ok())
    }
}

impl Workbench {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
            terms: Mutex::new(HashMap::new()),
        }
    }

    pub fn attach_term(&self, key: &str, root: &Path, cols: u16, rows: u16, id: Option<String>, sink: Sink<TermEvent>) -> Result<TermInfo, String> {
        let mut terms = self.terms.lock().map_err(err)?;
        if let Some(term) = id.as_ref().and_then(|id| terms.get_mut(id)).filter(|term| term.key == key) {
            let mut buffers = term.shared.lock().map_err(err)?;
            buffers.pending.clear();
            buffers.carry.clear();
            let replay = ring_text(&buffers.ring);
            if !replay.is_empty() {
                sink.send(TermEvent::Data { data: replay });
            }
            if let Some(code) = buffers.exit {
                sink.send(TermEvent::Exit { code });
            }
            buffers.channel = Some(sink);
            drop(buffers);
            let _ = term.master.resize(PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 });
            let id = id.unwrap_or_default();
            let term = &terms[&id];
            return Ok(TermInfo { id, key: term.key.clone(), title: term.title.clone(), exited: !term.alive.load(Ordering::SeqCst) });
        }
        let count = terms.values().filter(|term| term.key == key).count();
        if count >= TERMS_PER_KEY {
            return Err("Close a terminal first; this workspace has 8 open.".into());
        }
        let term = spawn_term(key, root, cols, rows, count, sink)?;
        let id = uuid::Uuid::new_v4().to_string();
        let info = TermInfo { id: id.clone(), key: term.key.clone(), title: term.title.clone(), exited: false };
        terms.insert(id, term);
        Ok(info)
    }

    pub fn list_terms(&self, key: &str) -> Result<Vec<TermInfo>, String> {
        let terms = self.terms.lock().map_err(err)?;
        let mut out = terms
            .iter()
            .filter(|(_, term)| term.key == key)
            .map(|(id, term)| TermInfo { id: id.clone(), key: term.key.clone(), title: term.title.clone(), exited: !term.alive.load(Ordering::SeqCst) })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(out)
    }

    pub fn write_term(&self, id: &str, data: &str) -> Result<(), String> {
        let mut terms = self.terms.lock().map_err(err)?;
        let term = terms.get_mut(id).ok_or_else(|| "That terminal is closed.".to_string())?;
        term.writer.write_all(data.as_bytes()).map_err(err)?;
        term.writer.flush().map_err(err)
    }

    pub fn resize_term(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let terms = self.terms.lock().map_err(err)?;
        let term = terms.get(id).ok_or_else(|| "That terminal is closed.".to_string())?;
        term.master
            .resize(PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 })
            .map_err(err)
    }

    pub fn close_term(&self, id: &str) -> Result<(), String> {
        let mut terms = self.terms.lock().map_err(err)?;
        if let Some(mut term) = terms.remove(id) {
            let _ = term.killer.kill();
            if let Ok(mut buffers) = term.shared.lock() {
                buffers.channel = None;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenInfo {
    key: String,
    root: String,
    name: String,
    is_git: bool,
    branch: Option<String>,
    base: Option<String>,
    head: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Entry {
    name: String,
    path: String,
    is_dir: bool,
    is_symlink: bool,
}

#[derive(Debug, Serialize)]
pub struct FileText {
    text: Option<String>,
    sha: String,
    size: u64,
    binary: bool,
    too_large: bool,
}

#[derive(Debug, Serialize)]
pub struct WriteResult {
    saved: bool,
    conflict: bool,
    sha: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone)]
pub struct Change {
    path: String,
    status: String,
    old_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Changes {
    base: Option<String>,
    merge_base: Option<String>,
    files: Vec<Change>,
}

#[derive(Debug, Serialize)]
pub struct BaseText {
    exists: bool,
    text: Option<String>,
    binary: bool,
}

#[derive(Debug, Serialize)]
pub struct FindMatch {
    path: String,
    line: u64,
    column: u64,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchBatch {
    paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TermEvent {
    Data { data: String },
    Exit { code: Option<u32> },
}

#[derive(Debug, Serialize)]
pub struct TermInfo {
    id: String,
    key: String,
    title: String,
    exited: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindMode {
    Text,
    Regex,
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn checked_components(value: &str, what: &str) -> Result<Vec<String>, String> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.contains('\\') || value.contains('\0') || value.starts_with('/') {
        return Err(format!("Invalid {what}."));
    }
    let parts = value.split('/').map(str::to_string).collect::<Vec<_>>();
    if parts.iter().any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(format!("Invalid {what}."));
    }
    Ok(parts)
}

fn inside(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub fn resolve_session(workspaces: &Path, key: &str) -> Result<(String, PathBuf), String> {
    if let Some(folder) = crate::folders::resolve(workspaces, key) {
        return folder;
    }
    let parts = checked_components(key, "session")?;
    if parts.is_empty() {
        return Err("Invalid session.".into());
    }
    let workspaces = fs::canonicalize(workspaces).map_err(|_| "This Mac has no runner workspaces yet.".to_string())?;
    let target = workspaces.join(parts.join("/"));
    let canonical = fs::canonicalize(&target).map_err(|_| "This session's files are not on this Mac.".to_string())?;
    if !inside(&workspaces, &canonical) || canonical == workspaces || !canonical.is_dir() {
        return Err("Invalid session.".into());
    }
    let mut key = parts.join("/");
    let mut root = canonical;
    if !root.join(".git").exists() {
        let dirs = fs::read_dir(&root)
            .map_err(err)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .collect::<Vec<_>>();
        let files = fs::read_dir(&root).map_err(err)?.count();
        if dirs.len() == 1 && files == 1 && dirs[0].path().join(".git").exists() {
            let child = fs::canonicalize(dirs[0].path()).map_err(err)?;
            if inside(&workspaces, &child) {
                key = format!("{key}/{}", dirs[0].file_name().to_string_lossy());
                root = child;
            }
        }
    }
    Ok((key, root))
}

pub fn resolve_in(root: &Path, rel: &str, for_write: bool) -> Result<PathBuf, String> {
    let parts = checked_components(rel, "path")?;
    if for_write && parts.iter().any(|part| part == ".git") {
        return Err("Git internals cannot be edited here.".into());
    }
    if parts.is_empty() {
        return Ok(root.to_path_buf());
    }
    let joined = root.join(parts.join("/"));
    if fs::symlink_metadata(&joined).is_ok() {
        let canonical = fs::canonicalize(&joined).map_err(|_| "That link points nowhere.".to_string())?;
        if !inside(root, &canonical) {
            return Err("That path leads outside this workspace.".into());
        }
        return Ok(canonical);
    }
    let parent = joined.parent().ok_or_else(|| "Invalid path.".to_string())?;
    let parent = fs::canonicalize(parent).map_err(|_| "The folder does not exist.".to_string())?;
    if !inside(root, &parent) {
        return Err("That path leads outside this workspace.".into());
    }
    Ok(parent.join(joined.file_name().ok_or_else(|| "Invalid path.".to_string())?))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .map_err(err)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn git_line(root: &Path, args: &[&str]) -> Option<String> {
    git(root, args)
        .ok()
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .filter(|line| !line.is_empty())
}

fn valid_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn is_git(root: &Path) -> bool {
    git_line(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

fn detect_base(root: &Path, base: Option<&str>) -> Option<String> {
    if let Some(base) = base.filter(|base| valid_ref(base)) {
        return Some(base.to_string());
    }
    git_line(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|value| value.strip_prefix("origin/").map(str::to_string))
        .or_else(|| Some("main".to_string()))
}

pub fn open_info(workspaces: &Path, key: &str, base: Option<&str>) -> Result<OpenInfo, String> {
    let (key, root) = resolve_session(workspaces, key)?;
    let git_repo = is_git(&root);
    Ok(OpenInfo {
        name: root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        root: root.to_string_lossy().to_string(),
        is_git: git_repo,
        branch: git_repo.then(|| git_line(&root, &["rev-parse", "--abbrev-ref", "HEAD"])).flatten(),
        head: git_repo.then(|| git_line(&root, &["rev-parse", "--short", "HEAD"])).flatten(),
        base: git_repo.then(|| detect_base(&root, base)).flatten(),
        key,
    })
}

pub fn list_dir(root: &Path, dir: &str) -> Result<Vec<Entry>, String> {
    let target = resolve_in(root, dir, false)?;
    if !target.is_dir() {
        return Err("Not a folder.".into());
    }
    let mut entries = Vec::new();
    let walker = WalkBuilder::new(&target)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .parents(true)
        .build();
    for item in walker.filter_map(Result::ok) {
        if item.depth() == 0 {
            continue;
        }
        let name = item.file_name().to_string_lossy().to_string();
        if name == ".git" || name == ".DS_Store" {
            continue;
        }
        let is_symlink = item.path_is_symlink();
        let is_dir = fs::metadata(item.path()).is_ok_and(|meta| meta.is_dir());
        entries.push(Entry {
            path: relative(root, item.path()),
            name,
            is_dir,
            is_symlink,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

pub fn all_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for item in walker.filter_map(Result::ok) {
        if item.file_type().is_some_and(|kind| kind.is_file()) {
            out.push(relative(root, item.path()));
            if out.len() >= FILES_CAP {
                break;
            }
        }
    }
    out.sort();
    out
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0) || std::str::from_utf8(bytes).is_err()
}

pub fn read_file(root: &Path, path: &str) -> Result<FileText, String> {
    let target = resolve_in(root, path, false)?;
    let meta = fs::metadata(&target).map_err(|_| "That file is gone.".to_string())?;
    if !meta.is_file() {
        return Err("Not a file.".into());
    }
    if meta.len() > READ_CAP {
        return Ok(FileText { text: None, sha: String::new(), size: meta.len(), binary: false, too_large: true });
    }
    let bytes = fs::read(&target).map_err(err)?;
    let binary = looks_binary(&bytes);
    Ok(FileText {
        sha: sha_hex(&bytes),
        size: bytes.len() as u64,
        text: (!binary).then(|| String::from_utf8_lossy(&bytes).to_string()),
        binary,
        too_large: false,
    })
}

fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = target.parent().ok_or_else(|| "Invalid path.".to_string())?;
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.wb-{}", uuid::Uuid::new_v4().simple()));
    let permissions = fs::metadata(target).ok().map(|meta| meta.permissions());
    let result = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temp, permissions)?;
        }
        fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(err)
}

pub fn write_file(root: &Path, path: &str, text: &str, base_sha: Option<&str>) -> Result<WriteResult, String> {
    let target = resolve_in(root, path, true)?;
    let current = match fs::read(&target) {
        Ok(bytes) => Some(sha_hex(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(err(error)),
    };
    if current.as_deref() != base_sha.filter(|sha| !sha.is_empty()) {
        return Ok(WriteResult { saved: false, conflict: true, sha: current });
    }
    atomic_write(&target, text.as_bytes())?;
    Ok(WriteResult { saved: true, conflict: false, sha: Some(sha_hex(text.as_bytes())) })
}

pub fn create_entry(root: &Path, path: &str, dir: bool) -> Result<(), String> {
    let target = resolve_in(root, path, true)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err("Something with that name already exists.".into());
    }
    if dir {
        fs::create_dir(&target).map_err(err)
    } else {
        fs::OpenOptions::new().write(true).create_new(true).open(&target).map(|_| ()).map_err(err)
    }
}

pub fn rename_entry(root: &Path, from: &str, to: &str) -> Result<(), String> {
    let source = resolve_in(root, from, true)?;
    let target = resolve_in(root, to, true)?;
    if source == root {
        return Err("The workspace itself cannot be renamed.".into());
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err("Something with that name already exists.".into());
    }
    fs::rename(source, target).map_err(err)
}

pub fn delete_entry(root: &Path, path: &str) -> Result<(), String> {
    let parts = checked_components(path, "path")?;
    if parts.is_empty() || parts.iter().any(|part| part == ".git") {
        return Err("That cannot be deleted here.".into());
    }
    let joined = root.join(parts.join("/"));
    let parent = fs::canonicalize(joined.parent().ok_or_else(|| "Invalid path.".to_string())?).map_err(err)?;
    if !inside(root, &parent) {
        return Err("That path leads outside this workspace.".into());
    }
    let target = parent.join(joined.file_name().ok_or_else(|| "Invalid path.".to_string())?);
    let meta = fs::symlink_metadata(&target).map_err(|_| "That is already gone.".to_string())?;
    if meta.is_dir() {
        fs::remove_dir_all(&target).map_err(err)
    } else {
        fs::remove_file(&target).map_err(err)
    }
}

fn merge_base(root: &Path, base: Option<&str>) -> Option<String> {
    if let Some(base) = base.filter(|base| valid_ref(base)) {
        for candidate in [format!("origin/{base}"), base.to_string()] {
            if let Some(found) = git_line(root, &["merge-base", "HEAD", &candidate]) {
                return Some(found);
            }
        }
    }
    git_line(root, &["rev-parse", "--verify", "-q", "HEAD"]).or_else(|| Some(EMPTY_TREE.to_string()))
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect()
}

pub fn changes(root: &Path, base: Option<&str>) -> Result<Changes, String> {
    if !is_git(root) {
        return Ok(Changes { base: None, merge_base: None, files: Vec::new() });
    }
    let base = detect_base(root, base);
    let mb = merge_base(root, base.as_deref()).ok_or_else(|| "Could not find a base commit.".to_string())?;
    let diff = git(root, &["diff", "--name-status", "-z", "-M", &mb, "--"])?;
    let tokens = split_z(&diff);
    let mut files = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let status = tokens[i].clone();
        let letter = status.chars().next().unwrap_or('M');
        if matches!(letter, 'R' | 'C') && i + 2 < tokens.len() {
            files.push(Change { status: letter.to_string(), old_path: Some(tokens[i + 1].clone()), path: tokens[i + 2].clone() });
            i += 3;
        } else if i + 1 < tokens.len() {
            files.push(Change { status: letter.to_string(), old_path: None, path: tokens[i + 1].clone() });
            i += 2;
        } else {
            break;
        }
    }
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let seen = files.iter().map(|f| f.path.clone()).collect::<BTreeSet<_>>();
    for path in split_z(&untracked) {
        if !seen.contains(&path) {
            files.push(Change { status: "U".into(), old_path: None, path });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Changes { base, merge_base: Some(mb), files })
}

pub fn base_text(root: &Path, path: &str, base: Option<&str>) -> Result<BaseText, String> {
    let parts = checked_components(path, "path")?;
    if parts.is_empty() {
        return Err("Invalid path.".into());
    }
    if !is_git(root) {
        return Ok(BaseText { exists: false, text: None, binary: false });
    }
    let base = detect_base(root, base);
    let mb = merge_base(root, base.as_deref()).ok_or_else(|| "Could not find a base commit.".to_string())?;
    if mb == EMPTY_TREE {
        return Ok(BaseText { exists: false, text: None, binary: false });
    }
    match git(root, &["show", &format!("{mb}:./{}", parts.join("/"))]) {
        Ok(bytes) => {
            let binary = looks_binary(&bytes);
            Ok(BaseText { exists: true, text: (!binary).then(|| String::from_utf8_lossy(&bytes).to_string()), binary })
        }
        Err(_) => Ok(BaseText { exists: false, text: None, binary: false }),
    }
}

fn rg_find(root: &Path, query: &str, regex: bool) -> Option<Vec<FindMatch>> {
    let mut command = Command::new("rg");
    command.current_dir(root).args(["--json", "--smart-case", "--max-columns", "400", "--max-count", "50"]);
    if !regex {
        command.arg("--fixed-strings");
    }
    let output = command.arg("--").arg(query).arg(".").output().ok()?;
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    let mut out = Vec::new();
    for line in output.stdout.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else { continue };
        if value["type"] != "match" {
            continue;
        }
        let data = &value["data"];
        let path = data["path"]["text"].as_str().unwrap_or_default().trim_start_matches("./").to_string();
        out.push(FindMatch {
            path,
            line: data["line_number"].as_u64().unwrap_or(0),
            column: data["submatches"][0]["start"].as_u64().unwrap_or(0) + 1,
            text: data["lines"]["text"].as_str().unwrap_or_default().trim_end().chars().take(400).collect(),
        });
        if out.len() >= FIND_CAP {
            break;
        }
    }
    Some(out)
}

fn walk_find(root: &Path, query: &str) -> Vec<FindMatch> {
    let needle = query.to_lowercase();
    let fold = query == needle;
    let mut out = Vec::new();
    for path in all_files(root) {
        let Ok(meta) = fs::metadata(root.join(&path)) else { continue };
        if meta.len() > 1024 * 1024 {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(&path)) else { continue };
        for (index, line) in text.lines().enumerate() {
            let hay = if fold { line.to_lowercase() } else { line.to_string() };
            if let Some(column) = hay.find(if fold { needle.as_str() } else { query }) {
                out.push(FindMatch { path: path.clone(), line: index as u64 + 1, column: column as u64 + 1, text: line.chars().take(400).collect() });
                if out.len() >= FIND_CAP {
                    return out;
                }
            }
        }
    }
    out
}

pub fn find(root: &Path, query: &str, mode: FindMode) -> Vec<FindMatch> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    rg_find(root, query, matches!(mode, FindMode::Regex)).unwrap_or_else(|| walk_find(root, query))
}

fn root_ignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    builder.add(root.join(".gitignore"));
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn watch_relevant(root: &Path, ignore: &Gitignore, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.components().any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".DS_Store") {
        return None;
    }
    let name = rel.file_name()?.to_string_lossy();
    if name.contains(".wb-") {
        return None;
    }
    if ignore.matched_path_or_any_parents(rel, path.is_dir()).is_ignore() {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn start_watch(root: PathBuf, channel: Sink<WatchBatch>) -> Result<notify::RecommendedWatcher, String> {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })
    .map_err(err)?;
    watcher.watch(&root, RecursiveMode::Recursive).map_err(err)?;
    std::thread::spawn(move || {
        let ignore = root_ignore(&root);
        while let Ok(first) = rx.recv() {
            let mut paths = BTreeSet::new();
            let mut collect = |event: notify::Result<notify::Event>| {
                if let Ok(event) = event {
                    for path in event.paths {
                        if let Some(rel) = watch_relevant(&root, &ignore, &path) {
                            paths.insert(rel);
                        }
                    }
                }
            };
            collect(first);
            let deadline = Instant::now() + Duration::from_millis(150);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(left) {
                    Ok(event) => collect(event),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            if !paths.is_empty() && !channel.send(WatchBatch { paths: paths.into_iter().collect() }) {
                return;
            }
        }
    });
    Ok(watcher)
}

struct TermBuffers {
    ring: Vec<u8>,
    pending: Vec<u8>,
    carry: Vec<u8>,
    channel: Option<Sink<TermEvent>>,
    exit: Option<Option<u32>>,
    exit_sent: bool,
}

struct Term {
    key: String,
    title: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    shared: Arc<Mutex<TermBuffers>>,
    alive: Arc<AtomicBool>,
}

fn decode(carry: &mut Vec<u8>, mut bytes: Vec<u8>) -> String {
    if !carry.is_empty() {
        let mut joined = std::mem::take(carry);
        joined.append(&mut bytes);
        bytes = joined;
    }
    match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() => {
            let valid = error.valid_up_to();
            *carry = bytes[valid..].to_vec();
            String::from_utf8_lossy(&bytes[..valid]).to_string()
        }
        Err(_) => String::from_utf8_lossy(&bytes).to_string(),
    }
}

fn push_ring(ring: &mut Vec<u8>, bytes: &[u8]) {
    ring.extend_from_slice(bytes);
    if ring.len() > SCROLLBACK {
        let cut = ring.len() - SCROLLBACK;
        ring.drain(..cut);
    }
}

fn ring_text(ring: &[u8]) -> String {
    let start = ring.iter().position(|b| b & 0b1100_0000 != 0b1000_0000).unwrap_or(ring.len());
    String::from_utf8_lossy(&ring[start..]).to_string()
}

pub(crate) fn login_shell() -> String {
    let from_env = std::env::var("SHELL").ok();
    let candidates = if cfg!(target_os = "macos") { [account_shell(), from_env] } else { [from_env, account_shell()] };
    candidates
        .into_iter()
        .flatten()
        .find(|shell| usable_shell(shell))
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".to_string() } else { "/bin/bash".to_string() })
}

fn usable_shell(shell: &str) -> bool {
    let path = std::path::Path::new(shell);
    path.is_absolute()
        && path.is_file()
        && !matches!(path.file_name().and_then(|name| name.to_str()), Some("nologin" | "false" | "true" | "sh"))
}

#[cfg(unix)]
fn account_shell() -> Option<String> {
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let code = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
    if code != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) }.to_str().ok().map(str::to_string)
}

#[cfg(not(unix))]
fn account_shell() -> Option<String> {
    None
}

fn spawn_term(key: &str, root: &Path, cols: u16, rows: u16, index: usize, channel: Sink<TermEvent>) -> Result<Term, String> {
    let pair = native_pty_system()
        .openpty(PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 })
        .map_err(err)?;
    let shell = login_shell();
    let mut command = CommandBuilder::new(&shell);
    command.args(["-l", "-i"]);
    command.cwd(root);
    command.env("SHELL", &shell);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "Loka");
    for key in ["WORLD_SLACK_CONNECTION_KEY", "WORLD_SLACK_KEY_FILE", "WORLD_RUNNER_LEASE_FILE"] {
        command.env_remove(key);
    }
    let mut child = pair.slave.spawn_command(command).map_err(err)?;
    drop(pair.slave);
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(err)?;
    let writer = pair.master.take_writer().map_err(err)?;
    let shared = Arc::new(Mutex::new(TermBuffers {
        ring: Vec::new(),
        pending: Vec::new(),
        carry: Vec::new(),
        channel: Some(channel),
        exit: None,
        exit_sent: false,
    }));
    let alive = Arc::new(AtomicBool::new(true));
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 32 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut state) = shared.lock() {
                            push_ring(&mut state.ring, &buffer[..n]);
                            state.pending.extend_from_slice(&buffer[..n]);
                        }
                    }
                }
            }
        });
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(mut state) = shared.lock() {
                state.exit = Some(code);
            }
        });
    }
    {
        let shared = shared.clone();
        let alive = alive.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(8));
            let Ok(mut state) = shared.lock() else { return };
            if !state.pending.is_empty() {
                let bytes = std::mem::take(&mut state.pending);
                let text = {
                    let carry = &mut state.carry;
                    decode(carry, bytes)
                };
                if let Some(channel) = state.channel.as_ref() {
                    if !text.is_empty() && !channel.send(TermEvent::Data { data: text }) {
                        state.channel = None;
                    }
                }
                continue;
            }
            if let Some(code) = state.exit {
                if !state.exit_sent {
                    if let Some(channel) = state.channel.as_ref() {
                        channel.send(TermEvent::Exit { code });
                    }
                    state.exit_sent = true;
                }
                alive.store(false, Ordering::SeqCst);
                return;
            }
        });
    }
    let shell_name = Path::new(&shell).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "shell".into());
    Ok(Term {
        key: key.to_string(),
        title: if index == 0 { shell_name } else { format!("{shell_name} {}", index + 1) },
        master: pair.master,
        writer,
        killer,
        shared,
        alive,
    })
}

fn state(window: &WebviewWindow) -> tauri::State<'_, Arc<Workbench>> {
    window.state::<Arc<Workbench>>()
}

async fn guard(window: &WebviewWindow) -> Result<PathBuf, String> {
    require_trusted(window)?;
    workspaces_root(window)
}

async fn session_root(window: &WebviewWindow, key: &str) -> Result<PathBuf, String> {
    let workspaces = guard(window).await?;
    resolve_session(&workspaces, key).map(|(_, root)| root)
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(err)?
}

#[tauri::command]
pub async fn wb_open(window: WebviewWindow, session_key: String, base: Option<String>) -> Result<OpenInfo, String> {
    let workspaces = guard(&window).await?;
    blocking(move || open_info(&workspaces, &session_key, base.as_deref())).await
}

#[tauri::command]
pub async fn wb_list(window: WebviewWindow, session_key: String, dir: String) -> Result<Vec<Entry>, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || list_dir(&root, &dir)).await
}

#[tauri::command]
pub async fn wb_files(window: WebviewWindow, session_key: String) -> Result<Vec<String>, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || Ok(all_files(&root))).await
}

#[tauri::command]
pub async fn wb_read(window: WebviewWindow, session_key: String, path: String) -> Result<FileText, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || read_file(&root, &path)).await
}

#[tauri::command]
pub async fn wb_write(window: WebviewWindow, session_key: String, path: String, text: String, base_sha: Option<String>) -> Result<WriteResult, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || write_file(&root, &path, &text, base_sha.as_deref())).await
}

#[tauri::command]
pub async fn wb_create(window: WebviewWindow, session_key: String, path: String, dir: bool) -> Result<(), String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || create_entry(&root, &path, dir)).await
}

#[tauri::command]
pub async fn wb_rename(window: WebviewWindow, session_key: String, from: String, to: String) -> Result<(), String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || rename_entry(&root, &from, &to)).await
}

#[tauri::command]
pub async fn wb_delete(window: WebviewWindow, session_key: String, path: String) -> Result<(), String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || delete_entry(&root, &path)).await
}

#[tauri::command]
pub async fn wb_changes(window: WebviewWindow, session_key: String, base: Option<String>) -> Result<Changes, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || changes(&root, base.as_deref())).await
}

#[tauri::command]
pub async fn wb_base(window: WebviewWindow, session_key: String, path: String, base: Option<String>) -> Result<BaseText, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || base_text(&root, &path, base.as_deref())).await
}

#[tauri::command]
pub async fn wb_find(window: WebviewWindow, session_key: String, query: String, mode: Option<FindMode>) -> Result<Vec<FindMatch>, String> {
    let root = session_root(&window, &session_key).await?;
    blocking(move || Ok(find(&root, &query, mode.unwrap_or(FindMode::Text)))).await
}

#[tauri::command]
pub async fn wb_watch(window: WebviewWindow, session_key: String, on_event: Channel<WatchBatch>) -> Result<(), String> {
    let root = session_root(&window, &session_key).await?;
    let watcher = start_watch(root, on_event.into())?;
    state(&window).watchers.lock().map_err(err)?.insert(session_key, watcher);
    Ok(())
}

#[tauri::command]
pub async fn wb_unwatch(window: WebviewWindow, session_key: String) -> Result<(), String> {
    require_trusted(&window)?;
    state(&window).watchers.lock().map_err(err)?.remove(&session_key);
    Ok(())
}

#[tauri::command]
pub async fn term_open(
    window: WebviewWindow,
    session_key: String,
    cols: u16,
    rows: u16,
    id: Option<String>,
    on_event: Channel<TermEvent>,
) -> Result<TermInfo, String> {
    let root = session_root(&window, &session_key).await?;
    state(&window).attach_term(&session_key, &root, cols, rows, id, on_event.into())
}

pub const HOME_TERM_KEY: &str = "~home";

#[tauri::command]
pub async fn home_term_open(
    window: WebviewWindow,
    cols: u16,
    rows: u16,
    id: Option<String>,
    on_event: Channel<TermEvent>,
) -> Result<TermInfo, String> {
    guard(&window).await?;
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir()).ok_or("Could not find your home folder.")?;
    state(&window).attach_term(HOME_TERM_KEY, &home, cols, rows, id, on_event.into())
}

#[tauri::command]
pub async fn term_list(window: WebviewWindow, session_key: String) -> Result<Vec<TermInfo>, String> {
    require_trusted(&window)?;
    state(&window).list_terms(&session_key)
}

#[tauri::command]
pub async fn term_write(window: WebviewWindow, id: String, data: String) -> Result<(), String> {
    require_trusted(&window)?;
    state(&window).write_term(&id, &data)
}

#[tauri::command]
pub async fn term_resize(window: WebviewWindow, id: String, cols: u16, rows: u16) -> Result<(), String> {
    require_trusted(&window)?;
    state(&window).resize_term(&id, cols, rows)
}

#[tauri::command]
pub async fn term_close(window: WebviewWindow, id: String) -> Result<(), String> {
    require_trusted(&window)?;
    state(&window).close_term(&id)
}

pub fn close_all(app: &tauri::AppHandle) {
    if let Some(wb) = app.try_state::<Arc<Workbench>>() {
        if let Ok(mut terms) = wb.terms.lock() {
            for (_, mut term) in terms.drain() {
                let _ = term.killer.kill();
            }
        }
        if let Ok(mut watchers) = wb.watchers.lock() {
            watchers.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn setup() -> (Temp, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("wb-test-{}", uuid::Uuid::new_v4().simple()));
        let workspaces = base.join("workspaces");
        let repo = workspaces.join("tasks").join("t1").join("app");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(base.join("outside")).unwrap();
        fs::write(base.join("outside").join("secret.txt"), "secret").unwrap();
        run(&repo, &["init", "-q"]);
        fs::write(repo.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(repo.join("gone.txt"), "bye\n").unwrap();
        fs::write(repo.join("move.txt"), "a long enough body to be detected as a rename\nline two\nline three\n").unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        run(&repo, &["checkout", "-q", "-b", "kar-1/work"]);
        let base_canon = fs::canonicalize(&base).unwrap();
        (Temp(base), base_canon.join("workspaces"), fs::canonicalize(&repo).unwrap())
    }

    #[test]
    fn sessions_resolve_only_inside_workspaces() {
        let (_t, ws, repo) = setup();
        let (key, root) = resolve_session(&ws, "tasks/t1/app").unwrap();
        assert_eq!((key.as_str(), root.as_path()), ("tasks/t1/app", repo.as_path()));
        let (key, root) = resolve_session(&ws, "tasks/t1").unwrap();
        assert_eq!((key.as_str(), root.as_path()), ("tasks/t1/app", repo.as_path()));
        for bad in ["", "/etc", "../outside", "tasks/../../outside", "tasks//t1", "tasks\\t1", "missing/one"] {
            assert!(resolve_session(&ws, bad).is_err(), "{bad} should be refused");
        }
        std::os::unix::fs::symlink(ws.parent().unwrap().join("outside"), ws.join("escape")).unwrap();
        assert!(resolve_session(&ws, "escape").is_err());
    }

    #[test]
    fn paths_cannot_escape_the_session() {
        let (_t, _ws, repo) = setup();
        for bad in ["../x", "/etc/passwd", "src/../../x", "a\\b"] {
            assert!(resolve_in(&repo, bad, false).is_err(), "{bad} should be refused");
        }
        std::os::unix::fs::symlink(repo.parent().unwrap().parent().unwrap().parent().unwrap().parent().unwrap().join("outside"), repo.join("out")).unwrap();
        assert!(resolve_in(&repo, "out/secret.txt", false).is_err());
        assert!(read_file(&repo, "out/secret.txt").is_err());
        assert!(write_file(&repo, "out/new.txt", "x", None).is_err());
        assert!(resolve_in(&repo, ".git/config", true).is_err());
        std::os::unix::fs::symlink(repo.join("a.txt"), repo.join("alias.txt")).unwrap();
        assert_eq!(read_file(&repo, "alias.txt").unwrap().text.as_deref(), Some("one\ntwo\n"));
        assert!(delete_entry(&repo, "..").is_err());
        assert!(delete_entry(&repo, "").is_err());
    }

    #[test]
    fn writes_detect_conflicts() {
        let (_t, _ws, repo) = setup();
        let opened = read_file(&repo, "a.txt").unwrap();
        fs::write(repo.join("a.txt"), "changed by agent\n").unwrap();
        let result = write_file(&repo, "a.txt", "mine\n", Some(&opened.sha)).unwrap();
        assert!(result.conflict && !result.saved);
        assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "changed by agent\n");
        let fresh = read_file(&repo, "a.txt").unwrap();
        let result = write_file(&repo, "a.txt", "mine\n", Some(&fresh.sha)).unwrap();
        assert!(result.saved);
        assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "mine\n");
        assert_eq!(result.sha.unwrap(), read_file(&repo, "a.txt").unwrap().sha);
        assert!(write_file(&repo, "new.txt", "hi", None).unwrap().saved);
        assert!(write_file(&repo, "new.txt", "again", None).unwrap().conflict);
        assert!(fs::read_dir(&repo).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".wb-")));
    }

    #[test]
    fn changes_match_git_diff() {
        let (_t, _ws, repo) = setup();
        fs::write(repo.join("a.txt"), "one\nthree\n").unwrap();
        fs::remove_file(repo.join("gone.txt")).unwrap();
        run(&repo, &["mv", "move.txt", "moved.txt"]);
        fs::write(repo.join("fresh.txt"), "new\n").unwrap();
        fs::write(repo.join("debug.log"), "ignored\n").unwrap();
        run(&repo, &["add", "a.txt"]);
        run(&repo, &["commit", "-q", "-m", "work"]);
        fs::write(repo.join("src").join("lib.rs"), "fn main() { println!(); }\n").unwrap();
        let result = changes(&repo, Some("main")).unwrap();
        let mb = run(&repo, &["merge-base", "HEAD", "main"]).trim().to_string();
        assert_eq!(result.merge_base.as_deref(), Some(mb.as_str()));
        let expected = run(&repo, &["diff", "--name-only", "-M", &mb])
            .lines()
            .map(str::to_string)
            .chain(run(&repo, &["ls-files", "--others", "--exclude-standard"]).lines().map(str::to_string))
            .collect::<BTreeSet<_>>();
        let got = result.files.iter().map(|f| f.path.clone()).collect::<BTreeSet<_>>();
        assert_eq!(got, expected);
        let status = |p: &str| result.files.iter().find(|f| f.path == p).map(|f| f.status.clone());
        assert_eq!(status("a.txt").as_deref(), Some("M"));
        assert_eq!(status("gone.txt").as_deref(), Some("D"));
        assert_eq!(status("fresh.txt").as_deref(), Some("U"));
        assert_eq!(status("src/lib.rs").as_deref(), Some("M"));
        assert!(status("debug.log").is_none());
        let renamed = result.files.iter().find(|f| f.path == "moved.txt").unwrap();
        assert_eq!((renamed.status.as_str(), renamed.old_path.as_deref()), ("R", Some("move.txt")));
        let base = base_text(&repo, "a.txt", Some("main")).unwrap();
        assert_eq!(base.text.as_deref(), Some("one\ntwo\n"));
        assert!(!base_text(&repo, "fresh.txt", Some("main")).unwrap().exists);
    }

    #[test]
    fn listing_respects_gitignore() {
        let (_t, _ws, repo) = setup();
        fs::create_dir_all(repo.join("target").join("debug")).unwrap();
        fs::write(repo.join("target").join("debug").join("bin"), "x").unwrap();
        fs::write(repo.join("trace.log"), "x").unwrap();
        fs::write(repo.join("src").join("note.log"), "x").unwrap();
        fs::write(repo.join(".env.example"), "x").unwrap();
        let names = list_dir(&repo, "").unwrap().into_iter().map(|e| e.name).collect::<Vec<_>>();
        assert_eq!(names, vec!["src", ".env.example", ".gitignore", "a.txt", "gone.txt", "move.txt"]);
        let nested = list_dir(&repo, "src").unwrap();
        assert_eq!(nested.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), vec!["src/lib.rs"]);
        let files = all_files(&repo);
        assert!(files.contains(&"src/lib.rs".to_string()) && !files.iter().any(|f| f.starts_with("target") || f.ends_with(".log")));
        let found = find(&repo, "fn main", FindMode::Text);
        assert_eq!(found.first().map(|m| (m.path.as_str(), m.line)), Some(("src/lib.rs", 1)));
        assert!(walk_find(&repo, "FN MAIN").is_empty());
        assert_eq!(walk_find(&repo, "fn main").len(), 1);
    }

    #[test]
    fn create_rename_delete_stay_inside() {
        let (_t, _ws, repo) = setup();
        create_entry(&repo, "docs", true).unwrap();
        create_entry(&repo, "docs/x.md", false).unwrap();
        assert!(create_entry(&repo, "docs/x.md", false).is_err());
        rename_entry(&repo, "docs/x.md", "docs/y.md").unwrap();
        assert!(repo.join("docs/y.md").exists());
        assert!(rename_entry(&repo, "docs/y.md", "../y.md").is_err());
        delete_entry(&repo, "docs").unwrap();
        assert!(!repo.join("docs").exists());
    }

    fn collect<T: Send + 'static>() -> (Channel<T>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                sink.lock().unwrap().push(json);
            }
            Ok(())
        });
        (channel, seen)
    }

    fn wait_for(seen: &Arc<Mutex<Vec<String>>>, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if seen.lock().unwrap().iter().any(|json| json.contains(needle)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn terminal_runs_commands_in_the_worktree_and_replays_scrollback() {
        let (_t, _ws, repo) = setup();
        let (channel, seen) = collect::<TermEvent>();
        let mut term = spawn_term("tasks/t1/app", &repo, 80, 24, 0, channel.into()).unwrap();
        term.writer.write_all(b"printf 'wb-%s\\n' ok; pwd; exit 3\n").unwrap();
        term.writer.flush().unwrap();
        assert!(wait_for(&seen, "wb-ok"), "{:?}", seen.lock().unwrap());
        assert!(wait_for(&seen, &repo.file_name().unwrap().to_string_lossy()));
        assert!(wait_for(&seen, "\"exit\""));
        assert!(seen.lock().unwrap().iter().any(|json| json.contains("\"code\":3")));
        let replay = ring_text(&term.shared.lock().unwrap().ring);
        assert!(replay.contains("wb-ok"));
        let _ = term.killer.kill();
    }

    #[test]
    fn watcher_batches_changes_and_skips_ignored_paths() {
        let (_t, _ws, repo) = setup();
        let (channel, seen) = collect::<WatchBatch>();
        let _watcher = start_watch(repo.clone(), channel.into()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        fs::write(repo.join("debug.log"), "ignored").unwrap();
        fs::write(repo.join("src").join("lib.rs"), "fn main() { 1; }\n").unwrap();
        fs::write(repo.join("a.txt"), "edited\n").unwrap();
        assert!(wait_for(&seen, "src/lib.rs"));
        assert!(wait_for(&seen, "a.txt"));
        std::thread::sleep(Duration::from_millis(400));
        assert!(seen.lock().unwrap().iter().all(|json| !json.contains("debug.log") && !json.contains(".git/")));
    }

    #[test]
    fn decoding_keeps_split_characters() {
        let mut carry = Vec::new();
        let bytes = "héllo".as_bytes().to_vec();
        let first = decode(&mut carry, bytes[..2].to_vec());
        let second = decode(&mut carry, bytes[2..].to_vec());
        assert_eq!(format!("{first}{second}"), "héllo");
        let mut ring = Vec::new();
        push_ring(&mut ring, &vec![b'a'; SCROLLBACK + 10]);
        assert_eq!(ring.len(), SCROLLBACK);
        assert_eq!(ring_text(&[0x80, b'o', b'k']), "ok");
    }
}
