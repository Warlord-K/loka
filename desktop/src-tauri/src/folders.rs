use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::WebviewWindow;
use uuid::Uuid;

use crate::native::{require_trusted, workspaces_root};

pub const PREFIX: &str = "folders/";
const FILE: &str = "folders.json";
const MAX_FOLDERS: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedFolder {
    pub id: Uuid,
    pub path: PathBuf,
    pub name: String,
}

fn registry(runner_root: &Path) -> PathBuf {
    runner_root.join(FILE)
}

pub fn read(runner_root: &Path) -> Vec<SharedFolder> {
    fs::read(registry(runner_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write(runner_root: &Path, folders: &[SharedFolder]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(folders).map_err(|e| e.to_string())?;
    let target = registry(runner_root);
    let temporary = target.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    fs::rename(&temporary, &target).map_err(|e| e.to_string())
}

const HIDDEN: [&str; 5] = [".ssh", ".aws", ".kube", ".gnupg", "Library"];

pub fn runner_root_of(workspaces: &Path) -> PathBuf {
    workspaces.parent().map(Path::to_path_buf).unwrap_or_else(|| workspaces.to_path_buf())
}

pub fn share_path(runner_root: &Path, path: &Path) -> Result<FolderView, String> {
    let path = allowed(path, runner_root)?;
    let mut folders = read(runner_root);
    if let Some(existing) = folders.iter().find(|f| f.path == path) {
        return Ok(view(existing.clone()));
    }
    if folders.len() >= MAX_FOLDERS {
        return Err(format!("You can share up to {MAX_FOLDERS} folders. Remove one first."));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Folder".into());
    let folder = SharedFolder { id: Uuid::new_v4(), path, name };
    folders.push(folder.clone());
    write(runner_root, &folders)?;
    Ok(view(folder))
}

pub fn unshare(runner_root: &Path, id: Uuid) -> Result<Vec<FolderView>, String> {
    let mut folders = read(runner_root);
    folders.retain(|f| f.id != id);
    write(runner_root, &folders)?;
    Ok(views(folders))
}

pub fn allowed(path: &Path, runner_root: &Path) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|_| "That folder does not exist.".to_string())?;
    if !path.is_dir() {
        return Err("Choose a folder.".into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).and_then(|h| fs::canonicalize(h).ok());
    if path.parent().is_none() || home.as_deref() == Some(path.as_path()) {
        return Err("Choose a project folder, not your whole disk or home folder.".into());
    }
    let runner = fs::canonicalize(runner_root).unwrap_or_else(|_| runner_root.to_path_buf());
    if path.starts_with(&runner) || runner.starts_with(&path) {
        return Err("Loka's own folder cannot be shared.".into());
    }
    if let Some(home) = home {
        for hidden in HIDDEN {
            if path.starts_with(home.join(hidden)) {
                return Err("That folder holds private settings and cannot be shared.".into());
            }
        }
    }
    Ok(path)
}

pub fn resolve(workspaces: &Path, key: &str) -> Option<Result<(String, PathBuf), String>> {
    let rest = key.strip_prefix(PREFIX)?;
    let Ok(id) = rest.parse::<Uuid>() else {
        return Some(Err("Invalid session.".into()));
    };
    let runner_root = runner_root_of(workspaces);
    let Some(folder) = read(&runner_root).into_iter().find(|f| f.id == id) else {
        return Some(Err("This folder is no longer shared on this Mac. Add it again under Workshop.".into()));
    };
    Some(allowed(&folder.path, &runner_root).map(|path| (format!("{PREFIX}{id}"), path)))
}

fn expand_any(home: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    let path = if raw == "~" || raw.starts_with("~/") {
        expand_in(home, raw)?
    } else if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        return Err("Give a full path on this Mac.".into());
    };
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)) {
        return Err("That path is not valid.".into());
    }
    Ok(path)
}

fn located(home: &Path, runner_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(expand_any(home, raw)?).map_err(|_| "That file is not on this Mac.".to_string())?;
    if !path.starts_with(home) || path == home {
        return Err("Only files inside your home folder can be opened.".into());
    }
    let runner = fs::canonicalize(runner_root).unwrap_or_else(|_| runner_root.to_path_buf());
    let dir = if path.is_dir() { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone()) };
    if private_dir(&dir, home, &runner) {
        return Err("That file is in a private folder.".into());
    }
    Ok(path)
}

