use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use loka_shared::runners::{ClosedTask, DiskNotice};
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
    pub trash_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
struct Usage {
    bytes: u64,
    tasks: usize,
    largest: Vec<(Uuid, u64)>,
}

static USAGE: Mutex<Option<Usage>> = Mutex::new(None);
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static LAST_NOTICE: Mutex<Option<Instant>> = Mutex::new(None);

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
        "free_gb": (free / GIB).floor(),
        "total_gb": (total / GIB).floor(),
        "low": free < LOW_DISK_BYTES,
        "largest": usage.largest.iter().map(|(id, bytes)| json!({ "id": id, "bytes": bytes })).collect::<Vec<_>>(),
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

fn scan(roots: &[PathBuf]) -> Usage {
    let mut sizes: Vec<(Uuid, u64)> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("workspaces").join("tasks")) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() {
                sizes.push((id, dir_size(&entry.path())));
            }
        }
    }
    sizes.sort_by(|a, b| b.1.cmp(&a.1));
    Usage {
        bytes: sizes.iter().map(|(_, bytes)| bytes).sum(),
        tasks: sizes.len(),
        largest: sizes.into_iter().filter(|(_, bytes)| *bytes >= LISTED_MIN_BYTES).take(LISTED_MAX).collect(),
    }
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
                info!(task = %entry.task.id, status = %entry.task.status, mb = bytes / 1_048_576, "Removed closed task workspace");
                removed.count += 1;
                removed.bytes += bytes;
                if !removed.trash_roots.contains(&entry.trash_root) {
                    removed.trash_roots.push(entry.trash_root);
                }
            }
            Err(error) => warn!(error = %format!("{error:#}"), task = %entry.task.id, "Failed to remove closed task workspace"),
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

pub(super) async fn sweep(api: &RunnerApi, root: &Path, mode: Sweep, rescan: &Arc<Notify>) -> Result<()> {
    let free = disk_space(root).map(|(free, _)| free).unwrap_or(f64::MAX);
    let mut entries = Vec::new();
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
        for task in api.closed_tasks(&tasks, mode != Sweep::Routine).await? {
            entries.push(Entry { dir: tasks_dir.join(task.id.to_string()), trash_root: base.join("trash"), task });
        }
    }
    let removed = remove_entries(by_age(entries), mode, free).await;
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
            warn_owner(&api, &root, &removed).await;
        }
    });
    Ok(())
}

async fn warn_owner(api: &RunnerApi, root: &Path, removed: &Removed) {
    let free = disk_space(root).map(|(free, _)| free).unwrap_or(0.0);
    let still_low = free < LOW_DISK_BYTES;
    if removed.count == 0 && !still_low {
        return;
    }
    {
        let mut last = LAST_NOTICE.lock().unwrap();
        if last.is_some_and(|at| at.elapsed() < NOTICE_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    let notice = DiskNotice { free_bytes: free as u64, freed_bytes: removed.bytes, removed: removed.count, still_low };
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
}
