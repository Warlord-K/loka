use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::{info, warn};
use uuid::Uuid;

use super::workspaces::{dir_size, in_use, process_listing};

const SEED_FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
const SEED_MARK: &str = ".loka-seed";
const SEED_PARTS: [&str; 3] = [".fingerprint", "deps", "build"];
const BUILD_SCRIPT_UNIT: &str = "build_script_build";

static SEEDS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) fn seeds_root(root: &Path) -> PathBuf {
    root.join("caches").join("cargo")
}

fn seed_dir(root: &Path, repo: &str) -> PathBuf {
    let name: String = repo.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    seeds_root(root).join(name)
}

async fn clone_tree(from: &Path, to: &Path) -> bool {
    let mut command = tokio::process::Command::new("cp");
    if cfg!(target_os = "macos") {
        command.arg("-cR");
    } else {
        command.args(["-a", "--reflink=always"]);
    }
    let cloned = command.arg(from).arg(to).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await;
    if cloned.is_ok_and(|status| status.success()) {
        return true;
    }
    let _ = tokio::fs::remove_dir_all(to).await;
    false
}

pub(super) async fn seed_quest(root: &Path, repo: &str, working_dir: &Path) {
    let Ok(Some(target)) = crate::cli::prepare_quest_temp_dir(working_dir) else {
        return;
    };
    if target.exists() {
        return;
    }
    let seed = seed_dir(root, repo);
    let _one_at_a_time = SEEDS.lock().await;
    if !seed.is_dir() {
        return;
    }
    if clone_tree(&seed, &target).await {
        info!(repo, "Seeded the quest build folder from the shared dependency cache");
    } else {
        warn!(repo, "Could not seed the quest build folder, so it builds from scratch");
    }
}

fn has_builds(target: &Path) -> bool {
    std::fs::read_dir(target).is_ok_and(|entries| entries.flatten().any(|entry| entry.path().join("deps").is_dir()))
}

fn seed_age(seed: &Path) -> Option<Duration> {
    std::fs::metadata(seed.join(SEED_MARK)).ok()?.modified().ok().and_then(|at| SystemTime::now().duration_since(at).ok())
}

pub(super) async fn keep_seed(root: &Path, repo: &str, working_dir: &Path) {
    keep_seed_with(root, repo, working_dir, process_listing().await).await;
}

async fn keep_seed_with(root: &Path, repo: &str, working_dir: &Path, listing: Option<String>) {
    let Some(target) = crate::cli::quest_target_dir(working_dir) else {
        return;
    };
    let seed = seed_dir(root, repo);
    if !has_builds(&target) || seed_age(&seed).is_some_and(|age| age < SEED_FRESH_FOR) {
        return;
    }
    if listing.is_none_or(|listing| in_use(&listing, &target)) {
        return;
    }
    let _one_at_a_time = SEEDS.lock().await;
    if seed_age(&seed).is_some_and(|age| age < SEED_FRESH_FOR) {
        return;
    }
    let Some(parent) = seed.parent() else {
        return;
    };
    if tokio::fs::create_dir_all(parent).await.is_err() {
        return;
    }
    let staging = parent.join(format!(".staging-{}", Uuid::new_v4().simple()));
    if !clone_tree(&target, &staging).await {
        warn!(repo, "Could not copy the quest build folder into the shared dependency cache");
        return;
    }
    let pruned = staging.clone();
    let kept = tokio::task::spawn_blocking(move || prune_to_dependencies(&pruned).and_then(|()| std::fs::write(pruned.join(SEED_MARK), b""))).await;
    if !matches!(kept, Ok(Ok(()))) {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return;
    }
    let retired = parent.join(format!(".retired-{}", Uuid::new_v4().simple()));
    let had_old = seed.exists() && tokio::fs::rename(&seed, &retired).await.is_ok();
    if tokio::fs::rename(&staging, &seed).await.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    if had_old {
        let _ = tokio::fs::remove_dir_all(&retired).await;
    }
    info!(repo, "Updated the shared dependency cache from a quest build folder");
}

pub(super) fn seed_bytes(root: &Path) -> u64 {
    dir_size(&seeds_root(root))
}

