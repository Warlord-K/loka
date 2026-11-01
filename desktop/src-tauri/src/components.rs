use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::local::{ensure_private_dir, write_private, Stack};

const COMPAT: &str = include_str!("../../compat.json");
const API_READY: Duration = Duration::from_secs(45);
const API_MIGRATING: Duration = Duration::from_secs(240);
const WEB_READY: Duration = Duration::from_secs(60);
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Levels {
    #[serde(default)]
    pub api: u32,
    #[serde(default)]
    pub runner: u32,
    #[serde(default)]
    pub shell: u32,
}

impl Levels {
    fn met_by(&self, have: &Levels) -> bool {
        self.api <= have.api && self.runner <= have.runner && self.shell <= have.shell
    }
}

pub fn shell_levels() -> Levels {
    serde_json::from_str(COMPAT).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Api,
    Web,
}

impl Part {
    pub const ALL: [Part; 2] = [Part::Api, Part::Web];

    pub fn name(self) -> &'static str {
        match self {
            Part::Api => "api",
            Part::Web => "web",
        }
    }

    fn entry(self) -> &'static str {
        match self {
            Part::Api => "loka-api",
            Part::Web => "web/server.js",
        }
    }

    fn running_path(self, dir: &Path) -> PathBuf {
        match self {
            Part::Api => dir.join("loka-api"),
            Part::Web => dir.join("web"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Bundle {
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub web: String,
    #[serde(default)]
    pub runner: String,
    #[serde(default)]
    pub shell: String,
    #[serde(default)]
    pub schema: u32,
    #[serde(skip)]
    pub version: String,
    #[serde(skip)]
    pub levels: Levels,
}

impl Bundle {
    pub fn read(runtime: &Path, version: &str) -> Option<Bundle> {
        let bytes = std::fs::read(runtime.join("components.json")).ok()?;
        let mut bundle: Bundle = serde_json::from_slice(&bytes).ok()?;
        if [&bundle.api, &bundle.web, &bundle.runner, &bundle.shell].iter().any(|id| id.is_empty()) {
            return None;
        }
        bundle.version = version.to_string();
        bundle.levels = shell_levels();
        Some(bundle)
    }

    fn id(&self, part: Part) -> &str {
        match part {
            Part::Api => &self.api,
            Part::Web => &self.web,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub schema: Option<u32>,
    #[serde(default)]
    pub provides: Levels,
    #[serde(default)]
    pub requires: Levels,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct State {
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub api: Option<Installed>,
    #[serde(default)]
    pub web: Option<Installed>,
}

impl State {
    fn get(&self, part: Part) -> Option<&Installed> {
        match part {
            Part::Api => self.api.as_ref(),
            Part::Web => self.web.as_ref(),
        }
    }

    fn put(&mut self, part: Part, installed: Option<Installed>) {
        match part {
            Part::Api => self.api = installed,
            Part::Web => self.web = installed,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub version: String,
    pub url: String,
    pub signature: String,
    #[serde(default)]
    pub requires: Levels,
    #[serde(default)]
    pub provides: Levels,
    #[serde(default)]
    pub schema: Option<u32>,
}

impl Offer {
    fn installed(&self) -> Installed {
        Installed {
            id: self.id.clone(),
            version: self.version.clone(),
            schema: self.schema,
            provides: self.provides,
            requires: self.requires,
        }
    }

    fn file(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Offers {
    pub runner: String,
    pub shell: String,
    #[serde(default)]
    pub parts: BTreeMap<Part, Offer>,
}

pub fn offers(raw: &serde_json::Value) -> Option<Offers> {
    serde_json::from_value(raw.get("components")?.clone()).ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Active {
    pub id: String,
    pub version: String,
    pub schema: u32,
    pub api_level: u32,
    pub installed: bool,
}

fn version_parts(version: &str) -> Option<[u64; 3]> {
    let core = version.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let out = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(out)
}

pub fn newer(candidate: &str, current: &str) -> bool {
    matches!((version_parts(candidate), version_parts(current)), (Some(a), Some(b)) if a > b)
}

pub fn active(bundle: &Bundle, state: &State, part: Part) -> Active {
    match state.get(part).filter(|installed| newer(&installed.version, &bundle.version)) {
        Some(installed) => Active {
            id: installed.id.clone(),
            version: installed.version.clone(),
            schema: installed.schema.unwrap_or(bundle.schema),
            api_level: if part == Part::Api { installed.provides.api } else { 0 },
            installed: true,
        },
        None => Active {
            id: bundle.id(part).to_string(),
            version: bundle.version.clone(),
            schema: bundle.schema,
            api_level: if part == Part::Api { bundle.levels.api } else { 0 },
            installed: false,
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub parts: Vec<(Part, Offer)>,
    pub full: bool,
}

pub fn plan(bundle: Option<&Bundle>, state: &State, offers: Option<&Offers>) -> Plan {
    let (Some(bundle), Some(offers)) = (bundle, offers) else {
        return Plan { parts: Vec::new(), full: true };
    };
    let mut full = offers.runner != bundle.runner || offers.shell != bundle.shell;
    let mut have = Levels { api: active(bundle, state, Part::Api).api_level, runner: bundle.levels.runner, shell: bundle.levels.shell };
    let mut parts = Vec::new();
    for part in Part::ALL {
        let Some(offer) = offers.parts.get(&part) else { continue };
        let current = active(bundle, state, part);
        if offer.id == current.id || !newer(&offer.version, &current.version) {
            continue;
        }
        if !offer.requires.met_by(&have) {
            full = true;
            continue;
        }
        if part == Part::Api {
            have.api = offer.provides.api;
        }
        parts.push((part, offer.clone()));
    }
    Plan { parts, full }
}

pub fn migrates(bundle: &Bundle, state: &State, offer: &Offer) -> bool {
    offer.schema.unwrap_or(0) > active(bundle, state, Part::Api).schema
}

pub fn verify(bytes: &[u8], signature: &str, public_key: &str, file: &str, version: &str) -> Result<(), String> {
    let decode = |text: &str| -> Result<String, String> {
        STANDARD
            .decode(text.trim())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| "The update signature is not readable.".to_string())
    };
    let key = minisign_verify::PublicKey::decode(&decode(public_key)?).map_err(|_| "The update key is not readable.".to_string())?;
    let signature = minisign_verify::Signature::decode(&decode(signature)?).map_err(|_| "The update signature is not readable.".to_string())?;
    key.verify(bytes, &signature, true).map_err(|_| "The update signature does not match. Loka did not install it.".to_string())?;
    let field = |name: &str| signature.trusted_comment().split('\t').find_map(|pair| pair.strip_prefix(name)).map(str::to_string);
    if field("file:").as_deref() != Some(file) || field("version:").as_deref() != Some(version) {
        return Err("The update was signed for a different file. Loka did not install it.".into());
    }
    Ok(())
}

fn expected_file(part: Part, offer: &Offer) -> bool {
    let file = offer.file();
    let prefix = format!("loka-{}-{}-", part.name(), offer.version);
    file.starts_with(&prefix)
        && file.ends_with(".tar.gz")
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[derive(Deserialize)]
struct Label {
    name: String,
    id: String,
    version: String,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn load(&self) -> State {
        std::fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, state: &State) -> Result<(), String> {
        ensure_private_dir(&self.root).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
        let temporary = self.root.join("state.json.tmp");
        write_private(&temporary, &bytes).map_err(|error| error.to_string())?;
        std::fs::rename(temporary, self.state_path()).map_err(|error| error.to_string())
    }

    pub fn dir(&self, part: Part, installed: &Installed) -> PathBuf {
        self.root.join(part.name()).join(format!("{}-{}", installed.version, installed.id))
    }

    pub fn running_path(&self, part: Part, installed: &Installed) -> PathBuf {
        part.running_path(&self.dir(part, installed))
    }

    fn ready(&self, part: Part, installed: &Installed) -> bool {
        let dir = self.dir(part, installed);
        dir.join(part.entry()).is_file() && label_matches(&dir, part, installed)
    }

    pub fn stage(&self, part: Part, offer: &Offer, bytes: &[u8]) -> Result<Installed, String> {
        let installed = offer.installed();
        if self.ready(part, &installed) {
            return Ok(installed);
        }
        let target = self.dir(part, &installed);
        let parent = target.parent().ok_or("No folder for the update.")?.to_path_buf();
        ensure_private_dir(&parent).map_err(|error| error.to_string())?;
        let staging = parent.join(format!("{}-{}.staging", installed.version, installed.id));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
        let archive = parent.join(format!("{}-{}.tar.gz", installed.version, installed.id));
        write_private(&archive, bytes).map_err(|error| error.to_string())?;
        let unpacked = Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let _ = std::fs::remove_file(&archive);
        let unpacked = unpacked.map_err(|error| format!("Could not unpack the update: {error}"))?;
        if !unpacked.status.success() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("Could not unpack the update: {}", String::from_utf8_lossy(&unpacked.stderr).trim()));
        }
        let entry = staging.join(part.entry());
        if !entry.is_file() || !label_matches(&staging, part, &installed) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err("The update is missing files. Loka did not install it.".into());
        }
        #[cfg(unix)]
        if part == Part::Api {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_dir_all(&target);
        std::fs::rename(&staging, &target).map_err(|error| format!("Could not move the update into place: {error}"))?;
        Ok(installed)
    }

    pub fn prune(&self, part: Part, keep: &[&Installed]) {
        let keep: Vec<PathBuf> = keep.iter().map(|installed| self.dir(part, installed)).collect();
        let Ok(entries) = std::fs::read_dir(self.root.join(part.name())) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if !keep.contains(&path) {
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
    }
}

fn label_matches(dir: &Path, part: Part, installed: &Installed) -> bool {
    std::fs::read(dir.join("component.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Label>(&bytes).ok())
        .is_some_and(|label| label.name == part.name() && label.id == installed.id && label.version == installed.version)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PartsView {
    pub state: &'static str,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub names: Vec<&'static str>,
    pub step: Option<String>,
    pub error: Option<String>,
    pub reload: bool,
}

#[derive(Clone)]
struct Pending {
    parts: Vec<(Part, Offer)>,
    version: String,
    notes: Option<String>,
}

pub struct Components {
    store: Store,
    bundle: Option<Bundle>,
    state: Mutex<State>,
    public_key: Option<String>,
    pending: Mutex<Option<Pending>>,
    view: Mutex<Option<PartsView>>,
    applying: AtomicBool,
}

impl Components {
    pub fn new(store: Store, bundle: Option<Bundle>, public_key: Option<String>) -> Self {
        let state = store.load();
        Self {
            store,
            bundle,
            state: Mutex::new(state),
            public_key,
            pending: Mutex::new(None),
            view: Mutex::new(None),
            applying: AtomicBool::new(false),
        }
    }

    pub fn enabled(&self) -> bool {
        self.bundle.is_some() && self.public_key.is_some()
    }

    pub fn auto(&self) -> bool {
        self.state.lock().map(|state| state.auto).unwrap_or(false)
    }

    pub fn applying(&self) -> bool {
        self.applying.load(Ordering::Acquire)
    }

    pub fn view(&self) -> Option<PartsView> {
        self.view.lock().ok().and_then(|view| view.clone())
    }

    fn set_view(&self, view: Option<PartsView>) {
        if let Ok(mut current) = self.view.lock() {
            *current = view;
        }
    }

    fn state(&self) -> State {
        self.state.lock().map(|state| state.clone()).unwrap_or_default()
    }

    fn commit(&self, state: State) -> Result<(), String> {
        self.store.save(&state)?;
        if let Ok(mut current) = self.state.lock() {
            *current = state;
        }
        Ok(())
    }

    pub fn set_auto(&self, auto: bool) -> Result<(), String> {
        let mut state = self.state();
        state.auto = auto;
        self.commit(state)
    }

    pub fn choose(&self, stack: &Stack) {
        let Some(bundle) = &self.bundle else { return };
        let mut state = self.state();
        let have = Levels { api: 0, ..bundle.levels };
        for part in Part::ALL {
            let usable = state
                .get(part)
                .filter(|installed| newer(&installed.version, &bundle.version))
                .filter(|installed| Levels { api: 0, ..installed.requires }.met_by(&have))
                .filter(|installed| self.store.ready(part, installed))
                .cloned();
            match &usable {
                Some(installed) => {
                    let path = self.store.running_path(part, installed);
                    match part {
                        Part::Api => stack.set_api_binary(path),
                        Part::Web => stack.set_web_dir(path),
                    }
                    self.store.prune(part, &[installed]);
                }
                None => self.store.prune(part, &[]),
            }
            state.put(part, usable);
        }
        let _ = self.commit(state);
    }

    pub fn plan(&self, raw: &serde_json::Value) -> Plan {
        if !self.enabled() {
            return Plan { parts: Vec::new(), full: true };
        }
        plan(self.bundle.as_ref(), &self.state(), offers(raw).as_ref())
    }

    pub fn offer(&self, plan: &Plan, version: String, notes: Option<String>) -> bool {
        if self.applying() {
            return false;
        }
        if plan.parts.is_empty() {
            if let Ok(mut pending) = self.pending.lock() {
                *pending = None;
            }
            if self.view().is_some_and(|view| matches!(view.state, "available" | "failed")) {
                self.set_view(None);
            }
            return false;
        }
        let names = plan.parts.iter().map(|(part, _)| part.name()).collect();
        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(Pending { parts: plan.parts.clone(), version: version.clone(), notes: notes.clone() });
        }
        self.set_view(Some(PartsView { state: "available", version: Some(version), notes, names, ..PartsView::default() }));
        true
    }

    pub fn has_pending(&self) -> bool {
        self.pending.lock().map(|pending| pending.is_some()).unwrap_or(false)
    }
}

pub fn start(app: &AppHandle, root: &Path, runtime: &Path) -> Arc<Components> {
    let version = app.package_info().version.to_string();
    let public_key = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("pubkey"))
        .and_then(|key| key.as_str())
        .map(str::to_string);
    Arc::new(Components::new(Store::new(root.join("components")), Bundle::read(runtime, &version), public_key))
}

async fn download(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_LIMIT)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client.get(url).send().await.map_err(|error| format!("Could not download the update: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Could not download the update ({}).", response.status()));
    }
    response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| format!("Could not download the update: {error}"))
}

fn label(part: Part) -> &'static str {
    match part {
        Part::Api => "server",
        Part::Web => "app",
    }
}

pub async fn apply(app: &AppHandle) -> Result<(), String> {
    let components = app.state::<Arc<Components>>().inner().clone();
    let stack = app.state::<Arc<Stack>>().inner().clone();
    let pending = components.pending.lock().map_err(|_| "Loka could not read the update.".to_string())?.clone();
    let Some(pending) = pending else {
        return Err("There is nothing to update right now.".into());
    };
    if !matches!(stack.phase(), crate::local::Phase::Ready) {
        return Err("Loka is still starting. Try again in a moment.".into());
    }
    if components.applying.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    if !stack.begin() {
        components.applying.store(false, Ordering::Release);
        return Err("Loka is busy starting up. Try again in a moment.".into());
    }
    let names: Vec<&'static str> = pending.parts.iter().map(|(part, _)| part.name()).collect();
    let base = PartsView { version: Some(pending.version.clone()), notes: pending.notes.clone(), names, ..PartsView::default() };
    let mut reload = false;
    let mut outcome = Ok(());
    for (part, offer) in &pending.parts {
        let step = |text: String| {
            components.set_view(Some(PartsView { state: "applying", step: Some(text), ..base.clone() }));
            crate::updates::announce(app);
        };
        step(format!("Downloading the new {}", label(*part)));
        match apply_part(&components, &stack, *part, offer, &step).await {
            Ok(()) => reload |= *part == Part::Web,
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    stack.end();
    if let Ok(mut current) = components.pending.lock() {
        *current = None;
    }
    let view = match &outcome {
        Ok(()) => PartsView { state: "applied", reload, ..base },
        Err(error) => PartsView { state: "failed", error: Some(error.clone()), reload, ..base },
    };
    components.set_view(Some(view));
    components.applying.store(false, Ordering::Release);
    crate::updates::announce(app);
    outcome
}

async fn apply_part(components: &Components, stack: &Stack, part: Part, offer: &Offer, step: &impl Fn(String)) -> Result<(), String> {
    let bundle = components.bundle.clone().ok_or("This build of Loka takes full updates only.")?;
    let public_key = components.public_key.clone().ok_or("This build of Loka has no update key.")?;
    if !expected_file(part, offer) {
        return Err("The update points at an unexpected file. Loka did not install it.".into());
    }
    let bytes = download(&offer.url).await?;
    step(format!("Checking the new {}", label(part)));
    let checking = offer.clone();
    let root = components.store.root.clone();
    let installed = tauri::async_runtime::spawn_blocking(move || {
        verify(&bytes, &checking.signature, &public_key, checking.file(), &checking.version)?;
        Store::new(root).stage(part, &checking, &bytes)
    })
    .await
    .map_err(|error| error.to_string())??;
    let state = components.state();
    let previous = state.get(part).cloned().filter(|installed| newer(&installed.version, &bundle.version));
    let keeps_data = part == Part::Api && migrates(&bundle, &state, offer);
    let before = match part {
        Part::Api => stack.api_binary(),
        Part::Web => stack.web_dir(),
    };
    let after = components.store.running_path(part, &installed);
    let mut next = state.clone();
    next.put(part, Some(installed.clone()));
    if keeps_data {
        components.commit(next.clone())?;
    }
    step(format!("Restarting the {}", label(part)));
    let started = match part {
        Part::Api => {
            stack.set_api_binary(after);
            stack.restart_api(if keeps_data { API_MIGRATING } else { API_READY }).await
        }
        Part::Web => {
            stack.set_web_dir(after);
            stack.restart_web(WEB_READY).await
        }
    };
    let error = match started {
        Ok(()) => {
            components.commit(next)?;
            let keep: Vec<&Installed> = std::iter::once(&installed).chain(previous.as_ref()).collect();
            components.store.prune(part, &keep);
            return Ok(());
        }
        Err(error) => error,
    };
    if keeps_data {
        return Err(format!(
            "The new {} did not start: {error}. It may already have updated your data, so Loka keeps it and keeps retrying. Check the {} log.",
            label(part),
            part.name()
        ));
    }
    step(format!("Going back to the previous {}", label(part)));
    let back = match part {
        Part::Api => {
            stack.set_api_binary(before);
            stack.restart_api(API_READY).await
        }
        Part::Web => {
            stack.set_web_dir(before);
            stack.restart_web(WEB_READY).await
        }
    };
    let _ = std::fs::remove_dir_all(components.store.dir(part, &installed));
    match back {
        Ok(()) => Err(format!("The new {} did not start, so Loka went back to the one you had: {error}", label(part))),
        Err(again) => Err(format!("The new {} did not start ({error}) and the previous one did not come back either ({again}). Choose Reload from the View menu.", label(part))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> Bundle {
        Bundle {
            api: "api-1".into(),
            web: "web-1".into(),
            runner: "runner-1".into(),
            shell: "shell-1".into(),
            schema: 130,
            version: "0.1.10".into(),
            levels: Levels { api: 1, runner: 1, shell: 1 },
        }
    }

    fn offer(id: &str, version: &str) -> Offer {
        Offer {
            id: id.into(),
            version: version.into(),
            url: format!("https://example.test/updates/free/files/{version}/loka-x-{version}-aarch64.tar.gz"),
            signature: String::new(),
            requires: Levels { api: 0, runner: 1, shell: 1 },
            provides: Levels { api: 1, ..Levels::default() },
            schema: Some(130),
        }
    }

    fn offers(api: Option<Offer>, web: Option<Offer>) -> Offers {
        let mut parts = BTreeMap::new();
        if let Some(api) = api {
            parts.insert(Part::Api, api);
        }
        if let Some(web) = web {
            parts.insert(Part::Web, Offer { requires: Levels { api: 1, runner: 0, shell: 1 }, provides: Levels::default(), schema: None, ..web });
        }
        Offers { runner: "runner-1".into(), shell: "shell-1".into(), parts }
    }

    #[test]
    fn only_changed_parts_update_and_the_bundle_stays() {
        let plan = plan(Some(&bundle()), &State::default(), Some(&offers(Some(offer("api-1", "0.1.11")), Some(offer("web-2", "0.1.11")))));
        assert!(!plan.full);
        assert_eq!(plan.parts.iter().map(|(part, _)| *part).collect::<Vec<_>>(), vec![Part::Web]);
    }

    #[test]
    fn a_new_runner_or_shell_needs_the_full_update() {
        let mut changed = offers(Some(offer("api-2", "0.1.11")), None);
        changed.runner = "runner-2".into();
        let plan = plan(Some(&bundle()), &State::default(), Some(&changed));
        assert!(plan.full);
        assert_eq!(plan.parts.len(), 1);
        assert!(super::plan(None, &State::default(), Some(&changed)).full);
        assert!(super::plan(Some(&bundle()), &State::default(), None).full);
    }

    #[test]
    fn parts_that_need_a_newer_bundle_wait_for_the_full_update() {
        let mut api = offer("api-2", "0.1.11");
        api.requires.runner = 2;
        let mut web = offer("web-2", "0.1.11");
        web.requires.api = 2;
        let mut set = offers(Some(api), None);
        set.parts.insert(Part::Web, Offer { requires: Levels { api: 2, runner: 0, shell: 1 }, ..web });
        let plan = plan(Some(&bundle()), &State::default(), Some(&set));
        assert!(plan.full);
        assert!(plan.parts.is_empty());
    }

    #[test]
    fn a_web_part_may_lean_on_the_api_part_in_the_same_update() {
        let mut api = offer("api-2", "0.1.11");
        api.provides.api = 2;
        let mut set = offers(Some(api), None);
        set.parts.insert(Part::Web, Offer { requires: Levels { api: 2, runner: 0, shell: 1 }, ..offer("web-2", "0.1.11") });
        let plan = plan(Some(&bundle()), &State::default(), Some(&set));
        assert!(!plan.full);
        assert_eq!(plan.parts.len(), 2);
        assert_eq!(plan.parts[0].0, Part::Api);
    }

    #[test]
    fn installed_parts_count_until_a_newer_bundle_arrives() {
        let state = State { api: Some(offer("api-2", "0.1.11").installed()), ..State::default() };
        let same = plan(Some(&bundle()), &state, Some(&offers(Some(offer("api-2", "0.1.12")), None)));
        assert!(same.parts.is_empty());
        let newer_bundle = Bundle { version: "0.1.12".into(), api: "api-3".into(), ..bundle() };
        assert!(!active(&newer_bundle, &state, Part::Api).installed);
        assert!(active(&bundle(), &state, Part::Api).installed);
    }

    #[test]
    fn migrations_are_noticed() {
        let mut api = offer("api-2", "0.1.11");
        assert!(!migrates(&bundle(), &State::default(), &api));
        api.schema = Some(131);
        assert!(migrates(&bundle(), &State::default(), &api));
    }

    #[test]
    fn manifest_components_parse() {
        let raw = serde_json::json!({
            "version": "0.1.11",
            "components": {
                "runner": "r", "shell": "s",
                "parts": { "api": { "id": "a", "version": "0.1.11", "url": "u/loka-api-0.1.11-aarch64.tar.gz", "signature": "x", "requires": { "runner": 1 }, "provides": { "api": 1 }, "schema": 133 } }
            }
        });
        let parsed = super::offers(&raw).unwrap();
        assert_eq!(parsed.parts[&Part::Api].schema, Some(133));
        assert!(expected_file(Part::Api, &parsed.parts[&Part::Api]));
        assert!(!expected_file(Part::Web, &parsed.parts[&Part::Api]));
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loka-components-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sign(dir: &Path, file: &Path, version: &str) -> Option<(String, String)> {
        let key = dir.join("test.key");
        let generated = Command::new("npx")
            .args(["--no-install", "tauri", "signer", "generate", "--ci", "-p", "", "-f", "-w"])
            .arg(&key)
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
            .output()
            .ok()?;
        if !generated.status.success() {
            return None;
        }
        let signed = Command::new("npx")
            .args(["--no-install", "tauri", "signer", "sign", "-p", "", "--app-version", version, "-f"])
            .arg(&key)
            .arg(file)
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
            .output()
            .ok()?;
        if !signed.status.success() {
            return None;
        }
        let public = std::fs::read_to_string(dir.join("test.key.pub")).ok()?;
        let signature = std::fs::read_to_string(file.with_file_name(format!("{}.sig", file.file_name()?.to_string_lossy()))).ok()?;
        Some((public, signature))
    }

    #[test]
    fn signed_archives_unpack_and_anything_else_is_refused() {
        let dir = scratch();
        let source = dir.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("loka-api"), "#!/bin/sh\n").unwrap();
        std::fs::write(source.join("component.json"), r#"{"name":"api","id":"api-2","version":"0.1.11"}"#).unwrap();
        let archive = dir.join("loka-api-0.1.11-aarch64.tar.gz");
        assert!(Command::new("/usr/bin/tar").arg("-czf").arg(&archive).arg("-C").arg(&source).arg(".").status().unwrap().success());
        let Some((public, signature)) = sign(&dir, &archive, "0.1.11") else {
            eprintln!("tauri signer is not installed, skipping");
            return;
        };
        let bytes = std::fs::read(&archive).unwrap();
        assert!(verify(&bytes, &signature, &public, "loka-api-0.1.11-aarch64.tar.gz", "0.1.11").is_ok());
        assert!(verify(&bytes, &signature, &public, "loka-web-0.1.11-aarch64.tar.gz", "0.1.11").is_err());
        assert!(verify(&bytes, &signature, &public, "loka-api-0.1.11-aarch64.tar.gz", "0.1.12").is_err());
        let mut tampered = bytes.clone();
        tampered.push(0);
        assert!(verify(&tampered, &signature, &public, "loka-api-0.1.11-aarch64.tar.gz", "0.1.11").is_err());

        let store = Store::new(dir.join("components"));
        let mut good = offer("api-2", "0.1.11");
        good.url = "https://example.test/loka-api-0.1.11-aarch64.tar.gz".into();
        let installed = store.stage(Part::Api, &good, &bytes).unwrap();
        assert!(store.running_path(Part::Api, &installed).is_file());
        let wrong = Offer { id: "api-3".into(), ..good.clone() };
        assert!(store.stage(Part::Api, &wrong, &bytes).is_err());
        store.prune(Part::Api, &[]);
        assert!(!store.dir(Part::Api, &installed).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
