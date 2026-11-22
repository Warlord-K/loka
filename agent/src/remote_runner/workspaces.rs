use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use loka_shared::runners::{ClosedTask, DiskFolder, DiskNotice};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tracing::{info, warn};
use uuid::Uuid;

use super::{disk_space, git_output, run_git, RunnerApi, LOW_DISK_BYTES};

pub(super) const TARGET_FREE_BYTES: f64 = 2.0 * LOW_DISK_BYTES;
pub(super) const LOW_DISK_RECHECK: Duration = Duration::from_secs(60);
const SCAN_INTERVAL: Duration = Duration::from_secs(1800);
const FIRST_SCAN_DELAY: Duration = Duration::from_secs(20);
const NOTICE_EVERY: Duration = Duration::from_secs(3600);
const LISTED_MIN_BYTES: u64 = 50 * 1024 * 1024;
const LISTED_MAX: usize = 100;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const FOLDERS_LISTED: usize = 5;
const FOLDER_MIN_BYTES: u64 = 100 * 1024 * 1024;
const FOLDERS_CACHED_FOR: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sweep {
    Routine,
    Pressure,
    Everything,
}

#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub dir: PathBuf,
    pub trash_root: PathBuf,
    pub task: ClosedTask,
}

#[derive(Debug, Default)]
pub(super) struct Removed {
    pub count: usize,
    pub bytes: u64,
    pub cache_bytes: u64,
    pub trash_roots: Vec<PathBuf>,
    pub cleaned: Vec<Uuid>,
}

impl Removed {
    fn absorb(&mut self, other: Removed) {
        for id in other.cleaned {
            if !self.cleaned.contains(&id) {
                self.cleaned.push(id);
                self.count += 1;
            }
        }
        self.bytes += other.bytes;
        self.cache_bytes += other.cache_bytes;
        for root in other.trash_roots {
            if !self.trash_roots.contains(&root) {
                self.trash_roots.push(root);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct QuestUse {
    id: Uuid,
    bytes: u64,
    temp: u64,
    build: u64,
}

#[derive(Debug, Clone, Default)]
struct Usage {
    bytes: u64,
    tasks: usize,
    temp_bytes: u64,
    build_bytes: u64,
    largest: Vec<QuestUse>,
}

static USAGE: Mutex<Option<Usage>> = Mutex::new(None);
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static LAST_NOTICE: Mutex<Option<Instant>> = Mutex::new(None);
static FOLDERS: Mutex<Option<(Instant, Vec<DiskFolder>)>> = Mutex::new(None);
static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) fn low_disk(root: &Path) -> bool {
    disk_space(root).is_some_and(|(free, _)| free < LOW_DISK_BYTES)
}

pub(super) fn mode_for(root: &Path, requested: bool) -> Sweep {
    if requested {
        Sweep::Everything
    } else if low_disk(root) {
        Sweep::Pressure
    } else {
        Sweep::Routine
    }
}

fn enough(mode: Sweep, free: f64, freed: u64) -> bool {
    mode == Sweep::Pressure && free + freed as f64 >= TARGET_FREE_BYTES
}

pub(super) fn view() -> Value {
    let Some(usage) = USAGE.lock().unwrap().clone() else {
        return Value::Null;
    };
    let (free, total) = ROOT.get().and_then(|root| disk_space(root)).unwrap_or((0.0, 0.0));
    json!({
        "bytes": usage.bytes,
        "tasks": usage.tasks,
        "temp_bytes": usage.temp_bytes,
        "build_bytes": usage.build_bytes,
        "free_gb": (free / GIB).floor(),
        "total_gb": (total / GIB).floor(),
        "low": free < LOW_DISK_BYTES,
        "largest": usage.largest.iter().map(|quest| json!({ "id": quest.id, "bytes": quest.bytes, "temp_bytes": quest.temp, "build_bytes": quest.build })).collect::<Vec<_>>(),
    })
}

pub(super) fn spawn_usage(root: PathBuf, rescan: Arc<Notify>) {
    let _ = ROOT.set(root.clone());
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_SCAN_DELAY).await;
        loop {
            let roots = vec![root.clone()];
            let scanned = tokio::task::spawn_blocking(move || scan(&roots)).await;
            if let Ok(usage) = scanned {
                *USAGE.lock().unwrap() = Some(usage);
            }
            tokio::select! {
                _ = tokio::time::sleep(SCAN_INTERVAL) => {}
                _ = rescan.notified() => {}
            }
        }
    });
}

fn quest_temp_sizes() -> std::collections::HashMap<String, u64> {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return Default::default();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(crate::cli::QUEST_TEMP_PREFIX))
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), dir_size(&entry.path())))
        .collect()
}

