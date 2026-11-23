use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use loka_shared::run_recording::{free_bytes, run_floor, LOW_DISK_BYTES};
use tracing::warn;
use uuid::Uuid;

pub(crate) const TARGET_FREE_BYTES: f64 = 2.0 * LOW_DISK_BYTES;

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub checkouts: PathBuf,
    pub owner: PathBuf,
}

impl Layout {
    pub fn quest_of(&self, dir: &Path) -> Option<Uuid> {
        dir.strip_prefix(&self.checkouts).ok()?.components().next()?.as_os_str().to_str()?.parse().ok()
    }

    pub fn runner(root: &Path) -> Self {
        Self { checkouts: root.join("workspaces").join("tasks"), owner: root.to_path_buf() }
    }

    pub fn local(workspaces_dir: &Path) -> Self {
        Self { checkouts: workspaces_dir.to_path_buf(), owner: workspaces_dir.to_path_buf() }
    }
}

pub(crate) struct TempFolder {
    pub(crate) path: PathBuf,
    pub(crate) owner: Option<(Uuid, PathBuf)>,
}

pub(crate) fn quest_temp_dirs() -> HashMap<String, Vec<TempFolder>> {
    let mut found: HashMap<String, Vec<TempFolder>> = HashMap::new();
    for home in crate::cli::private_temp_homes() {
        let Ok(entries) = std::fs::read_dir(home) else {
            continue;
        };
        for entry in entries.flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with(crate::cli::QUEST_TEMP_PREFIX)) {
            let path = entry.path();
            found.entry(entry.file_name().to_string_lossy().into_owned()).or_default().push(TempFolder { owner: crate::cli::private_temp_owner(&path), path });
        }
    }
    found
}

pub(crate) struct QuestFolder {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) temps: Vec<PathBuf>,
}

impl QuestFolder {
    pub(crate) fn dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.workspace.iter().chain(self.temps.iter())
    }

    pub(crate) fn rebuildable(&self) -> Vec<PathBuf> {
        self.dirs().flat_map(|dir| rebuildable_dirs(dir)).collect()
    }

    pub(crate) fn kept_by_a_process(&self, listing: &str, build: &Path) -> bool {
        in_use(listing, build)
            || build
                .parent()
                .is_some_and(|project| self.workspace.as_deref().is_some_and(|workspace| project.starts_with(workspace)) && in_use(listing, project))
    }
}

pub(crate) fn touched(dir: &Path) -> SystemTime {
    let modified = |path: &Path| std::fs::symlink_metadata(path).and_then(|meta| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let mut newest = modified(dir);
    let Ok(children) = std::fs::read_dir(dir) else {
        return newest;
    };
    for child in children.flatten() {
        newest = newest.max(modified(&child.path()));
        if let Ok(grandchildren) = std::fs::read_dir(child.path()) {
            for grandchild in grandchildren.flatten() {
                newest = newest.max(modified(&grandchild.path()));
            }
        }
    }
    newest
}

pub(crate) struct QuestFolders {
    pub(crate) quests: Vec<QuestFolder>,
    pub(crate) unowned: Vec<PathBuf>,
}

pub(crate) fn find_quest_folders(layout: &Layout) -> QuestFolders {
    let mut temps = quest_temp_dirs();
    let mut quests: Vec<QuestFolder> = std::fs::read_dir(&layout.checkouts)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().parse::<Uuid>().ok()?;
            let name = crate::cli::quest_temp_name(id);
            let temps = temps
                .remove(&name)
                .unwrap_or_default()
                .into_iter()
                .filter(|temp| match &temp.owner {
                    Some((_, place)) => place == &layout.owner,
                    None => {
                        let _ = crate::cli::mark_private_temp_owner(&temp.path, id, &layout.owner);
                        true
                    }
                })
                .map(|temp| temp.path)
                .collect();
            Some(QuestFolder { id, name, workspace: Some(entry.path()), temps })
        })
        .collect();
    let mut unowned = Vec::new();
    for (name, found) in temps {
        let mut mine: Vec<(Uuid, PathBuf)> = Vec::new();
        for temp in found {
            match temp.owner {
                Some((id, place)) if place == layout.owner => mine.push((id, temp.path)),
                Some(_) => {}
                None => unowned.push(temp.path),
            }
        }
        if let Some((id, _)) = mine.first() {
            quests.push(QuestFolder { id: *id, name, workspace: None, temps: mine.into_iter().map(|(_, path)| path).collect() });
        }
    }
    QuestFolders { quests, unowned }
}

