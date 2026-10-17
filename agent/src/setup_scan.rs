use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use loka_shared::setup_scan::{clean_repo, redact_server, scrub, source_name, ScanRequest};
use serde_json::Value;

const FILES_PER_SOURCE: usize = 80;
const READ_BYTES: u64 = 2 * 1024 * 1024;
const CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const PROMPTS_PER_SOURCE: usize = 120;
const PROMPT_CHARS: usize = 280;
const INSTRUCTION_CHARS: usize = 6000;
const INSTRUCTION_REPOS: usize = 6;
const MAX_DIGEST: usize = 140 * 1024;
const MAX_DEPTH: usize = 6;
const MAX_ENTRIES: usize = 20_000;

#[derive(Default)]
struct Activity {
    sessions: usize,
    prompts: Vec<String>,
    counts: BTreeMap<String, usize>,
    tools: BTreeMap<String, usize>,
    commands: BTreeMap<String, usize>,
    folders: BTreeMap<String, usize>,
}

impl Activity {
    fn prompt(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() || skipped_prompt(text) {
            return;
        }
        let line: String = scrub(&text.split_whitespace().collect::<Vec<_>>().join(" ")).chars().take(PROMPT_CHARS).collect();
        let key: String = line.to_lowercase().chars().take(80).collect();
        let seen = self.counts.entry(key).or_insert(0);
        *seen += 1;
        if *seen == 1 && self.prompts.len() < PROMPTS_PER_SOURCE {
            self.prompts.push(line);
        }
    }

    fn command(&mut self, command: &str) {
        let words: Vec<&str> = command.split_whitespace().filter(|w| !w.contains('=') || w.starts_with('-')).collect();
        let Some(first) = words.first() else { return };
        let program = first.rsplit('/').next().unwrap_or(first);
        let mut head = vec![program.to_string()];
        for word in words.iter().skip(1).take(2) {
            if word.starts_with('-') || word.contains('/') || word.contains('"') || word.contains('\'') || word.len() > 24 {
                break;
            }
            head.push(word.to_string());
            if !matches!(*word, "run" | "exec" | "x") {
                break;
            }
        }
        let head = scrub(&head.join(" "));
        if head.chars().count() <= 60 {
            *self.commands.entry(head).or_insert(0) += 1;
        }
    }
}

fn skipped_prompt(text: &str) -> bool {
    text.starts_with('<')
        || text.starts_with("Caveat:")
        || text.starts_with("# AGENTS.md")
        || text.starts_with("[aside]")
        || text.starts_with("[setup scan]")
        || text.starts_with("[instructions updated]")
        || text.starts_with("This session is being continued")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn env_dir(name: &str, fallback: &str) -> PathBuf {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(fallback))
}

fn tidy(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if home != Path::new("/") => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(cap).read_to_end(&mut bytes).ok()?;
    if meta.len() > cap {
        if let Some(end) = bytes.iter().rposition(|b| *b == b'\n') {
            bytes.truncate(end);
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(path).ok().and_then(|m| m.modified().ok())
}

fn recent_files(dir: &Path, ext: &str, since: SystemTime, skip: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((current, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            if skip.iter().any(|s| path.starts_with(s)) {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() && path.extension().and_then(|e| e.to_str()) == Some(ext) {
                if let Some(time) = modified(&path).filter(|t| *t >= since) {
                    found.push((time, path));
                }
            }
        }
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().take(FILES_PER_SOURCE).map(|(_, p)| p).collect()
}

fn texts(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("text" | "input_text")))
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn command_of(input: Option<&Value>) -> Option<String> {
    let input = match input? {
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok()?,
        other => other.clone(),
    };
    match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(c) => Some(c.clone()),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [shell, flag, script] if shell.ends_with("sh") && flag.starts_with('-') => Some(script.to_string()),
                _ => Some(parts.join(" ")),
            }
        }
        _ => None,
    }
}