fn project_root(path: &Path, home: &Path) -> PathBuf {
    let start = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf()) };
    let mut at = start.as_path();
    while at != home && at.starts_with(home) {
        if at.join(".git").exists() {
            return at.to_path_buf();
        }
        match at.parent() {
            Some(parent) => at = parent,
            None => break,
        }
    }
    start
}

#[derive(Serialize)]
pub struct PlacedPath {
    folder: FolderView,
    file: Option<String>,
    is_dir: bool,
    shared_now: bool,
}

pub fn place_path_in(home: &Path, runner_root: &Path, raw: &str) -> Result<PlacedPath, String> {
    let path = located(home, runner_root, raw)?;
    let shared = read(runner_root)
        .into_iter()
        .filter_map(|f| fs::canonicalize(&f.path).ok().map(|p| (p, f)))
        .filter(|(root, _)| path.starts_with(root) && allowed(root, runner_root).is_ok())
        .max_by_key(|(root, _)| root.components().count());
    let (root, folder, shared_now) = match shared {
        Some((root, folder)) => (root, view(folder), false),
        None => {
            let root = project_root(&path, home);
            if root == home {
                return Err("Files right in your home folder cannot open in the Workshop. Share their folder first.".into());
            }
            let folder = share_path(runner_root, &root)?;
            (fs::canonicalize(&root).unwrap_or(root), folder, true)
        }
    };
    let file = path.strip_prefix(&root).ok().map(|rest| rest.to_string_lossy().to_string()).filter(|rest| !rest.is_empty());
    Ok(PlacedPath { folder, file, is_dir: path.is_dir(), shared_now })
}

pub fn place_path(runner_root: &Path, raw: &str) -> Result<PlacedPath, String> {
    let home = home_dir().ok_or_else(|| "This Mac has no home folder.".to_string())?;
    place_path_in(&home, runner_root, raw)
}

pub fn reveal(runner_root: &Path, raw: &str) -> Result<(), String> {
    let home = home_dir().ok_or_else(|| "This Mac has no home folder.".to_string())?;
    let path = located(&home, runner_root, raw)?;
    let status = std::process::Command::new("/usr/bin/open").arg("-R").arg(&path).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("Finder could not show that file.".into()) }
}

fn shown(path: &Path) -> String {
    let full = path.to_string_lossy().to_string();
    match std::env::var("HOME").ok().filter(|h| !h.is_empty()).and_then(|h| full.strip_prefix(&h).map(String::from)) {
        Some(rest) => format!("~{rest}"),
        None => full,
    }
}

#[derive(Serialize)]
pub struct FolderView {
    id: Uuid,
    name: String,
    path: String,
}

fn view(folder: SharedFolder) -> FolderView {
    FolderView { id: folder.id, path: shown(&folder.path), name: folder.name }
}

fn views(folders: Vec<SharedFolder>) -> Vec<FolderView> {
    folders.into_iter().map(view).collect()
}

async fn guarded_root(window: &WebviewWindow) -> Result<PathBuf, String> {
    require_trusted(window)?;
    Ok(runner_root_of(&workspaces_root(window)?))
}

#[tauri::command]
pub async fn folders_list(window: WebviewWindow) -> Result<Vec<FolderView>, String> {
    let root = guarded_root(&window).await?;
    Ok(views(read(&root)))
}

#[tauri::command]
pub async fn folder_add(window: WebviewWindow) -> Result<Option<FolderView>, String> {
    let root = guarded_root(&window).await?;
    let Some(picked) = rfd::AsyncFileDialog::new().set_title("Share a folder with your agents").pick_folder().await else {
        return Ok(None);
    };
    share_path(&root, picked.path()).map(Some)
}

#[tauri::command]
pub async fn folder_remove(window: WebviewWindow, id: Uuid) -> Result<Vec<FolderView>, String> {
    let root = guarded_root(&window).await?;
    unshare(&root, id)
}