pub(crate) static ACTIVE: Mutex<Vec<(Uuid, String)>> = Mutex::new(Vec::new());

pub(crate) struct Active {
    lease: Uuid,
    quest: Option<(Layout, Uuid)>,
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.lock().unwrap().retain(|(lease, _)| *lease != self.lease);
        if let (Some((layout, quest)), Ok(runtime)) = (self.quest.take(), tokio::runtime::Handle::try_current()) {
            runtime.spawn(async move { quest_run_ended(&layout, quest).await });
        }
    }
}

pub(crate) fn activate(layout: &Layout, lease: Uuid, quest: Option<Uuid>) -> Active {
    if let Some(quest) = quest {
        ACTIVE.lock().unwrap().push((lease, crate::cli::quest_temp_name(quest)));
    }
    Active { lease, quest: quest.map(|quest| (layout.clone(), quest)) }
}

pub(crate) fn quest_running(name: &str) -> bool {
    ACTIVE.lock().unwrap().iter().any(|(_, running)| running == name)
}

pub(crate) const CACHE_TAG: &str = "CACHEDIR.TAG";
pub(crate) const CACHE_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
pub(crate) const BUILD_SEARCH_DEPTH: u8 = 4;

pub(crate) fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

pub(crate) fn tagged_as_cache(dir: &Path) -> bool {
    let Ok(mut tag) = std::fs::File::open(dir.join(CACHE_TAG)) else {
        return false;
    };
    let mut head = [0u8; CACHE_TAG_SIGNATURE.len()];
    std::io::Read::read_exact(&mut tag, &mut head).is_ok() && head == CACHE_TAG_SIGNATURE
}

pub(crate) fn rebuildable(dir: &Path, parent: &Path) -> bool {
    match dir.file_name().and_then(|name| name.to_str()) {
        Some("node_modules" | ".next" | crate::cli::QUEST_TARGET_DIR) => true,
        Some("target") if parent.join("Cargo.toml").is_file() => true,
        _ => tagged_as_cache(dir),
    }
}

pub(crate) fn rebuildable_below(folder: &Path, depth: u8, found: &mut Vec<PathBuf>) {
    let Ok(children) = std::fs::read_dir(folder) else {
        return;
    };
    for child in children.flatten() {
        let path = child.path();
        if child.file_name() == ".git" || !real_dir(&path) {
            continue;
        }
        if rebuildable(&path, folder) {
            found.push(path);
        } else if depth < BUILD_SEARCH_DEPTH {
            rebuildable_below(&path, depth + 1, found);
        }
    }
}

pub(crate) fn rebuildable_dirs(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    rebuildable_below(folder, 0, &mut found);
    found
}

pub(crate) async fn process_listing() -> Option<String> {
    let output = tokio::process::Command::new("ps").args(["-axo", "pid=,command="]).output().await.ok()?;
    (output.status.success() && !output.stdout.is_empty()).then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn in_use(listing: &str, dir: &Path) -> bool {
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if !dirs.contains(&real) {
            dirs.push(real);
        }
    }
    !pids_under(listing, &dirs, std::process::id()).is_empty()
}

pub(crate) struct Rebuildable {
    pub(crate) quest: String,
    pub(crate) dir: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) touched: SystemTime,
}

pub(crate) fn longest_idle_first<T>(items: &mut [T], key: impl Fn(&T) -> (SystemTime, u64)) {
    items.sort_by(|a, b| {
        let (a_touched, a_bytes) = key(a);
        let (b_touched, b_bytes) = key(b);
        a_touched.cmp(&b_touched).then(b_bytes.cmp(&a_bytes))
    });
}

