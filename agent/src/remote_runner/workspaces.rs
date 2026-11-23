use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use loka_shared::runners::{ClosedTask, DiskFolder, DiskNotice};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tracing::{info, warn};
use uuid::Uuid;

use super::{build_cache, git_output, run_git, RunnerApi};
use crate::quest_storage::{self, Active, Layout, QuestFolder, QuestFolders, TARGET_FREE_BYTES};
pub(super) use crate::quest_storage::{dir_size, in_use, process_listing};
use crate::quest_storage::{quest_running, remove_temp_dir};
use loka_shared::run_recording::{disk_space, free_bytes, no_room_message, run_floor, LOW_DISK_BYTES};

pub(super) const LOW_DISK_RECHECK: Duration = Duration::from_secs(60);
const PRESSURE_PASSES: usize = 4;
const SCAN_INTERVAL: Duration = Duration::from_secs(1800);
const FIRST_SCAN_DELAY: Duration = Duration::from_secs(20);
const NOTICE_EVERY: Duration = Duration::from_secs(3600);
const LISTED_MIN_BYTES: u64 = 50 * 1024 * 1024;
const LISTED_MAX: usize = 100;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const FOLDERS_LISTED: usize = 5;
const FOLDER_MIN_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sweep {
    Routine,
    Pressure,
    Everything,
    Caches,
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
    seed_bytes: u64,
    largest: Vec<QuestUse>,
}

static USAGE: Mutex<Option<Usage>> = Mutex::new(None);
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static LAST_NOTICE: Mutex<Option<Instant>> = Mutex::new(None);
static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) fn low_disk(root: &Path) -> bool {
    disk_space(root).is_some_and(|(free, _)| free < LOW_DISK_BYTES)
}

pub(super) fn modes_for(root: &Path, workspaces_requested: bool, caches_requested: bool) -> Vec<Sweep> {
    let mut modes = Vec::new();
    if caches_requested {
        modes.push(Sweep::Caches);
    }
    if workspaces_requested {
        modes.push(Sweep::Everything);
    }
    if modes.is_empty() {
        modes.push(if low_disk(root) { Sweep::Pressure } else { Sweep::Routine });
    }
    modes
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
        "seed_bytes": usage.seed_bytes,
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
            let seeds = root.clone();
            let scanned = tokio::task::spawn_blocking(move || Usage { seed_bytes: build_cache::seed_bytes(&seeds), ..scan(&roots) }).await;
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

fn find_quest_folders(root: &Path) -> QuestFolders {
    quest_storage::find_quest_folders(&Layout::runner(root))
}

fn quest_folders(root: &Path) -> Vec<QuestFolder> {
    quest_storage::quest_folders(&Layout::runner(root))
}

pub(super) fn activate(root: &Path, lease: Uuid, workspace_key: &str) -> Active {
    quest_storage::activate(&Layout::runner(root), lease, crate::cli::quest_id(Path::new(workspace_key)))
}

async fn clear_idle_builds(root: &Path, only: Option<HashSet<String>>, listing: Option<String>, enough: impl FnMut(u64) -> bool) -> u64 {
    quest_storage::clear_idle_builds(&Layout::runner(root), only, listing, enough).await
}

async fn clear_idle_scratch(root: &Path, listing: String, enough: impl FnMut(u64) -> bool) -> u64 {
    quest_storage::clear_idle_scratch(&Layout::runner(root), listing, enough).await
}

fn own_session_temps(root: &Path) -> Vec<(Uuid, PathBuf)> {
    crate::cli::private_temp_homes()
        .into_iter()
        .filter_map(|home| std::fs::read_dir(home).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(crate::cli::SESSION_TEMP_PREFIX))
        .filter_map(|entry| {
            let (id, owner) = crate::cli::private_temp_owner(&entry.path())?;
            (owner == root).then(|| (id, entry.path()))
        })
        .collect()
}

fn scan(roots: &[PathBuf]) -> Usage {
    let session_temp: u64 = roots.first().map(|root| own_session_temps(root).iter().map(|(_, dir)| dir_size(dir)).sum()).unwrap_or(0);
    let mut sizes: Vec<QuestUse> = Vec::new();
    for root in roots {
        for folder in quest_folders(root) {
            let temp: u64 = folder.temps.iter().map(|dir| dir_size(dir)).sum();
            let build = if quest_running(&folder.name) { 0 } else { folder.rebuildable().iter().map(|dir| dir_size(dir)).sum() };
            let checkout = folder.workspace.as_deref().map(dir_size).unwrap_or(0);
            sizes.push(QuestUse { id: folder.id, bytes: checkout + temp, temp, build });
        }
    }
    sizes.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    Usage {
        bytes: sizes.iter().map(|quest| quest.bytes).sum::<u64>() + session_temp,
        tasks: sizes.len(),
        temp_bytes: sizes.iter().map(|quest| quest.temp).sum::<u64>() + session_temp,
        build_bytes: sizes.iter().map(|quest| quest.build).sum(),
        seed_bytes: 0,
        largest: sizes.into_iter().filter(|quest| quest.bytes >= LISTED_MIN_BYTES).take(LISTED_MAX).collect(),
    }
}

async fn adopt_unowned(api: &RunnerApi, root: &Path) {
    let unowned = {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || find_quest_folders(&root).unowned).await.unwrap_or_default()
    };
    let mut prefixes: Vec<String> = unowned
        .iter()
        .filter_map(|dir| dir.file_name()?.to_str()?.strip_prefix(crate::cli::QUEST_TEMP_PREFIX).map(String::from))
        .collect();
    prefixes.sort();
    prefixes.dedup();
    if prefixes.is_empty() {
        return;
    }
    let found = match api.quests_by_prefix(&prefixes).await {
        Ok(found) => found,
        Err(error) => {
            warn!(error = %format!("{error:#}"), "Could not ask which quests old temp folders belong to");
            return;
        }
    };
    for (_, id) in found {
        let name = crate::cli::quest_temp_name(id);
        for dir in unowned.iter().filter(|dir| dir.file_name().is_some_and(|file| file.to_string_lossy() == name)) {
            if let Err(error) = crate::cli::mark_private_temp_owner(dir, id, root) {
                warn!(error = %error, dir = %dir.display(), "Failed to mark an old quest temp folder");
            }
        }
    }
}

async fn temps_without_checkout(root: &Path) -> Vec<Uuid> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || quest_folders(&root).into_iter().filter(|folder| folder.workspace.is_none()).map(|folder| folder.id).collect())
        .await
        .unwrap_or_default()
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
    let mut freed = None;
    for home in crate::cli::private_temp_homes() {
        if let Some(bytes) = remove_temp_dir(home.join(crate::cli::quest_temp_name(id))).await {
            *freed.get_or_insert(0) += bytes;
        }
    }
    freed
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

async fn remove_closed_session_temps(api: &RunnerApi, root: &Path) -> Removed {
    let owned = {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || own_session_temps(&root)).await.unwrap_or_default()
    };
    let mut removed = Removed::default();
    if owned.is_empty() {
        return removed;
    }
    let ids: Vec<Uuid> = owned.iter().map(|(id, _)| *id).collect();
    let closed = match api.closed_sessions(&ids).await {
        Ok(closed) => closed,
        Err(error) => {
            warn!(error = %format!("{error:#}"), "Failed to ask which folder and quick chat sessions are closed");
            return removed;
        }
    };
    for (id, dir) in owned.into_iter().filter(|(id, _)| closed.contains(id)) {
        if let Some(bytes) = remove_temp_dir(dir).await {
            info!(session = %id, mb = bytes / 1_048_576, "Removed closed session temp dir");
            removed.count += 1;
            removed.bytes += bytes;
        }
    }
    removed
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
    let listing = process_listing().await;
    clear_with(api, root, mode, free, listing).await
}