pub(super) async fn clear_seeds(root: &Path, mut enough: impl FnMut(u64) -> bool) -> u64 {
    let _one_at_a_time = SEEDS.lock().await;
    let Ok(entries) = std::fs::read_dir(seeds_root(root)) else {
        return 0;
    };
    let mut seeds: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| (entry.metadata().and_then(|meta| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH), entry.path()))
        .collect();
    seeds.sort();
    let mut freed = 0;
    for (_, dir) in seeds {
        if enough(freed) {
            break;
        }
        let measured = dir.clone();
        let bytes = tokio::task::spawn_blocking(move || dir_size(&measured)).await.unwrap_or(0);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => freed += bytes,
            Err(error) => warn!(error = %error, dir = %dir.display(), "Failed to clear a shared dependency cache"),
        }
    }
    freed
}

fn unit_name(stem: &str) -> String {
    stem.rsplit_once('-').map_or(stem, |(name, _)| name).replace('-', "_")
}

fn is_workspace_unit(dep_info: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dep_info) else {
        return false;
    };
    let first = text.split_once(": ").and_then(|(_, deps)| deps.split_whitespace().next());
    first.is_some_and(|path| !path.starts_with('/'))
}

fn remove_path(path: &Path) {
    let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
}

fn prune_profile(profile: &Path) {
    let mut units: HashSet<String> = HashSet::new();
    let mut names: HashSet<String> = HashSet::new();
    let deps = profile.join("deps");
    let listing: Vec<PathBuf> = std::fs::read_dir(&deps).map(|entries| entries.flatten().map(|entry| entry.path()).collect()).unwrap_or_default();
    for path in &listing {
        let file = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(stem) = file.strip_suffix(".d").filter(|_| is_workspace_unit(path)) {
            units.insert(stem.to_string());
            let name = unit_name(stem);
            if name != BUILD_SCRIPT_UNIT {
                names.insert(name);
            }
        }
    }
    for path in &listing {
        let file = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let bare = file.strip_prefix("lib").unwrap_or(&file);
        let stem = |name: &str| name.split('.').next().unwrap_or(name).to_string();
        if units.contains(&stem(&file)) || units.contains(&stem(bare)) {
            remove_path(path);
        }
    }
    for part in ["build", ".fingerprint"] {
        let Ok(entries) = std::fs::read_dir(profile.join(part)) else {
            continue;
        };
        for entry in entries.flatten() {
            if names.contains(&unit_name(&entry.file_name().to_string_lossy())) {
                remove_path(&entry.path());
            }
        }
    }
}