pub(crate) fn idle_rebuildables(layout: &Layout, only: Option<&HashSet<String>>, listing: &str) -> Vec<Rebuildable> {
    let mut found: Vec<Rebuildable> = quest_folders(layout)
        .into_iter()
        .filter(|folder| only.is_none_or(|only| only.contains(&folder.name)))
        .flat_map(|folder| {
            let builds: Vec<PathBuf> = folder.rebuildable().into_iter().filter(|dir| !folder.kept_by_a_process(listing, dir)).collect();
            let quest = folder.name;
            builds.into_iter().map(move |dir| Rebuildable { quest: quest.clone(), bytes: dir_size(&dir), touched: touched(&dir), dir })
        })
        .collect();
    longest_idle_first(&mut found, |build| (build.touched, build.bytes));
    found
}

pub(crate) struct Scratch {
    pub(crate) quest: String,
    pub(crate) dir: PathBuf,
    pub(crate) touched: SystemTime,
}

pub(crate) fn idle_scratch(layout: &Layout, listing: &str) -> Vec<Scratch> {
    let mut found: Vec<Scratch> = quest_folders(layout)
        .into_iter()
        .filter(|folder| !folder.rebuildable().iter().any(|build| folder.kept_by_a_process(listing, build)))
        .flat_map(|folder| {
            let quest = folder.name;
            folder.temps.into_iter().map(move |dir| Scratch { quest: quest.clone(), touched: touched(&dir), dir })
        })
        .collect();
    longest_idle_first(&mut found, |scratch| (scratch.touched, 0));
    found
}

pub(crate) async fn clear_idle_scratch(layout: &Layout, listing: String, mut enough: impl FnMut(u64) -> bool) -> u64 {
    let candidates = {
        let layout = layout.clone();
        tokio::task::spawn_blocking(move || idle_scratch(&layout, &listing)).await.unwrap_or_default()
    };
    let mut freed = 0;
    for candidate in candidates {
        if enough(freed) {
            break;
        }
        if quest_running(&candidate.quest) {
            continue;
        }
        freed += remove_temp_dir(candidate.dir).await.unwrap_or(0);
    }
    freed
}

pub(crate) async fn clear_idle_builds(layout: &Layout, only: Option<HashSet<String>>, listing: Option<String>, mut enough: impl FnMut(u64) -> bool) -> u64 {
    let Some(listing) = listing else {
        warn!("Could not list processes, so no build folder was cleared");
        return 0;
    };
    let candidates = {
        let layout = layout.clone();
        tokio::task::spawn_blocking(move || idle_rebuildables(&layout, only.as_ref(), &listing)).await.unwrap_or_default()
    };
    let mut freed = 0;
    for candidate in candidates {
        if enough(freed) {
            break;
        }
        if quest_running(&candidate.quest) {
            continue;
        }
        match tokio::fs::remove_dir_all(&candidate.dir).await {
            Ok(()) => freed += candidate.bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(error = %error, dir = %candidate.dir.display(), "Failed to clear a build cache"),
        }
    }
    freed
}

pub(crate) fn dir_size(path: &Path) -> u64 {
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

pub(crate) async fn remove_temp_dir(dir: PathBuf) -> Option<u64> {
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
                warn!(error = %error, dir = %dir.display(), "Failed to remove a private temp dir");
            }
            None
        }
    }
}

pub(crate) fn mentions(command: &str, dir: &str) -> bool {
    command.match_indices(dir).any(|(at, _)| {
        command[at + dir.len()..].chars().next().is_none_or(|next| next == '/' || next == '"' || next == '\'' || next.is_whitespace())
    })
}

pub(crate) fn pids_under(listing: &str, dirs: &[String], own: u32) -> Vec<u32> {
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

pub(crate) fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn spellings(dir: &Path) -> Vec<String> {
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if !dirs.contains(&real) {
            dirs.push(real);
        }
    }
    dirs
}