fn scan(roots: &[PathBuf]) -> Usage {
    let temps = quest_temp_sizes();
    let mut sizes: Vec<QuestUse> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("workspaces").join("tasks")) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() {
                let temp = temps.get(&crate::cli::quest_temp_name(id)).copied().unwrap_or(0);
                let target = quest_target_dir(id);
                let build = build_dirs_in(&entry.path()).iter().chain(std::iter::once(&target)).map(|dir| dir_size(dir)).sum();
                sizes.push(QuestUse { id, bytes: dir_size(&entry.path()) + temp, temp, build });
            }
        }
    }
    sizes.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    Usage {
        bytes: sizes.iter().map(|quest| quest.bytes).sum(),
        tasks: sizes.len(),
        temp_bytes: sizes.iter().map(|quest| quest.temp).sum(),
        build_bytes: sizes.iter().map(|quest| quest.build).sum(),
        largest: sizes.into_iter().filter(|quest| quest.bytes >= LISTED_MIN_BYTES).take(LISTED_MAX).collect(),
    }
}

const BUILD_DIRS: [&str; 3] = ["node_modules", ".next", "target"];

fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn build_dirs_under(folder: &Path) -> Vec<PathBuf> {
    BUILD_DIRS
        .iter()
        .filter(|name| **name != "target" || folder.join("Cargo.toml").is_file())
        .map(|name| folder.join(name))
        .filter(|path| real_dir(path))
        .collect()
}

const BUILD_SEARCH_DEPTH: u8 = 3;

fn build_dirs_below(folder: &Path, depth: u8, found: &mut Vec<PathBuf>) {
    found.extend(build_dirs_under(folder));
    if depth >= BUILD_SEARCH_DEPTH {
        return;
    }
    let Ok(children) = std::fs::read_dir(folder) else {
        return;
    };
    for child in children.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        if name != ".git" && !BUILD_DIRS.contains(&name.as_str()) && real_dir(&child.path()) {
            build_dirs_below(&child.path(), depth + 1, found);
        }
    }
}

pub(super) fn build_dirs_in(task_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(repos) = std::fs::read_dir(task_dir) else {
        return found;
    };
    for repo in repos.flatten().map(|entry| entry.path()).filter(|path| real_dir(path)) {
        build_dirs_below(&repo, 0, &mut found);
    }
    found
}

async fn process_listing() -> Option<String> {
    let output = tokio::process::Command::new("ps").args(["-axo", "pid=,command="]).output().await.ok()?;
    (output.status.success() && !output.stdout.is_empty()).then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn in_use(listing: &str, dir: &Path) -> bool {
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if !dirs.contains(&real) {
            dirs.push(real);
        }
    }
    !pids_under(listing, &dirs, std::process::id()).is_empty()
}

async fn remove_cache_dir(dir: &Path) -> u64 {
    let measured = dir.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || dir_size(&measured)).await.unwrap_or(0);
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => bytes,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %error, dir = %dir.display(), "Failed to clear a build cache");
            }
            0
        }
    }
}

fn quest_target_dir(id: Uuid) -> PathBuf {
    std::env::temp_dir().join(crate::cli::quest_temp_name(id)).join(crate::cli::QUEST_TARGET_DIR)
}

pub(super) async fn clear_build_dirs(quests: &[(Uuid, PathBuf)]) -> u64 {
    clear_build_dirs_with(quests, process_listing().await).await
}

async fn clear_build_dirs_with(quests: &[(Uuid, PathBuf)], listing: Option<String>) -> u64 {
    let Some(listing) = listing else {
        warn!("Could not list processes, so no build folder was cleared");
        return 0;
    };
    let mut freed = 0;
    for (id, task_dir) in quests {
        let target = quest_target_dir(*id);
        if in_use(&listing, task_dir) || in_use(&listing, &target) {
            continue;
        }
        let dirs = {
            let task_dir = task_dir.clone();
            tokio::task::spawn_blocking(move || build_dirs_in(&task_dir)).await.unwrap_or_default()
        };
        for dir in dirs.into_iter().chain(std::iter::once(target).filter(|target| real_dir(target))) {
            freed += remove_cache_dir(&dir).await;
        }
    }
    freed
}

pub(super) fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            total += meta.blocks() * 512;
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    total + std::fs::symlink_metadata(path).map(|meta| meta.blocks() * 512).unwrap_or(0)
}

async fn is_dirty(dir: &Path) -> bool {
    let Ok(mut children) = tokio::fs::read_dir(dir).await else {
        return false;
    };
    while let Ok(Some(child)) = children.next_entry().await {
        if let Ok(status) = git_output(&child.path(), &["status", "--porcelain"]).await {
            if !status.trim().is_empty() {
                return true;
            }
        }
    }
    false
}