fn read_transcript(text: &str, act: &mut Activity, skip: &[PathBuf]) -> bool {
    let mut folder: Option<String> = None;
    let mut prompts = Vec::new();
    let mut tools = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if folder.is_none() {
            folder = v.get("cwd").or_else(|| v.get("payload").and_then(|p| p.get("cwd"))).and_then(Value::as_str).map(str::to_string);
        }
        if v.get("isMeta").and_then(Value::as_bool) == Some(true) || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let message = match kind {
            "user" | "assistant" | "message" => v.get("message"),
            "response_item" => v.get("payload"),
            _ => None,
        };
        let Some(message) = message else { continue };
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "user" && message.get("type").and_then(Value::as_str).is_none_or(|t| t == "message") {
            prompts.extend(texts(message.get("content")));
        }
        if message.get("type").and_then(Value::as_str) == Some("function_call") {
            let name = message.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
            tools.push((name, command_of(message.get("arguments"))));
        }
        if role == "assistant" {
            for block in message.get("content").and_then(Value::as_array).into_iter().flatten() {
                if matches!(block.get("type").and_then(Value::as_str), Some("tool_use" | "toolCall")) {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                    tools.push((name, command_of(block.get("input").or_else(|| block.get("arguments")))));
                }
            }
        }
    }
    if let Some(dir) = folder.as_deref() {
        if skip.iter().any(|s| Path::new(dir).starts_with(s)) {
            return false;
        }
    }
    if prompts.is_empty() && tools.is_empty() {
        return false;
    }
    act.sessions += 1;
    if let Some(dir) = folder {
        *act.folders.entry(dir).or_insert(0) += 1;
    }
    for p in prompts {
        act.prompt(&p);
    }
    for (name, command) in tools {
        *act.tools.entry(name.chars().take(40).collect()).or_insert(0) += 1;
        if let Some(command) = command {
            act.command(&command);
        }
    }
    true
}

fn transcripts(dir: &Path, since: SystemTime, skip: &[PathBuf]) -> Activity {
    let mut act = Activity::default();
    for file in recent_files(dir, "jsonl", since, skip) {
        if let Some(text) = read_capped(&file, READ_BYTES) {
            read_transcript(&text, &mut act, skip);
        }
    }
    act
}

fn cursor_activity(since: SystemTime) -> Activity {
    let base = env_dir("CURSOR_CONFIG_DIR", ".cursor");
    let mut act = Activity::default();
    if let Ok(groups) = std::fs::read_dir(base.join("chats")) {
        for group in groups.flatten().take(500) {
            if let Ok(chats) = std::fs::read_dir(group.path()) {
                act.sessions += chats.flatten().filter(|c| modified(&c.path().join("store.db")).is_some_and(|t| t >= since)).count();
            }
        }
    }
    if let Ok(projects) = std::fs::read_dir(base.join("projects")) {
        for project in projects.flatten().take(500) {
            if modified(&project.path()).is_some_and(|t| t >= since) {
                *act.folders.entry(project.file_name().to_string_lossy().into_owned()).or_insert(0) += 1;
            }
        }
    }
    act
}

fn top(map: &BTreeMap<String, usize>, n: usize) -> String {
    let mut items: Vec<(&String, &usize)> = map.iter().collect();
    items.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    items.into_iter().take(n).map(|(k, v)| format!("{k} ({v})")).collect::<Vec<_>>().join(", ")
}

fn render_activity(out: &mut String, source: &str, act: &Activity, days: u32) {
    out.push_str(&format!("\n== {}: {} sessions in the last {days} days ==\n", source_name(source), act.sessions));
    if !act.folders.is_empty() {
        let folders: BTreeMap<String, usize> = act.folders.iter().map(|(k, v)| (tidy(Path::new(k)), *v)).collect();
        out.push_str(&format!("Folders: {}\n", top(&folders, 15)));
    }
    if !act.tools.is_empty() {
        out.push_str(&format!("Tools used: {}\n", top(&act.tools, 20)));
    }
    if !act.commands.is_empty() {
        out.push_str(&format!("Commands run: {}\n", top(&act.commands, 30)));
    }
    if !act.prompts.is_empty() {
        out.push_str("Requests, newest first, repeats counted:\n");
        for p in &act.prompts {
            let key: String = p.to_lowercase().chars().take(80).collect();
            match act.counts.get(&key).copied().unwrap_or(1) {
                1 => out.push_str(&format!("- {p}\n")),
                n => out.push_str(&format!("- (x{n}) {p}\n")),
            }
        }
    }
}

fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir);
    while let Some(path) = current {
        if std::fs::symlink_metadata(path.join(".git")).is_ok() {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

fn git_config(root: &Path) -> Option<String> {
    let dot = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot).ok()?;
    let dir = if meta.is_dir() {
        dot
    } else {
        let pointer = read_capped(&dot, 4096)?;
        let gitdir = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
        let gitdir = if gitdir.is_absolute() { gitdir } else { root.join(gitdir) };
        match read_capped(&gitdir.join("commondir"), 4096) {
            Some(common) => {
                let common = PathBuf::from(common.trim());
                if common.is_absolute() { common } else { gitdir.join(common) }
            }
            None => gitdir,
        }
    };
    read_capped(&dir.join("config"), 64 * 1024)
}

fn origin(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url").map(str::trim).and_then(|r| r.strip_prefix('=')) {
                return clean_repo(url.trim());
            }
        }
    }
    None
}

fn toml_string(value: &str) -> Option<String> {
    let v = value.trim();
    if let Some(inner) = v.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')).map(str::to_string)
}

fn toml_array(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in inner.chars() {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if c == q => {
                items.push(std::mem::take(&mut current));
                quote = None;
            }
            (Some(_), c) => current.push(c),
            _ => {}
        }
    }
    Some(items)
}

fn toml_inline_table(value: &str) -> Option<serde_json::Map<String, Value>> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut map = serde_json::Map::new();
    for pair in inner.split(',') {
        let (k, v) = pair.split_once('=')?;
        map.insert(k.trim().trim_matches('"').to_string(), Value::String(toml_string(v).unwrap_or_default()));
    }
    Some(map)
}

pub(crate) fn codex_servers(text: &str) -> BTreeMap<String, Value> {
    let mut servers: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    let mut section: Option<(String, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = header.strip_prefix("mcp_servers.").and_then(|rest| {
                let (name, sub) = match rest.strip_prefix('"') {
                    Some(quoted) => {
                        let end = quoted.find('"')?;
                        (quoted[..end].to_string(), quoted[end + 1..].strip_prefix('.').map(str::to_string))
                    }
                    None => match rest.split_once('.') {
                        Some((n, s)) => (n.to_string(), Some(s.to_string())),
                        None => (rest.to_string(), None),
                    },
                };
                Some((name, sub))
            });
            continue;
        }
        let Some((name, sub)) = section.as_ref() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim().trim_matches('"').to_string();
        let entry = servers.entry(name.clone()).or_default();
        match sub.as_deref() {
            Some(table @ ("env" | "http_headers" | "headers")) => {
                let field = if table == "env" { "env" } else { "headers" };
                let target = entry.entry(field).or_insert_with(|| Value::Object(Default::default()));
                if let (Some(map), Some(v)) = (target.as_object_mut(), toml_string(value)) {
                    map.insert(key, Value::String(v));
                }
            }
            Some(_) => {}
            None => match key.as_str() {
                "command" | "url" => {
                    if let Some(v) = toml_string(value) {
                        entry.insert(key, Value::String(v));
                    }
                }
                "args" => {
                    if let Some(items) = toml_array(value) {
                        entry.insert(key, Value::Array(items.into_iter().map(Value::String).collect()));
                    }
                }
                "env" | "http_headers" => {
                    if let Some(map) = toml_inline_table(value) {
                        entry.insert(if key == "env" { "env".into() } else { "headers".into() }, Value::Object(map));
                    }
                }
                _ => {}
            },
        }
    }
    servers.into_iter().map(|(k, v)| (k, Value::Object(v))).collect()
}

fn json_servers(value: &Value) -> BTreeMap<String, Value> {
    value
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_capped(path, CONFIG_BYTES)?).ok()
}