async fn terminate(pids: Vec<u32>, reason: &str) {
    if pids.is_empty() {
        return;
    }
    for pid in &pids {
        unsafe { libc::kill(*pid as i32, libc::SIGTERM) };
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for pid in pids.into_iter().filter(|pid| alive(*pid)) {
        warn!(pid, reason, "Stopping a process a quest left behind");
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

pub(crate) async fn stop_processes_in(dir: &Path) {
    let Ok(output) = tokio::process::Command::new("ps").args(["-axo", "pid=,command="]).output().await else {
        return;
    };
    terminate(pids_under(&String::from_utf8_lossy(&output.stdout), &spellings(dir), std::process::id()), "its folder is being removed").await;
}

pub(crate) fn orphans_under(listing: &str, dirs: &[String], own: u32) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let (pid, parent): (u32, u32) = (pid.parse().ok()?, parent.parse().ok()?);
            (parent == 1 && pid > 1 && pid != own && dirs.iter().any(|dir| mentions(command, dir))).then_some(pid)
        })
        .collect()
}

async fn orphan_listing() -> Option<String> {
    let output = tokio::process::Command::new("ps").args(["axewwo", "pid=,ppid=,command="]).output().await.ok()?;
    (output.status.success() && !output.stdout.is_empty()).then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) async fn quest_run_ended(layout: &Layout, quest: Uuid) {
    if quest_running(&crate::cli::quest_temp_name(quest)) {
        return;
    }
    let temps: Vec<String> = quest_folders(layout).into_iter().filter(|folder| folder.id == quest).flat_map(|folder| folder.temps.iter().flat_map(|dir| spellings(dir)).collect::<Vec<_>>()).collect();
    if temps.is_empty() {
        return;
    }
    let Some(listing) = orphan_listing().await else {
        return;
    };
    terminate(orphans_under(&listing, &temps, std::process::id()), "its run ended").await;
}

pub(crate) fn quest_folders(layout: &Layout) -> Vec<QuestFolder> {
    find_quest_folders(layout).quests
}

#[derive(Debug)]
pub(crate) struct Shortfall {
    pub free: f64,
    pub freed: u64,
}

pub(crate) async fn make_room(layout: &Layout, probe: &Path) -> Result<(), Shortfall> {
    make_room_with(layout, run_floor(probe), &|| free_bytes(probe)).await
}