pub(super) async fn remove_entries(entries: Vec<Entry>, mode: Sweep, free: f64) -> Removed {
    let mut removed = Removed::default();
    for entry in entries {
        if enough(mode, free, removed.bytes) {
            break;
        }
        if mode != Sweep::Routine && entry.task.status == "failed" && is_dirty(&entry.dir).await {
            info!(task = %entry.task.id, "Kept failed task workspace with unsaved changes");
            continue;
        }
        let bytes = if mode == Sweep::Routine {
            0
        } else {
            let dir = entry.dir.clone();
            tokio::task::spawn_blocking(move || dir_size(&dir)).await.unwrap_or(0)
        };
        let trash = entry.trash_root.join(entry.task.id.to_string());
        if let Err(error) = tokio::fs::create_dir_all(&entry.trash_root).await {
            warn!(error = %error, "Failed to prepare the workspace trash");
            continue;
        }
        match remove_task_workspace(&entry.dir, &trash).await {
            Ok(()) => {
                let temp = remove_quest_temp(entry.task.id).await.unwrap_or(0);
                info!(task = %entry.task.id, status = %entry.task.status, mb = (bytes + temp) / 1_048_576, "Removed closed task workspace");
                removed.count += 1;
                removed.cleaned.push(entry.task.id);
                removed.bytes += bytes + temp;
                if !removed.trash_roots.contains(&entry.trash_root) {
                    removed.trash_roots.push(entry.trash_root);
                }
            }
            Err(error) => warn!(error = %format!("{error:#}"), task = %entry.task.id, "Failed to remove closed task workspace"),
        }
    }
    removed
}

pub(super) async fn remove_quest_temp(id: Uuid) -> Option<u64> {
    let dir = std::env::temp_dir().join(crate::cli::quest_temp_name(id));
    if !dir.is_dir() {
        return None;
    }
    let measured = dir.clone();
    let bytes = tokio::task::spawn_blocking(move || dir_size(&measured)).await.unwrap_or(0);
    stop_processes_in(&dir).await;
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Some(bytes),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %error, task = %id, "Failed to remove the quest temp dir");
            }
            None
        }
    }
}

async fn remove_temps(ids: impl IntoIterator<Item = Uuid>) -> Removed {
    let mut removed = Removed::default();
    for id in ids {
        if let Some(bytes) = remove_quest_temp(id).await {
            info!(task = %id, mb = bytes / 1_048_576, "Removed closed quest temp dir");
            removed.count += 1;
            removed.cleaned.push(id);
            removed.bytes += bytes;
        }
    }
    removed
}

fn mentions(command: &str, dir: &str) -> bool {
    command.match_indices(dir).any(|(at, _)| {
        command[at + dir.len()..].chars().next().is_none_or(|next| next == '/' || next == '"' || next == '\'' || next.is_whitespace())
    })
}

fn pids_under(listing: &str, dirs: &[String], own: u32) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            (pid > 1 && pid != own && dirs.iter().any(|dir| mentions(command, dir))).then_some(pid)
        })
        .collect()
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