fn mcp_servers(sources: &[String], repos: &[PathBuf]) -> Vec<(String, String, Value)> {
    let mut found: Vec<(String, String, Value)> = Vec::new();
    let mut add = |source: &str, servers: BTreeMap<String, Value>| {
        for (name, def) in servers {
            if found.len() >= 40 || name == "world" || found.iter().any(|(_, n, _)| *n == name) {
                continue;
            }
            if let Some(clean) = redact_server(&name, &def) {
                found.push((source.to_string(), name, clean));
            }
        }
    };
    for source in sources {
        match source.as_str() {
            "claude" => {
                let claude_dir = env_dir("CLAUDE_CONFIG_DIR", ".claude");
                for path in [home().join(".claude.json"), claude_dir.join(".claude.json")] {
                    if let Some(config) = read_json(&path) {
                        add("Claude Code", json_servers(&config));
                        for project in config.get("projects").and_then(Value::as_object).into_iter().flat_map(|p| p.values()) {
                            add("Claude Code", json_servers(project));
                        }
                    }
                }
                for repo in repos {
                    if let Some(config) = read_json(&repo.join(".mcp.json")) {
                        add("Claude Code", json_servers(&config));
                    }
                }
            }
            "codex" => {
                if let Some(text) = read_capped(&env_dir("CODEX_HOME", ".codex").join("config.toml"), CONFIG_BYTES) {
                    add("Codex", codex_servers(&text));
                }
            }
            "cursor" => {
                if let Some(config) = read_json(&env_dir("CURSOR_CONFIG_DIR", ".cursor").join("mcp.json")) {
                    add("Cursor", json_servers(&config));
                }
                for repo in repos {
                    if let Some(config) = read_json(&repo.join(".cursor/mcp.json")) {
                        add("Cursor", json_servers(&config));
                    }
                }
            }
            "omp" => {
                if let Some(config) = read_json(&crate::omp::config_source().join("mcp.json")) {
                    add("omp", json_servers(&config));
                }
            }
            _ => {}
        }
    }
    found
}

fn instruction(out: &mut String, path: &Path) {
    let Some(text) = read_capped(path, 256 * 1024) else { return };
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let clipped: String = scrub(text).chars().take(INSTRUCTION_CHARS).collect();
    out.push_str(&format!("\n--- {} ---\n{clipped}\n", tidy(path)));
}