async fn make_room_with(layout: &Layout, floor: f64, free: &(dyn Fn() -> f64 + Send + Sync)) -> Result<(), Shortfall> {
    if free() >= floor {
        return Ok(());
    }
    let listing = process_listing().await.unwrap_or_else(|| {
        warn!("Could not list processes, clearing idle quests by their runs alone");
        String::new()
    });
    let enough = |_: u64| free() >= TARGET_FREE_BYTES;
    let builds = clear_idle_builds(layout, None, Some(listing.clone()), enough).await;
    let freed = builds + clear_idle_scratch(layout, listing, enough).await;
    match free() >= floor {
        true => Ok(()),
        false => Err(Shortfall { free: free(), freed }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Local {
        base: PathBuf,
        layout: Layout,
    }

    impl Local {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!("loka-qs-{}", Uuid::new_v4()));
            let checkouts = base.join("workspaces");
            std::fs::create_dir_all(&checkouts).unwrap();
            Self { layout: Layout::local(&checkouts), base }
        }

        fn quest(&self, build_bytes: usize) -> Uuid {
            let id = Uuid::new_v4();
            let app = self.app(id);
            std::fs::create_dir_all(app.join("src")).unwrap();
            std::fs::write(app.join("src").join("main.rs"), "fn main() {}").unwrap();
            std::fs::write(app.join("unsaved.txt"), "work in progress").unwrap();
            std::fs::write(app.join("Cargo.toml"), "[package]").unwrap();
            std::fs::create_dir_all(app.join("target")).unwrap();
            std::fs::write(app.join("target").join("out.bin"), vec![1u8; build_bytes]).unwrap();
            std::fs::create_dir_all(app.join("node_modules").join("left-pad")).unwrap();
            std::fs::write(app.join("node_modules").join("left-pad").join("index.js"), vec![1u8; build_bytes]).unwrap();
            id
        }

        fn app(&self, id: Uuid) -> PathBuf {
            self.layout.checkouts.join(id.to_string()).join("app")
        }

        fn built(&self, id: Uuid) -> bool {
            self.app(id).join("target").exists() || self.app(id).join("node_modules").exists()
        }

        fn only_source_left(&self, id: Uuid) -> bool {
            let app = self.app(id);
            !self.built(id) && app.join("src").join("main.rs").is_file() && app.join("unsaved.txt").is_file() && app.join("Cargo.toml").is_file()
        }
    }

    impl Drop for Local {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    const FLOOR: f64 = 5.0e9;

    fn room_while(quests: &[PathBuf]) -> impl Fn() -> f64 + Send + Sync {
        let apps: Vec<PathBuf> = quests.to_vec();
        move || if apps.iter().all(|app| !app.join("target").exists() && !app.join("node_modules").exists()) { 20.0e9 } else { 1.0e9 }
    }

    #[tokio::test]
    async fn a_low_disk_run_clears_every_idle_quests_build_output_and_then_starts() {
        let local = Local::new();
        let (in_review, waiting_to_merge, failed) = (local.quest(300_000), local.quest(200_000), local.quest(100_000));
        let free = room_while(&[local.app(in_review), local.app(waiting_to_merge), local.app(failed)]);
        assert!(make_room_with(&local.layout, FLOOR, &free).await.is_ok());
        for id in [in_review, waiting_to_merge, failed] {
            assert!(local.only_source_left(id), "source and unsaved work stay, build output goes");
        }
    }

    #[tokio::test]
    async fn a_quest_with_a_run_keeps_its_build_output_when_the_disk_is_low() {
        let local = Local::new();
        let (running, idle) = (local.quest(300_000), local.quest(300_000));
        let _held = activate(&local.layout, Uuid::new_v4(), Some(running));
        let free = room_while(&[local.app(idle)]);
        assert!(make_room_with(&local.layout, FLOOR, &free).await.is_ok());
        assert!(local.built(running), "a run is using it");
        assert!(local.only_source_left(idle));
    }

    #[tokio::test]
    async fn the_idle_quest_that_is_about_to_run_again_loses_its_own_build_output_and_rebuilds() {
        let local = Local::new();
        let resuming = local.quest(300_000);
        let free = room_while(&[local.app(resuming)]);
        assert!(make_room_with(&local.layout, FLOOR, &free).await.is_ok());
        assert!(local.only_source_left(resuming));
        let _run = activate(&local.layout, Uuid::new_v4(), Some(resuming));
        std::fs::create_dir_all(local.app(resuming).join("target")).unwrap();
        std::fs::write(local.app(resuming).join("target").join("out.bin"), "rebuilt").unwrap();
        assert!(local.built(resuming) && local.app(resuming).join("src").join("main.rs").is_file());
    }

    #[tokio::test]
    async fn nothing_is_cleared_while_there_is_room() {
        let local = Local::new();
        let idle = local.quest(100_000);
        assert!(make_room_with(&local.layout, FLOOR, &|| 20.0e9).await.is_ok());
        assert!(local.built(idle));
    }

    #[tokio::test]
    async fn a_disk_that_stays_full_reports_what_was_cleared_and_the_run_is_refused() {
        let local = Local::new();
        let idle = local.quest(400_000);
        let short = make_room_with(&local.layout, FLOOR, &|| 1.0e9).await.expect_err("still under the floor");
        assert!(local.only_source_left(idle));
        assert!(short.freed >= 400_000 && (short.free - 1.0e9).abs() < 1.0);
        let note = loka_shared::run_recording::cleared_note(short.freed);
        assert!(note.contains("already cleared") && !note.contains("nothing"), "{note}");
        assert_eq!(loka_shared::run_recording::cleared_note(0), "");
    }

    #[tokio::test]
    async fn a_disk_full_of_other_peoples_files_clears_nothing_it_does_not_own() {
        let local = Local::new();
        let stranger = local.base.join("not-a-quest").join("app");
        std::fs::create_dir_all(stranger.join("target")).unwrap();
        std::fs::write(stranger.join("Cargo.toml"), "[package]").unwrap();
        let short = make_room_with(&local.layout, FLOOR, &|| 1.0e9).await.expect_err("still under the floor");
        assert_eq!(short.freed, 0);
        assert!(stranger.join("target").exists());
    }

    #[test]
    fn a_local_checkout_names_its_quest_and_nothing_else_does() {
        let local = Local::new();
        let id = Uuid::new_v4();
        assert_eq!(local.layout.quest_of(&local.layout.checkouts.join(id.to_string()).join("app")), Some(id));
        assert_eq!(local.layout.quest_of(&local.layout.checkouts.join("agents").join("bunny")), None);
        assert_eq!(local.layout.quest_of(Path::new("/elsewhere")), None);
    }

    #[test]
    fn only_orphans_that_carry_the_quests_private_temp_folder_are_stopped_at_the_end_of_its_run() {
        let temp = "/t/loka-q-0123456789ab".to_string();
        let listing = [
            " 101     1 node pglite-server --port 56217 TMPDIR=/t/loka-q-0123456789ab HOME=/Users/a",
            " 102   900 node pglite-server --port 48613 TMPDIR=/t/loka-q-0123456789ab",
            " 103     1 node unrelated TMPDIR=/t/loka-q-0123456789abcd",
            " 104     1 /sbin/launchd",
            "   7     1 node pglite-server TMPDIR=/t/loka-q-0123456789ab",
            " 105     1 zsh -l PWD=/ws/0123/app TMPDIR=/t/loka-q-ffffffffffff",
            " 106     1 vim PWD=/ws/0123/app",
            " 107     1 node dev-server --data /t/loka-q-0123456789ab/pglite",
        ]
        .join("\n");
        let pids = orphans_under(&listing, &[temp], 7);
        assert_eq!(pids, vec![101, 107], "only orphans carrying this quest's temp folder, never an editor or shell that merely sits in the checkout, never ours, never a live parent's child: {pids:?}");
    }

    async fn orphan_in(dir: &Path) -> Option<u32> {
        std::fs::create_dir_all(dir).unwrap();
        let status = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("(sleep 60 </dev/null >/dev/null 2>&1 &) ; true")
            .env("TMPDIR", dir)
            .status()
            .await
            .ok()?;
        assert!(status.success());
        tokio::time::sleep(Duration::from_millis(300)).await;
        let listing = orphan_listing().await?;
        let found = orphans_under(&listing, &spellings(dir), std::process::id());
        found.first().copied()
    }

    #[tokio::test]
    async fn when_a_quests_last_run_ends_its_orphaned_test_database_stops_and_while_another_run_lives_it_stays() {
        let local = Local::new();
        let id = local.quest(1_000);
        let temp = crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id));
        std::fs::create_dir_all(&temp).unwrap();
        crate::cli::mark_private_temp_owner(&temp, id, &local.layout.owner).unwrap();
        let guard = TempGuard(temp.clone());
        let Some(pid) = orphan_in(&temp).await else {
            eprintln!("skipped: this environment does not show other processes' environments to ps");
            return;
        };
        let (first, second) = (activate(&local.layout, Uuid::new_v4(), Some(id)), activate(&local.layout, Uuid::new_v4(), Some(id)));
        drop(first);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(alive(pid), "the quest still has a run");
        drop(second);
        let mut stopped = false;
        for _ in 0..40 {
            if !alive(pid) {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !stopped {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        drop(guard);
        assert!(stopped, "the run ended, so its test database was stopped");
    }

    struct TempGuard(PathBuf);

    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