async fn stop_processes_in(dir: &Path) {
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if !dirs.contains(&real) {
            dirs.push(real);
        }
    }
    let Ok(output) = tokio::process::Command::new("ps").args(["-axo", "pid=,command="]).output().await else {
        return;
    };
    let pids = pids_under(&String::from_utf8_lossy(&output.stdout), &dirs, std::process::id());
    if pids.is_empty() {
        return;
    }
    for pid in &pids {
        unsafe { libc::kill(*pid as i32, libc::SIGTERM) };
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for pid in pids.into_iter().filter(|pid| alive(*pid)) {
        warn!(pid, dir = %dir.display(), "Stopping a process that still runs in a closed quest temp dir");
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

pub(super) async fn remove_task_workspace(dir: &Path, trash: &Path) -> Result<()> {
    let mut repos = Vec::new();
    let mut children = tokio::fs::read_dir(dir).await?;
    while let Some(child) = children.next_entry().await? {
        if let Ok(common) = git_output(&child.path(), &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await {
            let common = PathBuf::from(common.trim());
            if !repos.contains(&common) {
                repos.push(common);
            }
        }
    }
    tokio::fs::rename(dir, trash).await?;
    for repo in repos {
        run_git(&repo, &["worktree", "prune"]).await?;
    }
    Ok(())
}

async fn clear(api: &RunnerApi, root: &Path, mode: Sweep, free: f64) -> Result<Removed> {
    let mut entries = Vec::new();
    let mut temp_ids = Vec::new();
    let mut cache_quests: Vec<(Uuid, PathBuf)> = Vec::new();
    for base in [root.to_path_buf()] {
        let tasks_dir = base.join("workspaces").join("tasks");
        let Ok(mut listing) = tokio::fs::read_dir(&tasks_dir).await else {
            continue;
        };
        let mut tasks = Vec::new();
        while let Some(entry) = listing.next_entry().await? {
            if let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() {
                tasks.push(id);
            }
        }
        if tasks.is_empty() {
            continue;
        }
        let closed = api.closed_tasks(&tasks, mode != Sweep::Routine).await?;
        if base == root {
            let settled = if mode == Sweep::Routine { api.closed_tasks(&tasks, true).await? } else { closed.clone() };
            temp_ids.extend(settled.into_iter().map(|task| task.id));
            if mode != Sweep::Routine {
                let idle = api.idle_tasks(&tasks).await.unwrap_or_default();
                cache_quests.extend(closed.iter().map(|task| task.id).chain(idle).map(|id| (id, tasks_dir.join(id.to_string()))));
            }
        }
        for task in closed {
            entries.push(Entry { dir: tasks_dir.join(task.id.to_string()), trash_root: base.join("trash"), task });
        }
    }
    let mut removed = remove_temps(temp_ids).await;
    if mode != Sweep::Routine {
        let cache_bytes = clear_build_dirs(&cache_quests).await;
        removed.cache_bytes += cache_bytes;
        removed.bytes += cache_bytes;
    }
    let more = remove_entries(by_age(entries), mode, free + removed.bytes as f64).await;
    removed.absorb(more);
    Ok(removed)
}

pub(super) async fn sweep(api: &RunnerApi, root: &Path, mode: Sweep, rescan: &Arc<Notify>) -> Result<()> {
    let free = free_bytes(root);
    let removed = clear(api, root, mode, free).await?;
    let api = api.clone();
    let root = root.to_path_buf();
    let rescan = rescan.clone();
    tokio::spawn(async move {
        for trash in &removed.trash_roots {
            let _ = tokio::fs::remove_dir_all(trash).await;
        }
        if removed.count > 0 {
            rescan.notify_one();
        }
        if mode == Sweep::Pressure {
            warn_owner(&api, &root, free_bytes(&root), &removed).await;
        }
    });
    Ok(())
}

fn free_bytes(root: &Path) -> f64 {
    disk_space(root).map(|(free, _)| free).unwrap_or(f64::MAX)
}

pub(super) async fn guard(api: &RunnerApi, root: &Path) -> Result<()> {
    let floor = disk_space(root).map_or(LOW_DISK_BYTES, |(_, total)| LOW_DISK_BYTES.min(total * 0.1));
    guard_with(api, root, floor, &|| free_bytes(root)).await
}

async fn guard_with(api: &RunnerApi, root: &Path, floor: f64, free: &(dyn Fn() -> f64 + Send + Sync)) -> Result<()> {
    if free() >= floor {
        return Ok(());
    }
    let _one_at_a_time = GUARD.lock().await;
    if free() >= floor {
        return Ok(());
    }
    let removed = match clear(api, root, Sweep::Pressure, free()).await {
        Ok(removed) => removed,
        Err(error) => {
            warn!(error = %format!("{error:#}"), "Failed to clear closed quest folders before a run");
            Removed::default()
        }
    };
    for base in [root.to_path_buf()] {
        let _ = tokio::fs::remove_dir_all(base.join("trash")).await;
    }
    let free = free();
    warn_owner(api, root, free, &removed).await;
    if free >= floor {
        return Ok(());
    }
    anyhow::bail!(
        "No space left on device: this device has only {:.1} GB free, so Loka did not start this run. Build caches, closed quest workspaces and temp folders were cleared first. {}",
        free / 1e9,
        folder_list(&biggest_folders(root).await)
    )
}

fn folder_list(folders: &[DiskFolder]) -> String {
    if folders.is_empty() {
        return "Free up space on the device and retry.".to_string();
    }
    let named: Vec<String> = folders.iter().map(|folder| format!("{} ({:.1} GB)", folder.name, folder.bytes as f64 / 1e9)).collect();
    format!("Biggest folders: {}.", named.join(", "))
}

fn largest_folders(root: &Path, temp: &Path, min_bytes: u64) -> Vec<DiskFolder> {
    let mut found: Vec<DiskFolder> = Vec::new();
    let mut add = |dir: &Path, label: &dyn Fn(&str) -> String| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = dir_size(&entry.path());
            if bytes >= min_bytes {
                found.push(DiskFolder { name: label(&name), bytes });
            }
        }
    };
    add(&root.join("workspaces").join("tasks"), &|name| format!("quest workspace {}", name.chars().take(8).collect::<String>()));
    add(temp, &|name| format!("temp/{name}"));
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    found.truncate(FOLDERS_LISTED);
    found
}

async fn biggest_folders(root: &Path) -> Vec<DiskFolder> {
    if let Some((at, folders)) = FOLDERS.lock().unwrap().as_ref() {
        if at.elapsed() < FOLDERS_CACHED_FOR {
            return folders.clone();
        }
    }
    let root = root.to_path_buf();
    let folders = tokio::task::spawn_blocking(move || largest_folders(&root, &std::env::temp_dir(), FOLDER_MIN_BYTES)).await.unwrap_or_default();
    *FOLDERS.lock().unwrap() = Some((Instant::now(), folders.clone()));
    folders
}

async fn warn_owner(api: &RunnerApi, root: &Path, free: f64, removed: &Removed) {
    let still_low = free < LOW_DISK_BYTES;
    if removed.count == 0 && removed.cache_bytes == 0 && !still_low {
        return;
    }
    {
        let mut last = LAST_NOTICE.lock().unwrap();
        if last.is_some_and(|at| at.elapsed() < NOTICE_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    let folders = if still_low { biggest_folders(root).await } else { Vec::new() };
    let notice = DiskNotice { free_bytes: free as u64, freed_bytes: removed.bytes, removed: removed.count, still_low, folders, caches_bytes: removed.cache_bytes };
    if let Err(error) = api.disk_low(&notice).await {
        warn!(error = %format!("{error:#}"), "Failed to tell the owner the disk is low");
    }
}

pub(super) fn by_age(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by_key(|entry| entry.task.idle_since);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn closing_a_quest_removes_its_temp_dir_and_only_its_own() {
        let (closed, kept) = (Uuid::new_v4(), Uuid::new_v4());
        let dir_of = |id: Uuid| std::env::temp_dir().join(crate::cli::quest_temp_name(id));
        for id in [closed, kept] {
            std::fs::create_dir_all(dir_of(id).join("target")).unwrap();
            std::fs::write(dir_of(id).join("target").join("out.bin"), b"x").unwrap();
        }
        remove_quest_temp(closed).await;
        remove_quest_temp(closed).await;
        assert!(!dir_of(closed).exists());
        assert!(dir_of(kept).exists(), "another quest keeps its temp dir");
        remove_quest_temp(kept).await;
    }

    fn quest_workspace(base: &Path, repo: &Path, branch: &str) -> (Uuid, PathBuf) {
        let id = Uuid::new_v4();
        let dir = base.join("workspaces").join("tasks").join(id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let tree = dir.join("world");
        git(repo, &["worktree", "add", "-b", branch, tree.to_str().unwrap()]);
        std::fs::write(tree.join("target.bin"), vec![7u8; 600_000]).unwrap();
        (id, dir)
    }

    fn entry(base: &Path, id: Uuid, dir: PathBuf, status: &str, days: i64) -> Entry {
        Entry {
            dir,
            trash_root: base.join("trash"),
            task: ClosedTask { id, status: status.into(), idle_since: Some(chrono::Utc::now() - chrono::Duration::days(days)) },
        }
    }

    fn fixture() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("loka-workspaces-{}", Uuid::new_v4()));
        let repo = base.join("repos").join("acme").join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "hi").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        (base, repo)
    }

    fn worktrees(repo: &Path) -> String {
        let output = std::process::Command::new("git").current_dir(repo).args(["worktree", "list", "--porcelain"]).output().unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[tokio::test]
    async fn closed_workspaces_leave_no_worktree_behind() {
        let (base, repo) = fixture();
        let (id, dir) = quest_workspace(&base, &repo, "quest-a");
        assert!(worktrees(&repo).contains(&id.to_string()));
        let removed = remove_entries(vec![entry(&base, id, dir.clone(), "done", 5)], Sweep::Routine, f64::MAX).await;
        assert_eq!(removed.count, 1);
        assert!(!dir.exists());
        assert!(!worktrees(&repo).contains(&id.to_string()), "worktree entry pruned: {}", worktrees(&repo));
        let branches = std::process::Command::new("git").current_dir(&repo).args(["branch", "--list", "quest-a"]).output().unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).contains("quest-a"), "the branch and its commits stay");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn low_disk_removes_oldest_first_and_stops_once_there_is_room() {
        let (base, repo) = fixture();
        let mut entries = Vec::new();
        for (index, days) in [20i64, 10, 5].into_iter().enumerate() {
            let (id, dir) = quest_workspace(&base, &repo, &format!("quest-{index}"));
            entries.push(entry(&base, id, dir, "done", days));
        }
        let entries = by_age(entries.into_iter().rev().collect());
        assert!(entries[0].task.idle_since < entries[1].task.idle_since);
        let first_size = dir_size(&entries[0].dir);
        let free = TARGET_FREE_BYTES - first_size as f64;
        let removed = remove_entries(entries.clone(), Sweep::Pressure, free).await;
        assert_eq!(removed.count, 1, "stops as soon as the free space target is met");
        assert!(!entries[0].dir.exists());
        assert!(entries[1].dir.exists() && entries[2].dir.exists());
        let everything = remove_entries(entries[1..].to_vec(), Sweep::Everything, 0.0).await;
        assert_eq!(everything.count, 2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn failed_quests_with_unsaved_changes_survive_a_low_disk_sweep() {
        let (base, repo) = fixture();
        let (dirty_id, dirty_dir) = quest_workspace(&base, &repo, "quest-dirty");
        std::fs::write(dirty_dir.join("world").join("notes.txt"), "unsaved").unwrap();
        let (clean_id, clean_dir) = quest_workspace(&base, &repo, "quest-clean");
        git(&clean_dir.join("world"), &["add", "target.bin"]);
        git(&clean_dir.join("world"), &["commit", "-q", "-m", "work"]);
        let entries = vec![
            entry(&base, dirty_id, dirty_dir.clone(), "failed", 9),
            entry(&base, clean_id, clean_dir.clone(), "failed", 8),
        ];
        let removed = remove_entries(entries, Sweep::Pressure, 0.0).await;
        assert_eq!(removed.count, 1);
        assert!(dirty_dir.exists(), "unsaved work of a failed quest is kept");
        assert!(!clean_dir.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn disk_use_counts_every_file_and_nothing_else() {
        let base = std::env::temp_dir().join(format!("loka-size-{}", Uuid::new_v4()));
        std::fs::create_dir_all(base.join("a").join("b")).unwrap();
        std::fs::write(base.join("a").join("b").join("big.bin"), vec![1u8; 2_000_000]).unwrap();
        let bytes = dir_size(&base);
        assert!((2_000_000..2_200_000).contains(&bytes), "{bytes}");
        assert_eq!(dir_size(&base.join("missing")), 0);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_pressure_sweeps_stop_early() {
        assert!(!enough(Sweep::Routine, TARGET_FREE_BYTES, 0));
        assert!(!enough(Sweep::Everything, TARGET_FREE_BYTES, 0));
        assert!(!enough(Sweep::Pressure, 1.0, 1));
        assert!(enough(Sweep::Pressure, 1.0, TARGET_FREE_BYTES as u64));
    }

    #[test]
    fn closing_a_quest_stops_the_test_databases_it_started() {
        let dir = "/private/tmp/claude-501/loka-q-0123456789ab";
        let listing = format!(
            "  101 /usr/bin/node pglite-server -d {dir}/pgl/db -p 55499\n  102 cargo test --target-dir={dir}/target\n  103 node serve {dir}\n  104 node pglite-server -d {dir}2/pgl\n  105 /bin/zsh -l\n  7 sleep 1 {dir}/x\n 1 /sbin/launchd {dir}\n"
        );
        let pids = pids_under(&listing, &[dir.to_string()], 7);
        assert_eq!(pids, vec![101, 102, 103], "only processes inside the dir, never ours, never pid 1: {pids:?}");
        assert!(pids_under(&listing, &["/private/tmp/claude-501/loka-q-other".to_string()], 7).is_empty());
    }

    #[tokio::test]
    async fn closing_a_quest_kills_the_process_that_runs_in_its_temp_dir() {
        let id = Uuid::new_v4();
        let dir = std::env::temp_dir().join(crate::cli::quest_temp_name(id));
        std::fs::create_dir_all(dir.join("pgl")).unwrap();
        if !std::process::Command::new("ps").args(["-axo", "pid=,command="]).output().is_ok_and(|output| output.status.success()) {
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        let mut child = tokio::process::Command::new("sh").arg("-c").arg(format!("sleep 30; : {}/pgl", dir.display())).spawn().unwrap();
        let pid = child.id().unwrap();
        assert!(alive(pid));
        let freed = remove_quest_temp(id).await;
        assert!(freed.is_some());
        assert!(!dir.exists());
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await.expect("the process was stopped").unwrap();
        assert!(!status.success());
        assert_eq!(remove_quest_temp(id).await, None);
    }

    #[test]
    fn usage_counts_a_quests_temp_dir_with_its_workspace() {
        let id = Uuid::new_v4();
        let base = std::env::temp_dir().join(format!("loka-usage-{}", Uuid::new_v4()));
        std::fs::create_dir_all(base.join("workspaces").join("tasks").join(id.to_string())).unwrap();
        std::fs::write(base.join("workspaces").join("tasks").join(id.to_string()).join("a.bin"), vec![1u8; 300_000]).unwrap();
        let temp = std::env::temp_dir().join(crate::cli::quest_temp_name(id));
        std::fs::create_dir_all(temp.join("target")).unwrap();
        std::fs::write(temp.join("target").join("b.bin"), vec![2u8; 3_000_000]).unwrap();
        let usage = scan(std::slice::from_ref(&base));
        assert_eq!(usage.tasks, 1);
        assert!((3_000_000..3_400_000).contains(&usage.temp_bytes), "{}", usage.temp_bytes);
        assert!(usage.bytes > usage.temp_bytes + 200_000, "{} {}", usage.bytes, usage.temp_bytes);
        std::fs::remove_dir_all(&temp).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_biggest_folders_come_from_workspaces_and_temp_largest_first() {
        let base = std::env::temp_dir().join(format!("loka-folders-{}", Uuid::new_v4()));
        let temp = base.join("tmp");
        let task = Uuid::new_v4();
        std::fs::create_dir_all(base.join("workspaces").join("tasks").join(task.to_string())).unwrap();
        std::fs::write(base.join("workspaces").join("tasks").join(task.to_string()).join("w.bin"), vec![1u8; 1_000_000]).unwrap();
        std::fs::create_dir_all(temp.join("loka-target-abc")).unwrap();
        std::fs::write(temp.join("loka-target-abc").join("t.bin"), vec![1u8; 4_000_000]).unwrap();
        std::fs::create_dir_all(temp.join("tiny")).unwrap();
        std::fs::write(temp.join("tiny").join("x"), b"x").unwrap();
        let folders = largest_folders(&base, &temp, 500_000);
        let names: Vec<&str> = folders.iter().map(|folder| folder.name.as_str()).collect();
        assert_eq!(names, vec!["temp/loka-target-abc".to_string(), format!("quest workspace {}", &task.to_string()[..8])]);
        let text = folder_list(&folders);
        assert!(text.starts_with("Biggest folders: temp/loka-target-abc ("), "{text}");
        assert_eq!(folder_list(&[]), "Free up space on the device and retry.");
        std::fs::remove_dir_all(&base).unwrap();
    }

    async fn mock_api(closed: Uuid) -> (RunnerApi, Arc<Mutex<Vec<(String, Value)>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (head, mut body) = loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..read]);
                        if let Some(at) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                            break (String::from_utf8_lossy(&raw[..at]).to_string(), raw[at + 4..].to_vec());
                        }
                    };
                    let length: usize = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
                    while body.len() < length {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..read]);
                    }
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                    let plain = if head.to_ascii_lowercase().contains("content-encoding: gzip") {
                        let mut out = Vec::new();
                        let _ = std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&body[..]), &mut out);
                        out
                    } else {
                        body
                    };
                    log.lock().unwrap().push((path.clone(), serde_json::from_slice(&plain).unwrap_or(Value::Null)));
                    let reply = if path.ends_with("/workspaces") {
                        json!({ "closed": [closed], "entries": [{ "id": closed, "status": "done", "idle_since": null }] })
                    } else {
                        json!({ "sent": true })
                    };
                    let reply = reply.to_string();
                    let _ = socket
                        .write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).as_bytes())
                        .await;
                });
            }
        });
        let api = RunnerApi {
            client: reqwest::Client::new(),
            stream_client: reqwest::Client::new(),
            base_url: format!("http://127.0.0.1:{port}"),
            runner_id: Uuid::new_v4(),
            token: "t".into(),
        };
        (api, seen)
    }

    #[tokio::test]
    async fn a_full_disk_clears_closed_quests_first_then_refuses_runs_with_one_notice() {
        let quest = Uuid::new_v4();
        let root = std::env::temp_dir().join(format!("loka-guard-{}", Uuid::new_v4()));
        let tasks = root.join("workspaces").join("tasks");
        let quest_dir = tasks.join(quest.to_string());
        let temp = std::env::temp_dir().join(crate::cli::quest_temp_name(quest));
        let stage = || {
            std::fs::create_dir_all(&quest_dir).unwrap();
            std::fs::write(quest_dir.join("w.bin"), vec![1u8; 200_000]).unwrap();
            std::fs::create_dir_all(temp.join("target")).unwrap();
            std::fs::write(temp.join("target").join("t.bin"), vec![2u8; 200_000]).unwrap();
        };
        let (api, seen) = mock_api(quest).await;
        *LAST_NOTICE.lock().unwrap() = None;
        *FOLDERS.lock().unwrap() = None;

        stage();
        let recovers = || if quest_dir.exists() || temp.exists() { 1.0e9 } else { 50.0e9 };
        guard_with(&api, &root, LOW_DISK_BYTES, &recovers).await.expect("clearing the closed quest made room, so the run starts");
        assert!(!quest_dir.exists() && !temp.exists(), "closed quest workspace and temp dir are both gone");
        *LAST_NOTICE.lock().unwrap() = None;
        seen.lock().unwrap().clear();

        stage();
        let stays_low = || 1.0e9;
        let first = guard_with(&api, &root, LOW_DISK_BYTES, &stays_low).await.unwrap_err().to_string();
        let second = guard_with(&api, &root, LOW_DISK_BYTES, &stays_low).await.unwrap_err().to_string();
        for message in [&first, &second] {
            assert!(message.contains("did not start this run"), "{message}");
            assert_eq!(loka_shared::cli_errors::classify(message).map(|failure| failure.kind), Some(loka_shared::cli_errors::FailureKind::DiskFull), "{message}");
        }
        assert!(!quest_dir.exists() && !temp.exists(), "cleared before refusing");
        let calls = seen.lock().unwrap().clone();
        let notices: Vec<&Value> = calls.iter().filter(|(path, _)| path.ends_with("/disk")).map(|(_, body)| body).collect();
        assert_eq!(notices.len(), 1, "one notice, not one per refused run: {calls:?}");
        assert_eq!(notices[0]["still_low"], true);
        assert_eq!(notices[0]["removed"], 1);
        assert!(guard_with(&api, &root, LOW_DISK_BYTES, &|| 50.0e9).await.is_ok(), "plenty of room never touches the API");
        assert!(guard_with(&api, &root, 0.5e9, &|| 1.0e9).await.is_ok(), "a small disk is judged by its own floor, not 5 GB");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn build_tree(base: &Path) -> (PathBuf, PathBuf) {
        let task = base.join("workspaces").join("tasks").join(Uuid::new_v4().to_string());
        let repo = task.join("app");
        for dir in ["node_modules/pkg", ".next/cache", "frontend/node_modules/pkg", "frontend/.next", "target/debug", "src", "docs/target", "desktop/src-tauri/target/debug", "a/b/c/d/node_modules"] {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
            std::fs::write(repo.join(dir).join("f.bin"), vec![5u8; 100_000]).unwrap();
        }
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(repo.join("desktop/src-tauri/Cargo.toml"), "[package]").unwrap();
        std::fs::write(repo.join("src").join("main.rs"), "fn main() {}").unwrap();
        (task, repo)
    }

    #[test]
    fn build_dirs_are_node_modules_next_and_cargo_targets_and_nothing_else() {
        let base = std::env::temp_dir().join(format!("loka-build-{}", Uuid::new_v4()));
        let (task, repo) = build_tree(&base);
        let mut found = build_dirs_in(&task);
        found.sort();
        let mut expected = vec![repo.join(".next"), repo.join("node_modules"), repo.join("target"), repo.join("frontend/.next"), repo.join("frontend/node_modules"), repo.join("desktop/src-tauri/target")];
        expected.sort();
        assert_eq!(found, expected, "docs/target has no Cargo.toml beside it, src is source and a/b/c/d is below the search depth");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn build_dirs_skip_symlinks_so_nothing_outside_the_quest_is_reached() {
        let base = std::env::temp_dir().join(format!("loka-build-link-{}", Uuid::new_v4()));
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let repo = base.join("task").join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("node_modules")).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("task").join("linked-repo")).unwrap();
        assert!(build_dirs_in(&base.join("task")).is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn clearing_build_dirs_keeps_the_source_and_reports_the_bytes() {
        let base = std::env::temp_dir().join(format!("loka-build-clear-{}", Uuid::new_v4()));
        let (task, repo) = build_tree(&base);
        let id = Uuid::new_v4();
        let freed = clear_build_dirs_with(&[(id, task.clone())], Some(String::new())).await;
        assert!(freed >= 500_000, "{freed}");
        assert!(!repo.join("node_modules").exists() && !repo.join("frontend/.next").exists() && !repo.join("target").exists());
        assert!(repo.join("src/main.rs").is_file() && repo.join("Cargo.toml").is_file() && repo.join("docs/target/f.bin").is_file());
        assert_eq!(clear_build_dirs_with(&[(id, task)], Some(String::new())).await, 0);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn an_idle_quests_cargo_target_goes_but_a_running_build_keeps_it_and_other_quests_are_untouched() {
        let (idle, busy, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let base = std::env::temp_dir().join(format!("loka-build-quests-{}", Uuid::new_v4()));
        let task_dir = |id: Uuid| base.join(id.to_string());
        for id in [idle, busy, other] {
            std::fs::create_dir_all(task_dir(id)).unwrap();
            std::fs::create_dir_all(quest_target_dir(id).join("debug")).unwrap();
            std::fs::write(quest_target_dir(id).join("debug").join("lib.rlib"), vec![9u8; 150_000]).unwrap();
        }
        let listing = format!(" 501 rustc --out-dir {}/debug/deps\n 502 zsh -l\n", quest_target_dir(busy).display());
        let freed = clear_build_dirs_with(&[(idle, task_dir(idle)), (busy, task_dir(busy))], Some(listing)).await;
        assert!(freed >= 150_000, "{freed}");
        assert!(!quest_target_dir(idle).exists(), "an idle quest's cargo target is cleared");
        assert!(quest_target_dir(busy).exists(), "a target a process runs in is kept");
        assert!(quest_target_dir(other).exists(), "a quest that was not asked about is never touched");
        assert_eq!(clear_build_dirs_with(&[(other, task_dir(other))], None).await, 0, "without a process list nothing is cleared");
        assert!(quest_target_dir(other).exists());
        for id in [busy, other] {
            let _ = std::fs::remove_dir_all(std::env::temp_dir().join(crate::cli::quest_temp_name(id)));
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_process_running_in_a_cache_dir_marks_it_in_use() {
        let dir = Path::new("/c/loka-cargo-target/world");
        let listing = " 301 rustc --out-dir /c/loka-cargo-target/world/debug/deps\n 302 /c/loka-cargo-target/world2/debug/x\n 303 zsh -l\n";
        assert!(in_use(listing, dir));
        assert!(!in_use(" 302 /c/loka-cargo-target/world2/debug/x\n", dir));
    }
}