pub(super) fn prune_to_dependencies(target: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(target)?.flatten() {
        let path = entry.path();
        let is_profile = path.is_dir() && SEED_PARTS.iter().all(|part| path.join(part).is_dir());
        if !is_profile {
            let keep = entry.file_name().to_string_lossy().starts_with(".rustc_info") || entry.file_name() == "CACHEDIR.TAG";
            if !keep {
                remove_path(&path);
            }
            continue;
        }
        for child in std::fs::read_dir(&path)?.flatten() {
            if !SEED_PARTS.iter().any(|part| child.file_name() == *part) {
                remove_path(&child.path());
            }
        }
        prune_profile(&path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn fake_target(target: &Path) {
        let debug = target.join("debug");
        touch(&debug.join("deps/libserde-aa11.rlib"), "x");
        touch(&debug.join("deps/serde-aa11.d"), "/t/debug/deps/serde-aa11.d: /home/u/.cargo/registry/src/serde/lib.rs\n");
        touch(&debug.join("deps/build_script_build-bb22.d"), "/t/debug/deps/build_script_build-bb22.d: /home/u/.cargo/registry/src/ring/build.rs\n");
        touch(&debug.join("deps/build_script_build-bb22"), "bin");
        touch(&debug.join("deps/libloka_shared-cc33.rlib"), "x");
        touch(&debug.join("deps/libloka_shared-cc33.rmeta"), "x");
        touch(&debug.join("deps/loka_shared-cc33.d"), "/t/debug/deps/loka_shared-cc33.d: shared/src/lib.rs shared/src/a.rs\n");
        touch(&debug.join("deps/loka_agent-dd44.d"), "/t/debug/deps/loka_agent-dd44.d: agent/src/main.rs\n");
        touch(&debug.join("deps/loka_agent-dd44"), "bin");
        touch(&debug.join("deps/build_script_build-ee55.d"), "/t/debug/deps/build_script_build-ee55.d: agent/build.rs\n");
        touch(&debug.join("deps/build_script_build-ee55"), "bin");
        touch(&debug.join(".fingerprint/serde-aa11/lib-serde"), "x");
        touch(&debug.join(".fingerprint/loka-shared-cc33/lib-loka_shared"), "x");
        touch(&debug.join(".fingerprint/loka-agent-dd44/bin-loka-agent"), "x");
        touch(&debug.join("build/ring-ff66/output"), "x");
        touch(&debug.join("build/loka-agent-gg77/output"), "x");
        touch(&debug.join("incremental/loka_shared-hh88/s-1/data"), "x");
        touch(&debug.join("loka-agent"), "bin");
        touch(&debug.join("examples/e"), "x");
        touch(&target.join("CACHEDIR.TAG"), "tag");
        touch(&target.join(".rustc_info.json"), "{}");
        touch(&target.join("tmp/scratch"), "x");
    }

    #[test]
    fn pruning_keeps_only_third_party_build_output() {
        let dir = std::env::temp_dir().join(format!("loka-seed-test-{}", Uuid::new_v4().simple()));
        fake_target(&dir);
        prune_to_dependencies(&dir).unwrap();
        let debug = dir.join("debug");
        for kept in [
            "deps/libserde-aa11.rlib",
            "deps/serde-aa11.d",
            "deps/build_script_build-bb22.d",
            "deps/build_script_build-bb22",
            ".fingerprint/serde-aa11/lib-serde",
            "build/ring-ff66/output",
        ] {
            assert!(debug.join(kept).exists(), "{kept} is a dependency and stays");
        }
        for gone in [
            "deps/libloka_shared-cc33.rlib",
            "deps/libloka_shared-cc33.rmeta",
            "deps/loka_shared-cc33.d",
            "deps/loka_agent-dd44.d",
            "deps/loka_agent-dd44",
            "deps/build_script_build-ee55.d",
            "deps/build_script_build-ee55",
            ".fingerprint/loka-shared-cc33",
            ".fingerprint/loka-agent-dd44",
            "build/loka-agent-gg77",
            "incremental",
            "loka-agent",
            "examples",
        ] {
            assert!(!debug.join(gone).exists(), "{gone} belongs to the workspace and goes");
        }
        assert!(dir.join("CACHEDIR.TAG").exists() && dir.join(".rustc_info.json").exists());
        assert!(!dir.join("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_not_a_full_build_profile_is_not_kept() {
        let dir = std::env::temp_dir().join(format!("loka-seed-test-{}", Uuid::new_v4().simple()));
        touch(&dir.join("debug/deps/x.rlib"), "x");
        prune_to_dependencies(&dir).unwrap();
        assert!(!dir.join("debug").exists(), "anything that is not a full profile folder is not kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn build_toy(tree: &Path) -> String {
        let target = crate::cli::quest_target_dir(tree).unwrap();
        let output = tokio::process::Command::new("cargo")
            .current_dir(tree)
            .args(["build", "--offline", "-v"])
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .output()
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{text}");
        text
    }

    #[tokio::test]
    #[ignore]
    async fn live_a_seeded_quest_reuses_third_party_builds_and_rebuilds_its_own_crate() {
        let root = std::env::temp_dir().join(format!("loka-seed-live-{}", Uuid::new_v4().simple()));
        let make = |id: Uuid| {
            let tree = root.join("workspaces").join("tasks").join(id.to_string()).join("toy");
            touch(&tree.join("Cargo.toml"), "[package]\nname = \"toy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nserde_json = \"1\"\n");
            touch(&tree.join("src/lib.rs"), "pub fn n() -> usize { serde_json::json!([1]).as_array().unwrap().len() }\n");
            tree
        };
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        let (one, two) = (make(first), make(second));
        let cold = build_toy(&one).await;
        assert!(cold.contains("Compiling serde_json"), "{cold}");
        keep_seed_with(&root, "acme/toy", &one, Some(String::new())).await;
        let seed = seed_dir(&root, "acme/toy");
        assert!(seed.join(SEED_MARK).is_file());
        let kept: Vec<String> = std::fs::read_dir(seed.join("debug/deps")).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.iter().any(|name| name.contains("serde_json")) && !kept.iter().any(|name| name.starts_with("toy-") || name.starts_with("libtoy-")), "{kept:?}");
        seed_quest(&root, "acme/toy", &two).await;
        let warm = build_toy(&two).await;
        assert!(!warm.contains("Compiling serde_json") && warm.contains("Fresh serde_json"), "{warm}");
        assert!(warm.contains("Compiling toy"), "the quest's own crate is always built from its own source: {warm}");
        for id in [first, second] {
            let _ = std::fs::remove_dir_all(crate::cli::private_temp_root().join(crate::cli::quest_temp_name(id)));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