pub fn digest(request: &ScanRequest, runner_root: &Path) -> String {
    let since = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(request.days) * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut skip = vec![runner_root.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(runner_root) {
        skip.push(canonical);
    }
    let mut out = format!(
        "[local digest] Built by the Loka app on this Mac from {} over the last {} days. Secret-looking values are replaced with [removed].\n",
        request.names(),
        request.days
    );
    let mut folders: BTreeMap<String, usize> = BTreeMap::new();
    for source in &request.sources {
        let act = match source.as_str() {
            "claude" => transcripts(&env_dir("CLAUDE_CONFIG_DIR", ".claude").join("projects"), since, &skip),
            "codex" => transcripts(&env_dir("CODEX_HOME", ".codex").join("sessions"), since, &skip),
            "omp" => transcripts(&crate::omp::config_source().join("sessions"), since, &skip),
            "cursor" => cursor_activity(since),
            _ => continue,
        };
        if source != "cursor" {
            for (folder, n) in &act.folders {
                *folders.entry(folder.clone()).or_insert(0) += n;
            }
        }
        render_activity(&mut out, source, &act, request.days);
    }
    let mut repos: BTreeMap<PathBuf, (Option<String>, usize)> = BTreeMap::new();
    for (folder, n) in &folders {
        if let Some(root) = git_root(Path::new(folder)) {
            if skip.iter().any(|s| root.starts_with(s)) {
                continue;
            }
            let entry = repos.entry(root.clone()).or_insert_with(|| (git_config(&root).and_then(|c| origin(&c)), 0));
            entry.1 += n;
        }
    }
    let mut ranked: Vec<(PathBuf, Option<String>, usize)> = repos.into_iter().map(|(p, (o, n))| (p, o, n)).collect();
    ranked.sort_by_key(|r| std::cmp::Reverse(r.2));
    if !ranked.is_empty() {
        out.push_str("\n== Repos ==\n");
        for (path, remote, n) in ranked.iter().take(20) {
            match remote {
                Some(remote) => out.push_str(&format!("- {} is {remote} ({n} sessions)\n", tidy(path))),
                None => out.push_str(&format!("- {} has no GitHub remote ({n} sessions)\n", tidy(path))),
            }
        }
    }
    let top_repos: Vec<PathBuf> = ranked.iter().take(INSTRUCTION_REPOS).map(|(p, _, _)| p.clone()).collect();
    out.push_str("\n== Instruction files ==\n");
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for source in &request.sources {
        match source.as_str() {
            "claude" => {
                files.insert(env_dir("CLAUDE_CONFIG_DIR", ".claude").join("CLAUDE.md"));
                for repo in &top_repos {
                    files.insert(repo.join("CLAUDE.md"));
                }
            }
            "codex" => {
                files.insert(env_dir("CODEX_HOME", ".codex").join("AGENTS.md"));
                for repo in &top_repos {
                    files.insert(repo.join("AGENTS.md"));
                }
            }
            "omp" => {
                files.insert(crate::omp::config_source().join("AGENTS.md"));
            }
            "cursor" => {
                for repo in &top_repos {
                    files.insert(repo.join(".cursorrules"));
                }
            }
            _ => {}
        }
    }
    for file in &files {
        instruction(&mut out, file);
    }
    let servers = mcp_servers(&request.sources, &top_repos);
    if !servers.is_empty() {
        out.push_str("\n== MCP servers already configured (secret values replaced by ${secret:NAME}) ==\n");
        for (source, name, def) in servers {
            out.push_str(&format!("- {name} (from {source}): {def}\n"));
        }
    }
    if out.len() > MAX_DIGEST {
        let mut end = MAX_DIGEST;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[digest cut to size]\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_reads_each_cli_transcript_shape() {
        let claude = [
            r#"{"type":"user","cwd":"/work/api","message":{"role":"user","content":"fix the failing test in billing"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test -p billing"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the failing test in billing"},{"type":"tool_result","content":"ok"}]}}"#,
        ]
        .join("\n");
        let codex = [
            r#"{"type":"session_meta","payload":{"cwd":"/work/web"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"deploy with key sk-abcdefghijklmnopqrstuvwxyz0123"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"npm run build --prod\"]}"}}"#,
        ]
        .join("\n");
        let mut act = Activity::default();
        assert!(read_transcript(&claude, &mut act, &[]));
        assert!(read_transcript(&codex, &mut act, &[]));
        assert!(!read_transcript(&claude, &mut Activity::default(), &[PathBuf::from("/work")]));
        assert_eq!(act.sessions, 2);
        assert_eq!(act.prompts.len(), 2);
        assert_eq!(act.counts.get("fix the failing test in billing"), Some(&2));
        assert!(act.prompts[1].contains("[removed]") && !act.prompts[1].contains("sk-abc"));
        assert_eq!(act.commands.get("cargo test"), Some(&1));
        assert_eq!(act.commands.get("npm run build"), Some(&1));
        assert_eq!(act.folders.get("/work/api"), Some(&1));
    }

    #[test]
    fn digest_reads_codex_mcp_and_git_remotes() {
        let toml = "model = \"x\"\n[mcp_servers.linear]\nurl = \"https://mcp.linear.app/mcp\"\n[mcp_servers.linear.http_headers]\nAuthorization = \"Bearer abcdefghijklmnopqrstuvwxyz0123456789\"\n[mcp_servers.gh]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-github\"]\nenv = { GITHUB_TOKEN = \"ghp_abcdefghijklmnopqrstuvwxyz\" }\n";
        let servers = codex_servers(toml);
        let linear = redact_server("linear", &servers["linear"]).unwrap();
        assert_eq!(linear["headers"]["Authorization"], "${secret:LINEAR_AUTHORIZATION}");
        let gh = redact_server("gh", &servers["gh"]).unwrap();
        assert_eq!(gh["env"]["GITHUB_TOKEN"], "${secret:GITHUB_TOKEN}");
        assert_eq!(gh["args"][1], "@modelcontextprotocol/server-github");
        let config = "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:acme/api.git\n";
        assert_eq!(origin(config).as_deref(), Some("acme/api"));
    }
}