#[tauri::command]
pub async fn folder_for_path(window: WebviewWindow, path: String) -> Result<PlacedPath, String> {
    let root = guarded_root(&window).await?;
    tauri::async_runtime::spawn_blocking(move || place_path(&root, &path)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn reveal_path(window: WebviewWindow, path: String) -> Result<(), String> {
    let root = guarded_root(&window).await?;
    tauri::async_runtime::spawn_blocking(move || reveal(&root, &path)).await.map_err(|e| e.to_string())?
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|h| !h.as_os_str().is_empty()).and_then(|h| fs::canonicalize(h).ok())
}

fn expand_in(home: &Path, shown: &str) -> Result<PathBuf, String> {
    let shown = shown.trim();
    let rest = match shown {
        "" | "~" => "",
        _ => shown.strip_prefix("~/").ok_or_else(|| "Pick a folder inside your home folder.".to_string())?,
    };
    let rest = Path::new(rest);
    if rest.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err("That folder path is not valid.".into());
    }
    Ok(home.join(rest))
}

fn private_dir(path: &Path, home: &Path, runner: &Path) -> bool {
    let dotted = path
        .strip_prefix(home)
        .ok()
        .and_then(|rest| rest.components().next())
        .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('.'));
    dotted
        || HIDDEN.iter().any(|hidden| path.starts_with(home.join(hidden)))
        || path.starts_with(runner)
        || runner.starts_with(path) && path != home
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_registered_folders() {
        let base = std::env::temp_dir().join(format!("world-folders-{}", Uuid::new_v4()));
        let runner = base.join("runner");
        let workspaces = runner.join("workspaces");
        let project = base.join("project");
        fs::create_dir_all(&workspaces).unwrap();
        fs::create_dir_all(&project).unwrap();
        let id = Uuid::new_v4();
        write(&runner, &[SharedFolder { id, path: project.clone(), name: "project".into() }]).unwrap();
        let (key, root) = resolve(&workspaces, &format!("folders/{id}")).unwrap().unwrap();
        assert_eq!(key, format!("folders/{id}"));
        assert_eq!(root, fs::canonicalize(&project).unwrap());
        assert!(resolve(&workspaces, &format!("folders/{}", Uuid::new_v4())).unwrap().is_err());
        assert!(resolve(&workspaces, "folders/../x").unwrap().is_err());
        assert!(resolve(&workspaces, "tasks/x").is_none());
        assert!(allowed(&runner, &runner).is_err());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn paths_open_in_the_folder_that_holds_them() {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("world-place-{}", Uuid::new_v4()));
        let home = base.join("home");
        let runner = home.join("Library/Application Support/dev.ygpt.loka/runner");
        for dir in [home.join("code/app/.git"), home.join("code/app/src/deep"), home.join("loose/notes"), home.join(".ssh"), runner.clone(), base.join("outside")] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(home.join("code/app/src/deep/main.rs"), "fn main() {}").unwrap();
        fs::write(home.join("loose/notes/todo.md"), "x").unwrap();
        fs::write(home.join("top.txt"), "x").unwrap();
        fs::write(home.join(".ssh/id"), "x").unwrap();
        fs::write(base.join("outside/x.txt"), "x").unwrap();

        let first = place_path_in(&home, &runner, "~/code/app/src/deep/main.rs").unwrap();
        assert!(first.shared_now);
        assert_eq!(first.folder.name, "app");
        assert_eq!(first.file.as_deref(), Some("src/deep/main.rs"));

        let again = place_path_in(&home, &runner, &home.join("code/app/src").to_string_lossy()).unwrap();
        assert!(!again.shared_now);
        assert_eq!(again.folder.id, first.folder.id);
        assert_eq!(again.file.as_deref(), Some("src"));
        assert!(again.is_dir);
        assert!(!first.is_dir);

        let loose = place_path_in(&home, &runner, "~/loose/notes/todo.md").unwrap();
        assert_eq!(loose.folder.name, "notes");
        assert_eq!(loose.file.as_deref(), Some("todo.md"));

        for bad in ["~/top.txt", "~/.ssh/id", "~/missing.rs", "relative/path.rs", "~/code/../code/app", &base.join("outside/x.txt").to_string_lossy().to_string(), "/etc/hosts"] {
            assert!(place_path_in(&home, &runner, bad).is_err(), "{bad}");
        }
        assert_eq!(read(&runner).len(), 2);
        fs::remove_dir_all(&base).unwrap();
    }
}