async fn relieve(api: &RunnerApi, root: &Path, free_now: &(dyn Fn() -> f64 + Send + Sync), listing: Option<String>) -> Removed {
    let listing = listing.or_else(|| {
        warn!("Could not list processes, clearing idle quests by their runs alone");
        Some(String::new())
    });
    let mut total = Removed::default();
    for _ in 0..PRESSURE_PASSES {
        let removed = match clear_with(api, root, Sweep::Pressure, free_now(), listing.clone()).await {
            Ok(removed) => removed,
            Err(error) => {
                warn!(error = %format!("{error:#}"), "Failed to clear closed quest folders under disk pressure");
                break;
            }
        };
        for trash in &removed.trash_roots {
            let _ = tokio::fs::remove_dir_all(trash).await;
        }
        let progressed = removed.count > 0 || removed.bytes > 0;
        total.absorb(removed);
        if !progressed || free_now() >= TARGET_FREE_BYTES {
            break;
        }
    }
    total
}

async fn clear_with(api: &RunnerApi, root: &Path, mode: Sweep, free: f64, listing: Option<String>) -> Result<Removed> {
    if mode == Sweep::Caches {
        let cache_bytes = clear_idle_builds(root, None, listing, |_| false).await + build_cache::clear_seeds(root, |_| false).await;
        return Ok(Removed { bytes: cache_bytes, cache_bytes, ..Default::default() });
    }
    adopt_unowned(api, root).await;
    let scratch_listing = listing.clone().unwrap_or_default();
    let mut entries = Vec::new();
    let mut temp_ids = Vec::new();
    let mut closed_names: HashSet<String> = HashSet::new();
    for base in [root.to_path_buf()] {
        let tasks_dir = base.join("workspaces").join("tasks");
        let mut tasks = Vec::new();
        if let Ok(mut listing) = tokio::fs::read_dir(&tasks_dir).await {
            while let Some(entry) = listing.next_entry().await? {
                if let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() {
                    tasks.push(id);
                }
            }
        }
        let asked: Vec<Uuid> = if base == root { tasks.iter().copied().chain(temps_without_checkout(root).await).collect() } else { tasks.clone() };
        if asked.is_empty() {
            continue;
        }
        let closed = match api.closed_tasks(&asked, mode != Sweep::Routine).await {
            Ok(closed) => closed,
            Err(error) if mode == Sweep::Pressure => {
                warn!(error = %format!("{error:#}"), "Could not ask which quests are closed, clearing idle build output on this device alone");
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        if base == root {
            let settled = if mode == Sweep::Routine { api.closed_tasks(&asked, true).await? } else { closed.clone() };
            closed_names.extend(settled.iter().map(|task| crate::cli::quest_temp_name(task.id)));
            temp_ids.extend(settled.into_iter().map(|task| task.id));
        }
        for task in closed.into_iter().filter(|task| tasks.contains(&task.id)) {
            entries.push(Entry { dir: tasks_dir.join(task.id.to_string()), trash_root: base.join("trash"), task });
        }
    }
    let mut removed = remove_temps(temp_ids).await;
    removed.absorb(remove_closed_session_temps(api, root).await);
    let cache_bytes = match mode {
        Sweep::Pressure => {
            let already = removed.bytes;
            clear_idle_builds(root, None, listing, |freed| enough(mode, free, already + freed)).await
        }
        Sweep::Everything | Sweep::Routine => clear_idle_builds(root, Some(closed_names), listing, |_| false).await,
        Sweep::Caches => 0,
    };
    removed.cache_bytes += cache_bytes;
    removed.bytes += cache_bytes;
    if mode == Sweep::Pressure && !enough(mode, free, removed.bytes) {
        let seed_bytes = build_cache::clear_seeds(root, |freed| enough(mode, free, removed.bytes + freed)).await;
        removed.cache_bytes += seed_bytes;
        removed.bytes += seed_bytes;
    }
    if mode == Sweep::Pressure && !enough(mode, free, removed.bytes) {
        let scratch_bytes = clear_idle_scratch(root, scratch_listing, |freed| enough(mode, free, removed.bytes + freed)).await;
        removed.cache_bytes += scratch_bytes;
        removed.bytes += scratch_bytes;
    }
    let more = remove_entries(by_age(entries), mode, free + removed.bytes as f64).await;
    removed.absorb(more);
    Ok(removed)
}

pub(super) async fn sweep(api: &RunnerApi, root: &Path, mode: Sweep, rescan: &Arc<Notify>) -> Result<()> {
    let free = free_bytes(root);
    let listing = if mode == Sweep::Pressure { process_listing().await } else { None };
    let removed = if mode == Sweep::Pressure {
        relieve(api, root, &|| free_bytes(root), listing.clone()).await
    } else {
        clear(api, root, mode, free).await?
    };
    let api = api.clone();
    let root = root.to_path_buf();
    let rescan = rescan.clone();
    tokio::spawn(async move {
        for trash in &removed.trash_roots {
            let _ = tokio::fs::remove_dir_all(trash).await;
        }
        if removed.count > 0 || removed.cache_bytes > 0 {
            rescan.notify_one();
        }
        if mode == Sweep::Pressure {
            warn_owner(&api, &root, free_bytes(&root), run_floor(&root), &removed, listing.unwrap_or_default()).await;
        }
    });
    Ok(())
}

pub(super) async fn guard(api: &RunnerApi, root: &Path) -> Result<()> {
    let floor = run_floor(root);
    if free_bytes(root) >= floor {
        return Ok(());
    }
    guard_with(api, root, floor, &|| free_bytes(root), process_listing().await).await
}

async fn guard_with(api: &RunnerApi, root: &Path, floor: f64, free: &(dyn Fn() -> f64 + Send + Sync), listing: Option<String>) -> Result<()> {
    if free() >= floor {
        return Ok(());
    }
    let _one_at_a_time = GUARD.lock().await;
    if free() >= floor {
        return Ok(());
    }
    let left_listing = listing.clone().unwrap_or_default();
    let removed = relieve(api, root, free, listing).await;
    for base in [root.to_path_buf()] {
        let _ = tokio::fs::remove_dir_all(base.join("trash")).await;
    }
    let free = free();
    let notice = disk_notice(root, free, floor, &removed, left_listing).await;
    if notice_due(&notice) {
        send_notice(api, &notice).await;
    }
    if free >= floor {
        return Ok(());
    }
    anyhow::bail!("{}{}{}", no_room_message(free), notice.cleared(), notice.left())
}

struct LeftOver {
    held: u64,
    kept: Vec<DiskFolder>,
    unowned: Vec<DiskFolder>,
}

fn largest_first(mut folders: Vec<DiskFolder>) -> Vec<DiskFolder> {
    folders.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    folders.truncate(FOLDERS_LISTED);
    folders
}

fn left_over(root: &Path, listing: &str) -> LeftOver {
    let found = find_quest_folders(root);
    let mut held = 0;
    let mut kept = Vec::new();
    for folder in &found.quests {
        let builds = folder.rebuildable();
        if quest_running(&folder.name) {
            held += builds.iter().map(|dir| dir_size(dir)).sum::<u64>();
            continue;
        }
        let bytes: u64 = builds.iter().filter(|dir| folder.kept_by_a_process(listing, dir)).map(|dir| dir_size(dir)).sum();
        if bytes > 0 {
            kept.push(DiskFolder { name: format!("quest {}", &folder.id.to_string()[..8]), bytes, quest: Some(folder.id) });
        }
    }
    let unowned = found
        .unowned
        .iter()
        .map(|dir| DiskFolder { name: dir.display().to_string(), bytes: dir_size(dir), quest: None })
        .filter(|folder| folder.bytes >= FOLDER_MIN_BYTES)
        .collect();
    LeftOver { held, kept: largest_first(kept), unowned: largest_first(unowned) }
}

async fn disk_notice(root: &Path, free: f64, floor: f64, removed: &Removed, listing: String) -> DiskNotice {
    let still_low = free < floor;
    let mut notice = DiskNotice { free_bytes: free as u64, freed_bytes: removed.bytes, removed: removed.count, still_low, caches_bytes: removed.cache_bytes, ..Default::default() };
    if !still_low {
        return notice;
    }
    let root = root.to_path_buf();
    let left = tokio::task::spawn_blocking(move || left_over(&root, &listing)).await.unwrap_or(LeftOver { held: 0, kept: Vec::new(), unowned: Vec::new() });
    let coming_back = free + [left.held, left.kept.iter().map(|folder| folder.bytes).sum(), left.unowned.iter().map(|folder| folder.bytes).sum()].iter().sum::<u64>() as f64;
    notice.outside_bytes = (floor - coming_back).max(0.0) as u64;
    notice.held_bytes = left.held;
    notice.kept = left.kept;
    notice.unowned = left.unowned;
    notice
}

fn notice_due(notice: &DiskNotice) -> bool {
    if notice.removed == 0 && notice.caches_bytes == 0 && !notice.still_low {
        return false;
    }
    let mut last = LAST_NOTICE.lock().unwrap();
    if last.is_some_and(|at| at.elapsed() < NOTICE_EVERY) {
        return false;
    }
    *last = Some(Instant::now());
    true
}

async fn send_notice(api: &RunnerApi, notice: &DiskNotice) {
    if let Err(error) = api.disk_low(notice).await {
        warn!(error = %format!("{error:#}"), "Failed to tell the owner the disk is low");
    }
}

async fn warn_owner(api: &RunnerApi, root: &Path, free: f64, floor: f64, removed: &Removed, listing: String) {
    let quick = DiskNotice { free_bytes: free as u64, removed: removed.count, caches_bytes: removed.cache_bytes, still_low: free < floor, ..Default::default() };
    if !notice_due(&quick) {
        return;
    }
    send_notice(api, &disk_notice(root, free, floor, removed, listing).await).await;
}

pub(super) fn by_age(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by_key(|entry| entry.task.idle_since);
    entries
}

#[cfg(test)]
mod tests {
    use loka_shared::run_recording::floor_for;
    use super::*;
    use std::time::SystemTime;
    use crate::quest_storage::{alive, pids_under, rebuildable_dirs, CACHE_TAG, CACHE_TAG_SIGNATURE};

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
        let dir_of = |id: Uuid| crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id));
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
        let dir = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id));
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
        let temp = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id));
        std::fs::create_dir_all(temp.join("target")).unwrap();
        std::fs::write(temp.join("target").join("b.bin"), vec![2u8; 3_000_000]).unwrap();
        let usage = scan(std::slice::from_ref(&base));
        assert_eq!(usage.tasks, 1);
        assert!((3_000_000..3_400_000).contains(&usage.temp_bytes), "{}", usage.temp_bytes);
        assert!(usage.bytes > usage.temp_bytes + 200_000, "{} {}", usage.bytes, usage.temp_bytes);
        std::fs::remove_dir_all(&temp).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }

    fn session_temp(root: &Path, id: Uuid, bytes: usize) -> PathBuf {
        let dir = crate::cli::private_temp_root().join(crate::cli::session_temp_name(id));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::cli::PRIVATE_TEMP_OWNER), format!("{id}\n{}\n", root.display())).unwrap();
        std::fs::write(dir.join("scratch.bin"), vec![3u8; bytes]).unwrap();
        dir
    }

    #[tokio::test]
    async fn closing_a_folder_session_removes_its_temp_dir_and_only_its_own() {
        let root = std::env::temp_dir().join(format!("loka-runner-{}", Uuid::new_v4()));
        let (closed, open, foreign) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (closed_dir, open_dir) = (session_temp(&root, closed, 2_000_000), session_temp(&root, open, 10));
        let foreign_dir = session_temp(&root.join("other-runner"), foreign, 10);
        let unmarked = crate::cli::private_temp_root().join(crate::cli::session_temp_name(Uuid::new_v4()));
        std::fs::create_dir_all(&unmarked).unwrap();
        let (api, seen) = mock_api(closed).await;
        let removed = remove_closed_session_temps(&api, &root).await;
        assert_eq!(removed.count, 1);
        assert!((2_000_000..2_100_000).contains(&removed.bytes), "{}", removed.bytes);
        assert!(!closed_dir.exists() && open_dir.exists() && foreign_dir.exists() && unmarked.exists());
        let asked = seen.lock().unwrap().clone();
        let sessions: Vec<String> = asked.iter().flat_map(|(_, body)| body["sessions"].as_array().cloned().unwrap_or_default()).filter_map(|id| id.as_str().map(String::from)).collect();
        assert!(sessions.contains(&closed.to_string()) && sessions.contains(&open.to_string()));
        assert!(!sessions.contains(&foreign.to_string()), "another runner's folders are never asked about");
        let nothing_closed = remove_closed_session_temps(&api, &root).await;
        assert_eq!(nothing_closed.count, 0);
        for dir in [open_dir, foreign_dir, unmarked] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn usage_counts_a_sessions_temp_dir() {
        let root = std::env::temp_dir().join(format!("loka-runner-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("workspaces").join("tasks")).unwrap();
        let session = Uuid::new_v4();
        let dir = session_temp(&root, session, 3_000_000);
        let usage = scan(std::slice::from_ref(&root));
        assert!((3_000_000..3_100_000).contains(&usage.temp_bytes), "{}", usage.temp_bytes);
        assert!(usage.bytes >= usage.temp_bytes);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn quest_target_dir(id: Uuid) -> PathBuf {
        crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id)).join(crate::cli::QUEST_TARGET_DIR)
    }

    const NO_PROCESSES: fn() -> Option<String> = || Some(String::new());
    static NOTICE_STATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn mock_api(closed: Uuid) -> (RunnerApi, Arc<Mutex<Vec<(String, Value)>>>) {
        mock_api_knowing(closed, vec![closed]).await
    }

    async fn mock_api_knowing(closed: Uuid, known: Vec<Uuid>) -> (RunnerApi, Arc<Mutex<Vec<(String, Value)>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let log = log.clone();
                let known = known.clone();
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
                    let asked: Value = serde_json::from_slice(&plain).unwrap_or(Value::Null);
                    log.lock().unwrap().push((path.clone(), asked.clone()));
                    let reply = if path.ends_with("/workspaces") && asked["prefixes"].is_array() {
                        let asked_for = |id: &Uuid| asked["prefixes"].as_array().unwrap().iter().any(|prefix| prefix.as_str() == crate::cli::quest_temp_name(*id).strip_prefix(crate::cli::QUEST_TEMP_PREFIX));
                        let quests: Vec<Value> = known.iter().filter(|id| asked_for(id)).map(|id| json!({ "prefix": &id.simple().to_string()[..12], "id": id })).collect();
                        json!({ "quests": quests })
                    } else if path.ends_with("/workspaces") && asked["sessions"].is_array() {
                        let closed_sessions: Vec<&Value> = asked["sessions"].as_array().unwrap().iter().filter(|id| id.as_str() == Some(closed.to_string().as_str())).collect();
                        json!({ "closed_sessions": closed_sessions })
                    } else if path.ends_with("/workspaces") && (asked["pressure"] == false || !asked["tasks"].as_array().is_some_and(|tasks| tasks.iter().any(|id| id.as_str() == Some(closed.to_string().as_str())))) {
                        json!({ "closed": [], "entries": [] })
                    } else if path.ends_with("/workspaces") {
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
        let _isolated = NOTICE_STATE.lock().await;
        let quest = Uuid::new_v4();
        let root = std::env::temp_dir().join(format!("loka-guard-{}", Uuid::new_v4()));
        let tasks = root.join("workspaces").join("tasks");
        let quest_dir = tasks.join(quest.to_string());
        let temp = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(quest));
        let stage = || {
            std::fs::create_dir_all(&quest_dir).unwrap();
            std::fs::write(quest_dir.join("w.bin"), vec![1u8; 200_000]).unwrap();
            std::fs::create_dir_all(temp.join("target")).unwrap();
            std::fs::write(temp.join("target").join("t.bin"), vec![2u8; 200_000]).unwrap();
        };
        let (api, seen) = mock_api(quest).await;
        *LAST_NOTICE.lock().unwrap() = None;

        stage();
        let recovers = || if quest_dir.exists() || temp.exists() { 1.0e9 } else { 50.0e9 };
        guard_with(&api, &root, LOW_DISK_BYTES, &recovers, NO_PROCESSES()).await.expect("clearing the closed quest made room, so the run starts");
        assert!(!quest_dir.exists() && !temp.exists(), "closed quest workspace and temp dir are both gone");
        *LAST_NOTICE.lock().unwrap() = None;
        seen.lock().unwrap().clear();

        stage();
        let stays_low = || 1.0e9;
        let first = guard_with(&api, &root, LOW_DISK_BYTES, &stays_low, NO_PROCESSES()).await.unwrap_err().to_string();
        let second = guard_with(&api, &root, LOW_DISK_BYTES, &stays_low, NO_PROCESSES()).await.unwrap_err().to_string();
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
        assert!(guard_with(&api, &root, LOW_DISK_BYTES, &|| 50.0e9, NO_PROCESSES()).await.is_ok(), "plenty of room never touches the API");
        assert!(guard_with(&api, &root, 0.5e9, &|| 1.0e9, NO_PROCESSES()).await.is_ok(), "a small disk is judged by its own floor, not 5 GB");
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
    fn rebuildable_output_is_node_modules_next_cargo_targets_and_tagged_caches_and_nothing_else() {
        let base = std::env::temp_dir().join(format!("loka-build-{}", Uuid::new_v4()));
        let (task, repo) = build_tree(&base);
        std::fs::create_dir_all(repo.join("tools/.cache")).unwrap();
        std::fs::write(repo.join("tools/.cache").join(CACHE_TAG), format!("{}\n", String::from_utf8_lossy(CACHE_TAG_SIGNATURE))).unwrap();
        std::fs::create_dir_all(repo.join("tools/.other")).unwrap();
        std::fs::write(repo.join("tools/.other").join(CACHE_TAG), "not a cache tag").unwrap();
        let mut found = rebuildable_dirs(&task);
        found.sort();
        let mut expected = vec![repo.join(".next"), repo.join("node_modules"), repo.join("target"), repo.join("frontend/.next"), repo.join("frontend/node_modules"), repo.join("desktop/src-tauri/target"), repo.join("tools/.cache")];
        expected.sort();
        assert_eq!(found, expected, "docs/target has no Cargo.toml beside it, src is source and a/b/c/d is below the search depth");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_temp_folder_is_searched_for_cargo_targets_by_any_name() {
        let base = std::env::temp_dir().join(format!("loka-temp-find-{}", Uuid::new_v4()));
        let tag = format!("{}\n", String::from_utf8_lossy(CACHE_TAG_SIGNATURE));
        for dir in ["cargo-target/debug", "side-target/debug", "clone/node_modules/pkg", "pglite-data/base", "scratch/notes"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
            std::fs::write(base.join(dir).join("f.bin"), vec![1u8; 1000]).unwrap();
        }
        std::fs::write(base.join("side-target").join(CACHE_TAG), tag).unwrap();
        let mut found = rebuildable_dirs(&base);
        found.sort();
        assert_eq!(found, vec![base.join("cargo-target"), base.join("clone/node_modules"), base.join("side-target")], "test databases and scratch notes are not build output");
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
        assert!(rebuildable_dirs(&base.join("task")).is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }

    struct Quests {
        root: PathBuf,
        ids: Vec<Uuid>,
    }

    impl Quests {
        fn new() -> Self {
            Self { root: std::env::temp_dir().join(format!("loka-quests-{}", Uuid::new_v4())), ids: Vec::new() }
        }

        fn workspace(&self, id: Uuid) -> PathBuf {
            self.root.join("workspaces").join("tasks").join(id.to_string())
        }

        fn temp(&self, id: Uuid) -> PathBuf {
            crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id))
        }

        fn add(&mut self, workspace_bytes: usize, temp_bytes: usize) -> Uuid {
            let id = Uuid::new_v4();
            let repo = self.workspace(id).join("app");
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join("src").join("main.rs"), "fn main() {}").unwrap();
            std::fs::write(repo.join("unsaved.txt"), "work in progress").unwrap();
            std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
            std::fs::create_dir_all(repo.join("target")).unwrap();
            std::fs::write(repo.join("target").join("out.bin"), vec![1u8; workspace_bytes]).unwrap();
            std::fs::create_dir_all(self.temp(id).join("cargo-target")).unwrap();
            std::fs::write(self.temp(id).join("cargo-target").join("out.bin"), vec![2u8; temp_bytes]).unwrap();
            std::fs::create_dir_all(self.temp(id).join("pglite").join("base")).unwrap();
            std::fs::write(self.temp(id).join("pglite").join("base").join("rows"), "database").unwrap();
            self.ids.push(id);
            id
        }

        fn build_survives(&self, id: Uuid) -> bool {
            self.workspace(id).join("app").join("target").exists() || self.temp(id).join("cargo-target").exists()
        }

        fn only_source_left(&self, id: Uuid) -> bool {
            let repo = self.workspace(id).join("app");
            !self.build_survives(id)
                && repo.join("src").join("main.rs").is_file()
                && repo.join("unsaved.txt").is_file()
        }

        fn database_survives(&self, id: Uuid) -> bool {
            self.temp(id).join("pglite").join("base").join("rows").is_file()
        }
    }

    impl Drop for Quests {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            for id in &self.ids {
                let _ = std::fs::remove_dir_all(self.temp(*id));
            }
        }
    }

    #[tokio::test]
    async fn clearing_idle_builds_keeps_source_and_databases_and_reports_the_bytes() {
        let mut quests = Quests::new();
        let id = quests.add(300_000, 400_000);
        let freed = clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await;
        assert!(freed >= 700_000, "{freed}");
        assert!(quests.only_source_left(id) && quests.database_survives(id));
        assert_eq!(clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await, 0, "nothing is left to clear");
    }

    fn idle_for(path: &Path, minutes: u64) {
        let at = SystemTime::now() - Duration::from_secs(minutes * 60);
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                }
                std::fs::File::open(entry.path()).unwrap().set_modified(at).unwrap();
            }
        }
        std::fs::File::open(path).unwrap().set_modified(at).unwrap();
    }

    fn idle_quest(quests: &mut Quests, bytes: usize, minutes: u64) -> Uuid {
        let id = quests.add(0, bytes);
        idle_for(&quests.temp(id).join("cargo-target"), minutes);
        std::fs::remove_dir_all(quests.workspace(id).join("app").join("target")).unwrap();
        id
    }

    #[tokio::test]
    async fn builds_are_cleared_longest_idle_first_and_clearing_stops_once_there_is_room() {
        let mut quests = Quests::new();
        let (two_hours, five_minutes, a_day) = (idle_quest(&mut quests, 300_000, 120), idle_quest(&mut quests, 900_000, 5), idle_quest(&mut quests, 100_000, 1440));
        let freed = clear_idle_builds(&quests.root, None, NO_PROCESSES(), |freed| freed >= 350_000).await;
        assert!((400_000..1_300_000).contains(&freed), "{freed}");
        assert!(!quests.build_survives(a_day) && !quests.build_survives(two_hours), "the longest idle went first");
        assert!(quests.build_survives(five_minutes), "the quest that was just used stays once there is room");
    }

    #[tokio::test]
    async fn a_running_quest_keeps_its_build_output_whoever_asks_and_the_others_do_not() {
        let mut quests = Quests::new();
        let (running, idle, review_target) = (quests.add(200_000, 200_000), quests.add(200_000, 200_000), quests.add(200_000, 200_000));
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let _also_active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{review_target}/app"));
        let freed = clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await;
        assert!(freed >= 400_000);
        assert!(quests.build_survives(running) && quests.build_survives(review_target), "a running quest and a review of it are never touched");
        assert!(quests.only_source_left(idle));
        drop(_active);
        clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await;
        assert!(quests.only_source_left(running), "once the run ends its build output is fair game");
        assert!(quests.build_survives(review_target));
    }

    #[tokio::test]
    async fn a_process_in_the_quests_folders_keeps_its_build_output() {
        let mut quests = Quests::new();
        let (building, quiet) = (quests.add(200_000, 200_000), quests.add(200_000, 200_000));
        let listing = Some(format!(" 501 rustc --out-dir {}/debug/deps\n 502 zsh -l\n", quests.temp(building).join("cargo-target").display()));
        clear_idle_builds(&quests.root, None, listing, |_| false).await;
        assert!(quests.build_survives(building));
        assert!(quests.only_source_left(quiet));
        assert_eq!(clear_idle_builds(&quests.root, None, None, |_| false).await, 0, "without a process list nothing is cleared");
        assert!(quests.build_survives(building));
    }

    #[tokio::test]
    async fn a_test_database_left_running_does_not_keep_the_quests_cargo_target() {
        let mut quests = Quests::new();
        let id = quests.add(0, 400_000);
        let listing = Some(format!(" 600 node pglite-server --db {}/pglite\n", quests.temp(id).display()));
        let freed = clear_idle_builds(&quests.root, None, listing, |_| false).await;
        assert!(freed >= 400_000, "{freed}");
        assert!(!quests.build_survives(id));
        assert!(quests.temp(id).join("pglite").join("base").join("rows").is_file(), "the database itself is left for the scratch pass");
    }

    #[tokio::test]
    async fn only_the_asked_quests_are_cleared_when_a_list_is_given() {
        let mut quests = Quests::new();
        let (asked, other) = (quests.add(200_000, 200_000), quests.add(200_000, 200_000));
        let only: HashSet<String> = [crate::cli::quest_temp_name(asked)].into();
        clear_idle_builds(&quests.root, Some(only), NO_PROCESSES(), |_| false).await;
        assert!(quests.only_source_left(asked));
        assert!(quests.build_survives(other));
    }

    #[tokio::test]
    async fn a_routine_sweep_clears_the_checkout_builds_of_a_quest_an_hour_after_it_closes_and_no_one_elses() {
        let mut quests = Quests::new();
        let (closed, open, building) = (quests.add(300_000, 0), quests.add(300_000, 0), quests.add(300_000, 0));
        let listing = Some(format!(" 501 cargo build --manifest-path {}/app/Cargo.toml\n", quests.workspace(building).display()));
        let (api, _) = mock_api(closed).await;
        let removed = clear_with(&api, &quests.root, Sweep::Routine, f64::MAX, listing).await.unwrap();
        assert!(removed.cache_bytes >= 300_000, "{}", removed.cache_bytes);
        assert!(!quests.build_survives(closed), "the closed quest's checkout and temp builds are gone");
        assert!(quests.workspace(closed).join("app").join("src").join("main.rs").is_file(), "the checkout source stays until its grace ends");
        assert!(quests.workspace(closed).join("app").join("unsaved.txt").is_file());
        assert!(quests.build_survives(open), "an open quest is never touched by a routine sweep");
        let (api, _) = mock_api(building).await;
        let listing = Some(format!(" 501 cargo build --manifest-path {}/app/Cargo.toml\n", quests.workspace(building).display()));
        clear_with(&api, &quests.root, Sweep::Routine, f64::MAX, listing).await.unwrap();
        assert!(quests.build_survives(building), "a process still working in a closed quest's checkout keeps its build output");
    }

    #[tokio::test]
    async fn a_temp_folder_that_belongs_to_no_quest_of_this_runner_is_never_touched() {
        let mut quests = Quests::new();
        let id = quests.add(0, 500_000);
        std::fs::remove_dir_all(quests.workspace(id)).unwrap();
        assert_eq!(clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await, 0, "another runner of this user may own it and be running in it");
        assert!(quests.temp(id).join("cargo-target").exists());
    }

    fn unreachable_api() -> RunnerApi {
        RunnerApi { client: reqwest::Client::new(), stream_client: reqwest::Client::new(), base_url: "http://127.0.0.1:9".into(), runner_id: Uuid::new_v4(), token: "t".into() }
    }

    impl Quests {
        fn without_checkout(&mut self, temp_bytes: usize) -> Uuid {
            let id = self.add(0, temp_bytes);
            crate::cli::prepare_quest_temp_dir(&self.workspace(id).join("app")).unwrap();
            std::fs::remove_dir_all(self.workspace(id)).unwrap();
            id
        }

        fn temp_gone(&self, id: Uuid) -> bool {
            !self.temp(id).exists()
        }
    }

    #[tokio::test]
    async fn a_closed_quest_whose_checkout_is_already_gone_still_loses_its_whole_temp_folder() {
        let mut quests = Quests::new();
        let closed = quests.without_checkout(500_000);
        let open = quests.without_checkout(400_000);
        let (api, seen) = mock_api(closed).await;
        let removed = clear_with(&api, &quests.root, Sweep::Routine, f64::MAX, NO_PROCESSES()).await.unwrap();
        assert!(quests.temp_gone(closed), "the folder names its quest, so the server can say it is closed");
        assert!(removed.cleaned.contains(&closed) && removed.bytes >= 500_000, "{removed:?}");
        assert!(quests.build_survives(open) && quests.database_survives(open), "an open quest keeps everything in a routine sweep");
        let asked: Vec<String> = seen.lock().unwrap().iter().flat_map(|(_, body)| body["tasks"].as_array().cloned().unwrap_or_default()).filter_map(|id| id.as_str().map(String::from)).collect();
        assert!(asked.contains(&closed.to_string()) && asked.contains(&open.to_string()), "{asked:?}");
    }

    #[tokio::test]
    async fn an_idle_open_quest_without_a_checkout_loses_its_build_output_under_pressure_and_a_running_one_keeps_it() {
        let mut quests = Quests::new();
        let (idle, running) = (quests.without_checkout(600_000), quests.without_checkout(600_000));
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &quests.root, Sweep::Pressure, TARGET_FREE_BYTES - 100_000.0, NO_PROCESSES()).await.unwrap();
        assert!(removed.cache_bytes >= 600_000, "{}", removed.cache_bytes);
        assert!(!quests.build_survives(idle) && quests.database_survives(idle), "only rebuildable output goes once that is enough");
        assert!(quests.build_survives(running) && quests.database_survives(running));
    }

    #[test]
    fn a_quest_temp_folder_found_through_its_checkout_is_marked_and_still_found_once_the_checkout_goes() {
        let mut quests = Quests::new();
        let id = quests.add(0, 300_000);
        assert_eq!(crate::cli::private_temp_owner(&quests.temp(id)), None, "folders made before the marker existed");
        assert_eq!(quest_folders(&quests.root).iter().filter(|folder| folder.id == id).count(), 1);
        assert_eq!(crate::cli::private_temp_owner(&quests.temp(id)), Some((id, quests.root.clone())));
        std::fs::remove_dir_all(quests.workspace(id)).unwrap();
        let folders = quest_folders(&quests.root);
        let found = folders.iter().find(|folder| folder.id == id).expect("found by its marker");
        assert!(found.workspace.is_none() && found.temps == vec![quests.temp(id)]);
    }

    #[tokio::test]
    async fn another_runners_quest_folder_and_an_unmarked_orphan_are_never_touched() {
        let mut quests = Quests::new();
        let theirs = quests.add(0, 300_000);
        crate::cli::mark_private_temp_owner(&quests.temp(theirs), theirs, Path::new("/elsewhere/runner")).unwrap();
        std::fs::remove_dir_all(quests.workspace(theirs)).unwrap();
        let orphan = quests.add(0, 300_000);
        std::fs::remove_dir_all(quests.workspace(orphan)).unwrap();
        let (api, _) = mock_api(theirs).await;
        clear_with(&api, &quests.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        for id in [theirs, orphan] {
            assert!(quests.build_survives(id) && quests.database_survives(id), "{id}");
        }
        let found = find_quest_folders(&quests.root);
        assert!(found.quests.iter().all(|folder| folder.id != theirs && folder.id != orphan));
        assert!(found.unowned.contains(&quests.temp(orphan)) && !found.unowned.contains(&quests.temp(theirs)));
    }

    #[tokio::test]
    async fn an_old_unmarked_folder_is_matched_to_its_quest_by_the_server_and_cleared_like_any_other() {
        let mut quests = Quests::new();
        let [closed, open, unknown] = [(); 3].map(|_| {
            let id = quests.add(0, 400_000);
            std::fs::remove_dir_all(quests.workspace(id)).unwrap();
            id
        });
        assert!(find_quest_folders(&quests.root).unowned.contains(&quests.temp(closed)), "made before the marker, with its checkout gone");
        let (api, _) = mock_api_knowing(closed, vec![closed, open]).await;
        clear_with(&api, &quests.root, Sweep::Routine, f64::MAX, NO_PROCESSES()).await.unwrap();
        assert!(quests.temp_gone(closed), "the server named its quest and it is closed, so the whole folder goes");
        assert_eq!(crate::cli::private_temp_owner(&quests.temp(open)), Some((open, quests.root.clone())));
        assert!(quests.build_survives(open) && quests.database_survives(open), "an open quest keeps everything in a routine sweep");
        assert!(crate::cli::private_temp_owner(&quests.temp(unknown)).is_none() && quests.build_survives(unknown), "a folder no quest of this owner matches is left alone");
        clear_with(&api, &quests.root, Sweep::Pressure, TARGET_FREE_BYTES - 100_000.0, NO_PROCESSES()).await.unwrap();
        assert!(!quests.build_survives(open) && quests.database_survives(open), "once adopted, an idle open quest loses its build output under pressure");
        assert!(quests.build_survives(unknown));
    }

    #[tokio::test]
    async fn a_part_quest_and_its_parent_are_separate_and_a_review_in_flight_counts_as_running() {
        let mut quests = Quests::new();
        let (parent, part, reviewed) = (quests.add(100_000, 300_000), quests.add(100_000, 300_000), quests.add(100_000, 300_000));
        let _part_runs = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{part}/app"));
        let _review = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{reviewed}/app-2"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        clear_with(&api, &quests.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert!(quests.only_source_left(parent), "a part running does not hold its parent quest's build output");
        assert!(quests.build_survives(part) && quests.build_survives(reviewed));
    }

    #[tokio::test]
    async fn without_the_server_a_full_disk_still_clears_idle_build_output() {
        let mut quests = Quests::new();
        let idle = quests.add(200_000, 400_000);
        let removed = clear_with(&unreachable_api(), &quests.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert!(removed.cache_bytes >= 600_000, "{}", removed.cache_bytes);
        assert!(quests.only_source_left(idle));
        assert!(clear_with(&unreachable_api(), &quests.root, Sweep::Routine, f64::MAX, NO_PROCESSES()).await.is_err(), "a routine sweep never guesses without the server");
    }

    #[tokio::test]
    async fn a_refusal_names_the_program_holding_a_build_and_the_space_to_free_outside_loka() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let busy = quests.add(0, 700_000);
        let listing = Some(format!(" 900 node {}/server.js\n", quests.temp(busy).join("cargo-target").display()));
        let (api, seen) = mock_api(Uuid::new_v4()).await;
        let message = guard_with(&api, &quests.root, LOW_DISK_BYTES, &|| 1.0e9, listing).await.unwrap_err().to_string();
        assert!(quests.build_survives(busy), "a program the person runs keeps its folder");
        assert!(message.contains(&format!("quest {}", &busy.to_string()[..8])) && message.contains("quit it"), "{message}");
        assert!(message.contains("outside Loka") && message.contains("starts by itself once there is room"), "{message}");
        let calls = seen.lock().unwrap().clone();
        let notice = calls.iter().find(|(path, _)| path.ends_with("/disk")).map(|(_, body)| body.clone()).expect("one notice");
        assert_eq!(notice["kept"][0]["quest"], busy.to_string());
        assert!(notice["outside_bytes"].as_u64().unwrap() > 3_000_000_000, "{notice}");
    }

    #[tokio::test]
    async fn a_device_out_of_space_frees_itself_without_any_run_so_waiting_runs_can_start() {
        let mut quests = Quests::new();
        let (idle_open, closed_elsewhere) = (quests.add(300_000, 900_000), quests.without_checkout(900_000));
        let (api, _) = mock_api(closed_elsewhere).await;
        let free = || if quests.build_survives(idle_open) || !quests.temp_gone(closed_elsewhere) { 0.1e9 } else { 20.0e9 };
        assert!(free() < floor_for(None));
        let removed = relieve(&api, &quests.root, &free, NO_PROCESSES()).await;
        assert!(quests.only_source_left(idle_open) && quests.temp_gone(closed_elsewhere), "{removed:?}");
        assert!(free() >= floor_for(None), "the gate that holds work runs back opens on the next poll");
    }

    #[tokio::test]
    async fn a_refusal_with_only_running_quests_left_asks_nothing_of_the_person() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let running = quests.add(0, 300_000);
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let floor = 1.0e9 + 100_000.0;
        let message = guard_with(&api, &quests.root, floor, &|| 1.0e9, NO_PROCESSES()).await.unwrap_err().to_string();
        assert!(message.contains("running now"), "{message}");
        for asked in ["quit", "delete", "outside Loka"] {
            assert!(!message.contains(asked), "the running quest frees enough when it ends: {message}");
        }
    }

    #[tokio::test]
    async fn a_full_disk_clears_every_idle_quest_whatever_its_status_and_leaves_the_running_one() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let (open, failed, in_review, running) = (quests.add(100_000, 600_000), quests.add(100_000, 900_000), quests.add(100_000, 300_000), quests.add(100_000, 300_000));
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &quests.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert!(removed.cache_bytes >= 1_800_000, "{}", removed.cache_bytes);
        for id in [open, failed, in_review] {
            assert!(quests.only_source_left(id), "{id}");
        }
        assert!(quests.build_survives(running) && quests.database_survives(running), "the running quest keeps its build output and scratch");
        for id in [open, failed, in_review] {
            assert!(!quests.database_survives(id), "nothing was enough, so the idle quests' test scratch went too: {id}");
        }
    }

    #[tokio::test]
    async fn a_full_disk_clears_the_longest_idle_quest_first_and_keeps_the_one_just_used() {
        let mut quests = Quests::new();
        let (just_used, two_hours) = (idle_quest(&mut quests, 1_000_000, 5), idle_quest(&mut quests, 1_000_000, 120));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &quests.root, Sweep::Pressure, TARGET_FREE_BYTES - 500_000.0, NO_PROCESSES()).await.unwrap();
        assert!(removed.cache_bytes >= 1_000_000);
        assert!(quests.only_source_left(two_hours));
        assert!(quests.build_survives(just_used), "the quest idle for 5 minutes keeps its build once there is room");
        assert!(quests.database_survives(two_hours) && quests.database_survives(just_used), "scratch is only touched when builds were not enough");
    }

    #[tokio::test]
    async fn clearing_the_builds_is_not_enough_so_scratch_goes_next_and_the_quest_that_runs_is_left_alone() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let (idle, running) = (quests.add(100_000, 400_000), quests.add(100_000, 400_000));
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, seen) = mock_api(Uuid::new_v4()).await;
        let message = guard_with(&api, &quests.root, LOW_DISK_BYTES, &|| 1.0e9, NO_PROCESSES()).await.unwrap_err().to_string();
        assert!(message.contains("Loka cleared") && message.contains("running now"), "{message}");
        assert!(quests.only_source_left(idle) && !quests.database_survives(idle) && !quests.temp(idle).exists());
        assert!(quests.build_survives(running) && quests.database_survives(running));
        let calls = seen.lock().unwrap().clone();
        let notice = calls.iter().find(|(path, _)| path.ends_with("/disk")).map(|(_, body)| body.clone()).expect("one notice");
        assert_eq!(notice["still_low"], true);
        assert!(notice["caches_bytes"].as_u64().unwrap() >= 400_000, "{notice}");
        assert!(notice["held_bytes"].as_u64().unwrap() >= 400_000, "what is left is named: {notice}");
    }

    #[tokio::test]
    async fn a_quest_whose_scratch_was_cleared_gets_a_fresh_temp_dir_and_build_folder() {
        let mut quests = Quests::new();
        let id = quests.add(0, 400_000);
        let (api, _) = mock_api(Uuid::new_v4()).await;
        clear_with(&api, &quests.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert!(!quests.temp(id).exists());
        let target = crate::cli::prepare_quest_temp_dir(&quests.workspace(id).join("app")).unwrap().expect("a quest folder gets a build folder");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("rebuilt.bin"), b"x").unwrap();
        assert!(target.join("rebuilt.bin").is_file() && quests.workspace(id).join("app").join("unsaved.txt").is_file());
    }

    #[tokio::test]
    async fn pressure_stops_clearing_quest_builds_once_there_is_margin() {
        let mut quests = Quests::new();
        let (big, small) = (quests.add(0, 1_000_000), quests.add(0, 100_000));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &quests.root, Sweep::Pressure, TARGET_FREE_BYTES - 500_000.0, NO_PROCESSES()).await.unwrap();
        assert!(removed.cache_bytes >= 1_000_000);
        assert!(!quests.temp(big).join("cargo-target").exists());
        assert!(quests.temp(small).join("cargo-target").exists(), "enough was freed, so the smaller one stays");
    }

    #[tokio::test]
    async fn a_resumed_quest_gets_its_build_folder_back_after_it_was_cleared() {
        let mut quests = Quests::new();
        let id = quests.add(0, 400_000);
        let working_dir = quests.workspace(id).join("app");
        clear_idle_builds(&quests.root, None, NO_PROCESSES(), |_| false).await;
        assert!(!quests.temp(id).join("cargo-target").exists());
        let target = crate::cli::prepare_quest_temp_dir(&working_dir).unwrap().expect("a quest folder gets a build folder");
        assert_eq!(target, quests.temp(id).join("cargo-target"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("rebuilt.bin"), b"x").unwrap();
        assert!(quests.workspace(id).join("app").join("unsaved.txt").is_file() && quests.temp(id).join("pglite").join("base").join("rows").is_file());
    }

    #[test]
    fn usage_counts_idle_build_output_as_reclaimable_and_skips_the_quest_that_is_running() {
        let mut quests = Quests::new();
        let running = quests.add(200_000, 300_000);
        quests.add(200_000, 300_000);
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let usage = scan(&[quests.root.clone()]);
        assert!(usage.build_bytes >= 500_000, "{}: the idle quest's workspace and temp output", usage.build_bytes);
        assert!(usage.build_bytes < 1_000_000, "{}: the running quest's 500 KB is not counted", usage.build_bytes);
        assert!(usage.temp_bytes >= 600_000, "{}", usage.temp_bytes);
        assert_eq!(usage.tasks, 2);
    }

    #[tokio::test]
    async fn a_refused_run_says_nothing_was_left_only_when_that_is_true() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let running = quests.add(100_000, 400_000);
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let message = guard_with(&api, &quests.root, LOW_DISK_BYTES, &|| 1.0e9, NO_PROCESSES()).await.unwrap_err().to_string();
        assert!(message.contains("nothing Loka could safely clear"), "{message}");
        assert!(message.contains("running now"), "{message}");

        *LAST_NOTICE.lock().unwrap() = None;
        let idle = quests.add(100_000, 400_000);
        let message = guard_with(&api, &quests.root, LOW_DISK_BYTES, &|| 1.0e9, NO_PROCESSES()).await.unwrap_err().to_string();
        assert!(message.contains("Loka cleared") && !message.contains("nothing Loka could"), "{message}");
        assert!(quests.only_source_left(idle));
    }

    #[tokio::test]
    async fn a_run_that_is_about_to_start_may_have_its_own_quest_cleared_but_a_running_one_may_not() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let mut quests = Quests::new();
        let (starting, running) = (quests.add(0, 600_000), quests.add(0, 600_000));
        let _active = activate(&quests.root, Uuid::new_v4(), &format!("tasks/{running}/app"));
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let free = || 1.0e9 + if quests.build_survives(starting) { 0.0 } else { 5.0e9 };
        guard_with(&api, &quests.root, LOW_DISK_BYTES, &free, NO_PROCESSES()).await.expect("the starting quest's old build folder made room");
        assert!(quests.only_source_left(starting));
        assert!(quests.build_survives(running));
    }

    #[test]
    fn leases_that_end_release_their_quest() {
        let quest = Uuid::new_v4();
        let name = crate::cli::quest_temp_name(quest);
        let lease = Uuid::new_v4();
        let root = Path::new("/nonexistent");
        let active = activate(root, lease, &format!("tasks/{quest}/app"));
        assert!(quest_running(&name));
        drop(active);
        assert!(!quest_running(&name));
        let _folder = activate(root, Uuid::new_v4(), "folders/not-a-quest");
        assert!(!quest_running("folders/not-a-quest"));
    }

    #[test]
    fn a_process_running_in_a_cache_dir_marks_it_in_use() {
        let dir = Path::new("/c/loka-cargo-target/world");
        let listing = " 301 rustc --out-dir /c/loka-cargo-target/world/debug/deps\n 302 /c/loka-cargo-target/world2/debug/x\n 303 zsh -l\n";
        assert!(in_use(listing, dir));
        assert!(!in_use(" 302 /c/loka-cargo-target/world2/debug/x\n", dir));
    }

    struct Disk {
        root: PathBuf,
        quest: Uuid,
        seed: PathBuf,
    }

    impl Disk {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("loka-tiers-{}", Uuid::new_v4()));
            let quest = Uuid::new_v4();
            let source = root.join("workspaces").join("tasks").join(quest.to_string()).join("app");
            std::fs::create_dir_all(source.join("src")).unwrap();
            std::fs::write(source.join("src").join("main.rs"), "fn main() {}").unwrap();
            std::fs::write(source.join("unsaved.txt"), "work in progress").unwrap();
            let target = quest_target_dir(quest).join("debug");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("lib.rlib"), vec![3u8; 300_000]).unwrap();
            let seed = root.join("caches").join("cargo").join("acme__app");
            std::fs::create_dir_all(seed.join("debug").join("deps")).unwrap();
            std::fs::write(seed.join("debug").join("deps").join("dep.rlib"), vec![4u8; 300_000]).unwrap();
            Self { root, quest, seed }
        }

        fn source_survives(&self) -> bool {
            let app = self.root.join("workspaces").join("tasks").join(self.quest.to_string()).join("app");
            app.join("src").join("main.rs").is_file() && app.join("unsaved.txt").is_file()
        }

        fn cache_survives(&self) -> bool {
            quest_target_dir(self.quest).exists()
        }

        fn seed_survives(&self) -> bool {
            self.seed.exists()
        }
    }

    impl Drop for Disk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(crate::cli::private_temp_root().join(crate::cli::quest_temp_name(self.quest)));
        }
    }

    #[tokio::test]
    async fn plenty_of_space_never_touches_a_cache() {
        let disk = Disk::new();
        let (api, seen) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &disk.root, Sweep::Routine, f64::MAX, NO_PROCESSES()).await.unwrap();
        assert_eq!((removed.cache_bytes, removed.count), (0, 0));
        assert!(disk.cache_survives() && disk.seed_survives() && disk.source_survives());
        drop(seen);
        guard_with(&api, &disk.root, LOW_DISK_BYTES, &|| 50.0e9, NO_PROCESSES()).await.unwrap();
        assert!(disk.cache_survives() && disk.seed_survives());
    }

    #[tokio::test]
    async fn low_space_clears_quest_builds_before_shared_caches_and_stops_once_there_is_room() {
        let disk = Disk::new();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &disk.root, Sweep::Pressure, TARGET_FREE_BYTES - 100.0, NO_PROCESSES()).await.unwrap();
        assert!(!disk.cache_survives(), "an idle quest's build output goes first");
        assert!(disk.seed_survives(), "the shared dependency cache stays while that was enough");
        assert!(disk.source_survives());
        assert!(removed.cache_bytes >= 300_000);
    }

    #[tokio::test]
    async fn low_space_with_more_to_free_clears_every_idle_build_but_never_source_or_unsaved_work() {
        let disk = Disk::new();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &disk.root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert!(!disk.seed_survives() && !disk.cache_survives());
        assert!(removed.cache_bytes >= 600_000, "{}", removed.cache_bytes);
        assert!(disk.source_survives(), "source and uncommitted work are never touched");
    }

    #[tokio::test]
    async fn a_build_that_is_running_keeps_its_output_even_when_space_is_short() {
        let disk = Disk::new();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let busy = Some(format!(" 501 rustc --out-dir {}/debug/deps\n", quest_target_dir(disk.quest).display()));
        clear_with(&api, &disk.root, Sweep::Pressure, 0.0, busy).await.unwrap();
        assert!(disk.cache_survives() && disk.source_survives());
        assert!(!disk.seed_survives());
    }

    #[tokio::test]
    async fn low_space_with_nothing_to_free_removes_nothing_and_leaves_the_work_alone() {
        let root = std::env::temp_dir().join(format!("loka-nothing-{}", Uuid::new_v4()));
        let work = root.join("workspaces").join("tasks").join(Uuid::new_v4().to_string()).join("app");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("main.rs"), "fn main() {}").unwrap();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &root, Sweep::Pressure, 0.0, NO_PROCESSES()).await.unwrap();
        assert_eq!((removed.cache_bytes, removed.bytes), (0, 0), "there was nothing rebuildable to free, which is what makes the guard refuse the run");
        assert!(work.join("main.rs").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn clearing_caches_removes_every_build_and_seed_and_asks_the_server_nothing() {
        let disk = Disk::new();
        let (api, seen) = mock_api(Uuid::new_v4()).await;
        let removed = clear_with(&api, &disk.root, Sweep::Caches, f64::MAX, NO_PROCESSES()).await.unwrap();
        assert!(!disk.seed_survives() && !disk.cache_survives());
        assert!(disk.source_survives(), "workspaces are not caches");
        assert_eq!((removed.count, removed.bytes), (0, removed.cache_bytes));
        assert!(removed.cache_bytes >= 600_000);
        assert!(seen.lock().unwrap().is_empty(), "no closed-quest lookup is needed to clear caches");
        let modes = |workspaces, caches| modes_for(&disk.root, workspaces, caches);
        assert_eq!(modes(false, true), vec![Sweep::Caches]);
        assert_eq!(modes(true, false), vec![Sweep::Everything]);
        assert_eq!(modes(true, true), vec![Sweep::Caches, Sweep::Everything]);
    }

    #[tokio::test]
    async fn a_caches_request_polled_with_a_workspaces_request_still_clears_the_caches() {
        let disk = Disk::new();
        let (api, _) = mock_api(disk.quest).await;
        for mode in modes_for(&disk.root, true, true) {
            clear_with(&api, &disk.root, mode, f64::MAX, NO_PROCESSES()).await.unwrap();
        }
        assert!(!disk.seed_survives() && !disk.cache_survives(), "the confirmed caches cleanup ran");
        assert!(!disk.source_survives(), "and so did the closed quest workspace");

        let kept = Disk::new();
        clear_with(&api, &kept.root, Sweep::Everything, f64::MAX, NO_PROCESSES()).await.unwrap();
        assert!(kept.seed_survives(), "a workspaces cleanup alone never touches the shared caches");
    }

    #[tokio::test]
    async fn a_low_start_with_caches_to_free_lets_the_run_start_and_keeps_the_work() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let disk = Disk::new();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let free = || 1.0e9 + if disk.seed_survives() { 0.0 } else { 5.0e9 } + if disk.cache_survives() { 0.0 } else { 5.0e9 };
        guard_with(&api, &disk.root, LOW_DISK_BYTES, &free, NO_PROCESSES()).await.expect("clearing caches made room, so the run starts");
        assert!(!disk.seed_survives() && !disk.cache_survives());
        assert!(disk.source_survives(), "source and unsaved work are never touched");
    }

    #[tokio::test]
    async fn a_low_start_with_nothing_to_free_refuses_the_run_before_it_starts() {
        let _isolated = NOTICE_STATE.lock().await;
        *LAST_NOTICE.lock().unwrap() = None;
        let root = std::env::temp_dir().join(format!("loka-nothing-{}", Uuid::new_v4()));
        let work = root.join("workspaces").join("tasks").join(Uuid::new_v4().to_string()).join("app");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("main.rs"), "fn main() {}").unwrap();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let message = guard_with(&api, &root, LOW_DISK_BYTES, &|| 1.0e9, NO_PROCESSES()).await.unwrap_err().to_string();
        assert!(message.contains("did not start this run"), "{message}");
        assert!(work.join("main.rs").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn pressure_stops_when_real_free_space_is_enough_and_goes_on_while_it_is_not() {
        let enough_after_the_build = Disk::new();
        let (api, _) = mock_api(Uuid::new_v4()).await;
        let nearly = TARGET_FREE_BYTES - 100.0;
        let quest = enough_after_the_build.quest;
        let free = || if quest_target_dir(quest).exists() { nearly } else { TARGET_FREE_BYTES + 1.0 };
        relieve(&api, &enough_after_the_build.root, &free, NO_PROCESSES()).await;
        assert!(!enough_after_the_build.cache_survives());
        assert!(enough_after_the_build.seed_survives(), "the real free space was enough, so the shared cache stays");

        let still_short = Disk::new();
        relieve(&api, &still_short.root, &|| nearly, NO_PROCESSES()).await;
        assert!(!still_short.seed_survives() && !still_short.cache_survives(), "the estimate said enough but the disk said no, so another pass cleared more");
        assert!(still_short.source_survives());
    }
}
