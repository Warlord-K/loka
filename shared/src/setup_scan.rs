use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::runners::{RunnerStore, RunnerView};
use crate::starter_team::{SeatPick, StarterOptions, SEATS};
use crate::world::{AgentSession, NewEvent, NewMessage, SessionRun, WorldAgent, WorldStore, NOTES_MAX_BYTES};

pub const KIND: &str = "setup_scan";
pub const SOURCES: &[&str] = &["claude", "codex", "cursor", "omp"];
pub const DEFAULT_DAYS: u32 = 30;
pub const DAY_CHOICES: &[(u32, &str)] = &[(7, "Last 7 days"), (30, "Last 30 days"), (90, "Last 90 days")];
pub const LOOK: &str = "Look at my setup";
pub const NOT_NOW: &str = "Not now";
pub const CREATE: &str = "Create what I picked";
pub const MAX_OUTPUT: usize = 256 * 1024;
pub const MAX_STORED: usize = 48 * 1024;
pub const TIMEOUT_SECS: i32 = 900;
const TEMPLATE: &str = include_str!("../../agent/templates/world_setup_scan.txt");

const MAX_SEATS: usize = 5;
const MAX_SKILLS: usize = 5;
const MAX_RULES: usize = 6;
const MAX_MCP: usize = 8;
const MAX_REPOS: usize = 8;
const MAX_NOTES: usize = 6;
const REASON_CHARS: usize = 200;
const RULE_CHARS: usize = 600;
const NOTE_CHARS: usize = 1000;
const SKILL_BYTES: usize = 8 * 1024;
const LABEL_CHARS: usize = 400;
const SECRET_WORDS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "AUTH", "CREDENTIAL", "PAT", "COOKIE", "SESSION", "PRIVATE"];
const SECRET_PREFIXES: &[&str] = &[
    "sk-", "sk_live", "sk_test", "rk_live", "pk_live", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxp-", "xoxa-", "xapp-", "glpat-",
    "AKIA", "ASIA", "AIza", "ya29.", "eyJ", "-----BEGIN", "npm_", "pypi-", "hf_", "fal_", "wld_", "mv_", "shpat_", "sq0",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequest {
    pub sources: Vec<String>,
    pub days: u32,
}

impl ScanRequest {
    pub fn new(sources: &[String], days: u32) -> Option<ScanRequest> {
        let sources: Vec<String> = SOURCES.iter().filter(|s| sources.iter().any(|w| w == *s)).map(|s| s.to_string()).collect();
        (!sources.is_empty()).then(|| ScanRequest { sources, days: days.clamp(1, 365) })
    }

    pub fn names(&self) -> String {
        list(&self.sources.iter().map(|s| source_name(s).to_string()).collect::<Vec<_>>(), "and")
    }
}

pub fn source_name(source: &str) -> &'static str {
    match source {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "omp" => "omp",
        _ => "a CLI",
    }
}

pub fn requested(asked_by: Option<&Value>) -> Option<ScanRequest> {
    let asked_by = asked_by?;
    if asked_by.get("kind").and_then(Value::as_str) != Some(KIND) {
        return None;
    }
    let sources: Vec<String> = asked_by.get("sources")?.as_array()?.iter().filter_map(|s| s.as_str().map(str::to_string)).collect();
    let days = asked_by.get("days").and_then(Value::as_u64).unwrap_or(DEFAULT_DAYS as u64) as u32;
    ScanRequest::new(&sources, days)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Seat {
    #[serde(default)]
    pub id: String,
    pub seat: String,
    #[serde(default)]
    pub cli: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub body: String,
    #[serde(default = "main_agent")]
    pub agent: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default = "work_scope")]
    pub scope: String,
    pub text: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub definition: Value,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    #[serde(default)]
    pub id: String,
    pub repo: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    #[serde(default)]
    pub id: String,
    #[serde(default = "main_agent")]
    pub agent: String,
    pub text: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Suggestions {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub team: Vec<Seat>,
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub notes: Vec<Note>,
}

fn main_agent() -> String {
    "main".into()
}

fn work_scope() -> String {
    "work".into()
}

impl Suggestions {
    pub fn count(&self) -> usize {
        self.team.len() + self.skills.len() + self.rules.len() + self.mcp_servers.len() + self.repos.len() + self.notes.len()
    }
}

fn plain(text: &str, max_chars: usize) -> String {
    let text = text.replace(" \u{2014} ", " - ").replace('\u{2014}', " - ").replace(" \u{2013} ", " - ");
    let text = scrub(text.trim());
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out = out.trim_end().to_string();
        out.push_str("...");
    }
    out
}

fn one_line(text: &str, max_chars: usize) -> String {
    plain(&text.split_whitespace().collect::<Vec<_>>().join(" "), max_chars)
}

pub fn looks_secret(word: &str) -> bool {
    let w = word.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '`'));
    if w.len() < 8 {
        return false;
    }
    if SECRET_PREFIXES.iter().any(|p| w.starts_with(p) && w.len() >= p.len() + 8) {
        return true;
    }
    if w.contains("://") && w.split("://").nth(1).is_some_and(|rest| rest.split('/').next().is_some_and(|host| host.contains('@') && host.contains(':'))) {
        return true;
    }
    let body = w.rsplit('=').next().unwrap_or(w);
    let tokenish = body.len() >= 32
        && body.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
        && body.chars().any(|c| c.is_ascii_digit())
        && body.chars().any(|c| c.is_ascii_alphabetic())
        && !body.contains('/');
    tokenish
}

pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if looks_secret(word) {
            out.push_str("[removed]");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for ch in text.chars() {
        if ch.is_whitespace() {
            flush(&mut word, &mut out);
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn sensitive_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    SECRET_WORDS.iter().any(|w| upper.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == *w || part.ends_with(w) && part.len() > w.len() + 2 && *w != "PAT"))
}

fn ref_name(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut out = out.trim_matches('_').to_string();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "MCP_");
    }
    if crate::loka_env::reserved(&out) || !crate::environment::valid_name(&out) {
        out = format!("MCP_{out}");
    }
    out.chars().take(64).collect()
}

fn is_ref(text: &str) -> bool {
    text.strip_prefix("${secret:").and_then(|r| r.strip_suffix('}')).is_some_and(crate::environment::valid_name)
}

fn safe_name(name: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(max).collect::<String>().trim_matches('-').to_string()
}

fn strip_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let mut clean = parsed.clone();
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    clean.set_query(None);
    clean.set_fragment(None);
    let text = clean.to_string();
    (!text.split('/').any(looks_secret)).then_some(text)
}

pub fn redact_server(name: &str, definition: &Value) -> Option<Value> {
    let def = definition.as_object()?;
    let mut out = Map::new();
    let server = ref_name(name);
    if let Some(command) = def.get("command").and_then(Value::as_str) {
        let command = command.trim();
        if command.is_empty() || looks_secret(command) || command.contains("${") {
            return None;
        }
        out.insert("command".into(), json!(command));
        let args: Vec<String> = def
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect();
        let mut kept = Vec::new();
        let mut after_flag = false;
        for arg in args.iter().take(40) {
            if arg.contains("${secret:") || looks_secret(arg) || (after_flag && arg.len() >= 16) {
                return None;
            }
            if let Some((flag, value)) = arg.split_once('=') {
                if flag.starts_with('-') && sensitive_key(flag) && !value.is_empty() {
                    return None;
                }
            }
            after_flag = arg.starts_with('-') && !arg.contains('=') && sensitive_key(arg);
            kept.push(json!(arg.chars().take(300).collect::<String>()));
        }
        if !kept.is_empty() {
            out.insert("args".into(), Value::Array(kept));
        }
    } else {
        let url = def.get("url").or_else(|| def.get("serverUrl")).and_then(Value::as_str)?;
        out.insert("url".into(), json!(strip_url(url)?));
        if let Some(kind) = def.get("type").and_then(Value::as_str).filter(|t| matches!(*t, "http" | "sse" | "streamable-http")) {
            out.insert("type".into(), json!(kind));
        }
    }
    for field in ["env", "headers"] {
        let source = if field == "headers" { def.get("headers").or_else(|| def.get("http_headers")) } else { def.get("env") };
        let Some(values) = source.and_then(Value::as_object) else { continue };
        let mut clean = Map::new();
        for (key, value) in values.iter().take(30) {
            let key = key.trim();
            if key.is_empty() || key.len() > 80 || key.contains("${") {
                continue;
            }
            let text = value.as_str().map(str::trim).unwrap_or_default();
            let keep = !text.is_empty() && text.len() <= 16 && !looks_secret(text) && !sensitive_key(key) && !text.contains("${") && field == "env";
            let value = if is_ref(text) || keep {
                text.to_string()
            } else if field == "env" && crate::environment::valid_name(key) {
                format!("${{secret:{key}}}")
            } else {
                format!("${{secret:{}}}", ref_name(&format!("{server}_{key}")))
            };
            clean.insert(key.to_string(), json!(value));
        }
        if !clean.is_empty() {
            out.insert(field.into(), Value::Object(clean));
        }
    }
    let out = Value::Object(out);
    crate::environment::validate_mcp_definition(&out).ok()?;
    Some(out)
}

pub fn clean_repo(text: &str) -> Option<String> {
    let mut t = text.trim().trim_end_matches('/').trim_end_matches(".git").to_string();
    for prefix in ["https://", "http://", "ssh://", "git@", "www."] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.to_string();
        }
    }
    let t = t.replacen("github.com:", "github.com/", 1);
    let t = t.strip_prefix("github.com/").unwrap_or(&t);
    let parts: Vec<&str> = t.split('/').collect();
    let ok = |p: &str| !p.is_empty() && p.len() <= 100 && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (parts.len() == 2 && parts.iter().all(|p| ok(p))).then(|| format!("{}/{}", parts[0], parts[1]))
}

fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 80 && !model.starts_with('-') && !model.contains(char::is_whitespace)
}

fn seat_keys() -> Vec<&'static str> {
    SEATS.iter().map(|(k, _)| *k).collect()
}

fn agent_key(text: &str) -> String {
    let t = text.trim().to_lowercase();
    if seat_keys().contains(&t.as_str()) {
        t
    } else {
        "main".into()
    }
}

pub fn skill_markdown(name: &str, description: &str, body: &str) -> String {
    let (_, _, text) = crate::world::parse_skill_markdown(body);
    let rest = match body.trim_start().strip_prefix("---").and_then(|r| r.find("\n---").map(|end| r[end + 4..].to_string())) {
        Some(after) if !text.is_empty() => after.trim_start_matches(['-', '\n', '\r']).to_string(),
        _ => body.to_string(),
    };
    format!("---\nname: {name}\ndescription: {}\n---\n{}\n", description.replace('\n', " "), rest.trim())
}

pub fn check(raw: Suggestions) -> Suggestions {
    let mut out = Suggestions { summary: plain(&raw.summary, 400), ..Default::default() };
    let mut seen_seats = Vec::new();
    for seat in raw.team {
        let key = seat.seat.trim().to_lowercase();
        if !seat_keys().contains(&key.as_str()) || seen_seats.contains(&key) || out.team.len() >= MAX_SEATS {
            continue;
        }
        seen_seats.push(key.clone());
        out.team.push(Seat {
            id: format!("t{}", out.team.len() + 1),
            seat: key,
            cli: seat.cli.map(|c| c.trim().to_lowercase()).filter(|c| crate::backends::AGENT_CLIS.contains(&c.as_str())),
            model: seat.model.map(|m| m.trim().to_string()).filter(|m| valid_model(m)),
            repos: seat.repos.iter().filter_map(|r| clean_repo(r)).take(4).collect(),
            reason: one_line(&seat.reason, REASON_CHARS),
        });
    }
    let mut names = Vec::new();
    for skill in raw.skills {
        let name = safe_name(&skill.name, 40);
        let body = skill.body.trim();
        if name.is_empty() || name == "world" || name.ends_with("-notes") || names.contains(&name) || body.is_empty() || out.skills.len() >= MAX_SKILLS {
            continue;
        }
        let description = one_line(if skill.description.trim().is_empty() { &skill.reason } else { &skill.description }, 200);
        let markdown = skill_markdown(&name, &description, &scrub(body));
        if markdown.len() > SKILL_BYTES {
            continue;
        }
        names.push(name.clone());
        out.skills.push(Skill {
            id: format!("s{}", out.skills.len() + 1),
            name,
            description,
            body: markdown,
            agent: agent_key(&skill.agent),
            reason: one_line(&skill.reason, REASON_CHARS),
        });
    }
    for rule in raw.rules {
        let text = plain(&rule.text, RULE_CHARS);
        if text.is_empty() || out.rules.len() >= MAX_RULES || out.rules.iter().any(|r| r.text == text) {
            continue;
        }
        out.rules.push(Rule {
            id: format!("r{}", out.rules.len() + 1),
            scope: if rule.scope.trim() == "talk" { "talk".into() } else { "work".into() },
            text,
            source: one_line(&rule.source, 120),
            reason: one_line(&rule.reason, REASON_CHARS),
        });
    }
    let mut server_names = Vec::new();
    for server in raw.mcp_servers {
        let name = safe_name(&server.name, 40);
        if name.is_empty() || name == "world" || server_names.contains(&name) || out.mcp_servers.len() >= MAX_MCP {
            continue;
        }
        let Some(definition) = redact_server(&name, &server.definition) else { continue };
        server_names.push(name.clone());
        out.mcp_servers.push(McpServer {
            id: format!("m{}", out.mcp_servers.len() + 1),
            name,
            definition,
            source: one_line(&server.source, 60),
            reason: one_line(&server.reason, REASON_CHARS),
        });
    }
    for repo in raw.repos {
        let Some(name) = clean_repo(&repo.repo) else { continue };
        if out.repos.len() >= MAX_REPOS || out.repos.iter().any(|r| r.repo.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.repos.push(Repo { id: format!("p{}", out.repos.len() + 1), repo: name, reason: one_line(&repo.reason, REASON_CHARS) });
    }
    for note in raw.notes {
        let text = plain(&note.text, NOTE_CHARS);
        if text.is_empty() || out.notes.len() >= MAX_NOTES {
            continue;
        }
        out.notes.push(Note { id: format!("n{}", out.notes.len() + 1), agent: agent_key(&note.agent), text, reason: one_line(&note.reason, REASON_CHARS) });
    }
    while serde_json::to_vec(&out).map(|v| v.len()).unwrap_or(0) > MAX_STORED {
        if out.skills.pop().is_none() && out.notes.pop().is_none() && out.rules.pop().is_none() && out.mcp_servers.pop().is_none() {
            break;
        }
    }
    out
}

fn block_body<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<!-- {name}");
    let close = format!("{name} -->");
    let start = output.rfind(&open)?;
    let rest = &output[start + open.len()..];
    if let Some(end) = rest.find(&close) {
        return Some(rest[..end].trim());
    }
    rest.match_indices("-->").map(|(end, _)| rest[..end].trim()).find(|body| serde_json::from_str::<Value>(body).is_ok())
}

pub fn parse(output: &str) -> Result<Suggestions> {
    if output.len() > MAX_OUTPUT {
        bail!("the reply was too long");
    }
    let Some(body) = block_body(output, "setup") else { bail!("the reply had no setup block") };
    let raw: Suggestions = serde_json::from_str(body).map_err(|e| anyhow::anyhow!("the setup block was not valid JSON ({e})"))?;
    Ok(check(raw))
}

fn list(items: &[String], and: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {and} {last}", rest.join(", ")),
    }
}

fn seat_label(seat: &str) -> &'static str {
    SEATS.iter().find(|(k, _)| *k == seat).map(|(_, job)| job.label()).unwrap_or("agent")
}

fn agent_label(key: &str) -> String {
    if key == "main" {
        "your main agent".into()
    } else {
        format!("the {} seat", seat_label(key))
    }
}

fn label(text: String) -> String {
    text.chars().take(LABEL_CHARS).collect()
}

fn with_reason(head: String, reason: &str) -> String {
    label(if reason.is_empty() { head } else { format!("{head}. {reason}") })
}

pub fn item_labels(s: &Suggestions) -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        (
            "Team",
            s.team
                .iter()
                .map(|t| {
                    let on = match (&t.cli, &t.model) {
                        (Some(cli), Some(model)) => format!(" on {cli} {model}"),
                        (Some(cli), None) => format!(" on {cli}"),
                        _ => String::new(),
                    };
                    let repos = if t.repos.is_empty() { String::new() } else { format!(", works in {}", t.repos.join(", ")) };
                    (t.id.clone(), with_reason(format!("A {}{on}{repos}", seat_label(&t.seat)), &t.reason))
                })
                .collect(),
        ),
        (
            "Skills",
            s.skills.iter().map(|k| (k.id.clone(), with_reason(format!("Skill {} for {}: {}", k.name, agent_label(&k.agent), k.description), &k.reason))).collect(),
        ),
        (
            "Rules",
            s.rules
                .iter()
                .map(|r| {
                    let from = if r.source.is_empty() { String::new() } else { format!(" (from {})", r.source) };
                    (r.id.clone(), label(format!("{} rule{from}: {}", if r.scope == "talk" { "Chat" } else { "Work" }, r.text)))
                })
                .collect(),
        ),
        (
            "MCP servers",
            s.mcp_servers
                .iter()
                .map(|m| {
                    let keys: Vec<String> = crate::environment::secret_reference_names(&m.definition).unwrap_or_default().into_iter().collect();
                    let needs = if keys.is_empty() { String::new() } else { format!(", needs keys {}", keys.join(", ")) };
                    let from = if m.source.is_empty() { String::new() } else { format!(" from {}", m.source) };
                    (m.id.clone(), with_reason(format!("{}{from}{needs}", m.name), &m.reason))
                })
                .collect(),
        ),
        ("Repos", s.repos.iter().map(|r| (r.id.clone(), with_reason(r.repo.clone(), &r.reason))).collect()),
        ("Starting notes", s.notes.iter().map(|n| (n.id.clone(), label(format!("For {}: {}", agent_label(&n.agent), n.text)))).collect()),
    ]
}

pub fn suggestions_card(s: &Suggestions, request: &ScanRequest) -> Result<(String, Value)> {
    let mut elements = Map::new();
    let mut children = Vec::new();
    let mut picks = Map::new();
    let intro = if s.summary.is_empty() {
        "Tick what you want. Nothing is created until you press Create.".to_string()
    } else {
        format!("{} Tick what you want. Nothing is created until you press Create.", s.summary)
    };
    elements.insert("intro".into(), json!({ "type": "Text", "props": { "text": intro, "tone": "muted" } }));
    children.push("intro".to_string());
    for (index, (title, items)) in item_labels(s).into_iter().enumerate() {
        if items.is_empty() {
            continue;
        }
        let head = format!("h{index}");
        elements.insert(head.clone(), json!({ "type": "Heading", "props": { "text": title } }));
        children.push(head);
        for (id, text) in items {
            let key = format!("c_{id}");
            elements.insert(key.clone(), json!({ "type": "Checkbox", "props": { "label": text, "checked": { "$bindState": format!("/pick/{id}") } } }));
            picks.insert(id, json!(false));
            children.push(key);
        }
    }
    let mut buttons = Vec::new();
    if s.count() > 0 {
        elements.insert("create".into(), json!({ "type": "Button", "props": { "label": CREATE, "variant": "primary" }, "on": { "press": { "action": "submit", "params": { "label": CREATE } } } }));
        buttons.push("create");
    } else {
        elements.insert("empty".into(), json!({ "type": "Text", "props": { "text": "I did not find enough repeated work to suggest a setup. Try a longer time range later." } }));
        children.push("empty".to_string());
    }
    elements.insert("skip".into(), json!({ "type": "Button", "props": { "label": NOT_NOW, "variant": "secondary" }, "on": { "press": { "action": "reply", "params": { "text": NOT_NOW } } } }));
    buttons.push("skip");
    elements.insert("buttons".into(), json!({ "type": "Stack", "props": { "direction": "row", "gap": "sm" }, "children": buttons }));
    children.push("buttons".to_string());
    let days = request.days;
    elements.insert(
        "card".into(),
        json!({ "type": "Card", "props": { "title": "Suggested setup", "subtitle": format!("From the last {days} days of {}", request.names()) }, "children": children }),
    );
    let spec = crate::ui_cards::normalize(&json!({ "root": "card", "elements": elements, "state": { "pick": picks } }))?;
    let body = if s.count() == 0 {
        format!("I looked at the last {days} days of {} and found nothing clear enough to suggest.", request.names())
    } else {
        let counts: Vec<String> = item_labels(s)
            .into_iter()
            .filter(|(_, items)| !items.is_empty())
            .map(|(title, items)| format!("{} {}", items.len(), title.to_lowercase()))
            .collect();
        format!("I looked at the last {days} days of {} and have suggestions: {}. Tick what you want on the card.", request.names(), list(&counts, "and"))
    };
    Ok((body, spec))
}

pub fn request_card(installed: &[String]) -> Result<(String, Value)> {
    let mut elements = Map::new();
    let mut children = vec!["intro".to_string()];
    elements.insert(
        "intro".into(),
        json!({ "type": "Text", "props": { "text": "I can read your recent sessions, instruction files and MCP settings on your Mac and suggest a team, skills, rules, MCP servers and repos for you. The reading happens on your Mac; only my suggestions come back, and nothing is created until you pick it.", "tone": "muted" } }),
    );
    let mut look = Map::new();
    for source in installed {
        let key = format!("l_{source}");
        elements.insert(key.clone(), json!({ "type": "Checkbox", "props": { "label": source_name(source), "checked": { "$bindState": format!("/look/{source}") } } }));
        look.insert(source.clone(), json!(true));
        children.push(key);
    }
    let options: Vec<&str> = DAY_CHOICES.iter().map(|(_, l)| *l).collect();
    let default = DAY_CHOICES.iter().find(|(d, _)| *d == DEFAULT_DAYS).map(|(_, l)| *l).unwrap_or("Last 30 days");
    elements.insert("days".into(), json!({ "type": "Select", "props": { "label": "Time range", "value": { "$bindState": "/days" }, "options": options } }));
    children.push("days".to_string());
    elements.insert("look".into(), json!({ "type": "Button", "props": { "label": LOOK, "variant": "primary" }, "on": { "press": { "action": "submit", "params": { "label": LOOK } } } }));
    elements.insert("skip".into(), json!({ "type": "Button", "props": { "label": NOT_NOW, "variant": "secondary" }, "on": { "press": { "action": "reply", "params": { "text": NOT_NOW } } } }));
    elements.insert("buttons".into(), json!({ "type": "Stack", "props": { "direction": "row", "gap": "sm" }, "children": ["look", "skip"] }));
    children.push("buttons".to_string());
    elements.insert("card".into(), json!({ "type": "Card", "props": { "title": "Suggest a setup from how you already work?" }, "children": children }));
    let spec = crate::ui_cards::normalize(&json!({ "root": "card", "elements": elements, "state": { "look": look, "days": default } }))?;
    let names: Vec<String> = installed.iter().map(|s| source_name(s).to_string()).collect();
    let body = format!(
        "Want me to look at how you already use {} and suggest a setup? Tick what I may read and pick a time range.",
        list(&names, "and")
    );
    Ok((body, spec))
}

fn answer_lines(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn is_decline(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    t == NOT_NOW.to_lowercase() || ["no", "skip", "no thanks", "nope", "later", "cancel"].contains(&t.as_str())
}

pub fn parse_request(text: &str, installed: &[String]) -> Option<ScanRequest> {
    if is_decline(text) {
        return None;
    }
    let lines = answer_lines(text);
    let sources: Vec<String> = installed.iter().filter(|s| lines.get(&format!("look.{s}")).map(String::as_str) == Some("true")).cloned().collect();
    let days = lines
        .get("days")
        .and_then(|d| DAY_CHOICES.iter().find(|(_, l)| l.eq_ignore_ascii_case(d)).map(|(n, _)| *n))
        .unwrap_or(DEFAULT_DAYS);
    ScanRequest::new(&sources, days)
}

pub fn picked_ids(text: &str) -> Vec<String> {
    answer_lines(text)
        .into_iter()
        .filter(|(k, v)| k.starts_with("pick.") && v == "true")
        .map(|(k, _)| k.trim_start_matches("pick.").to_string())
        .collect()
}

pub fn render_prompt(request: &ScanRequest, clis: &[crate::starter_team::CliModels]) -> String {
    let models = clis.iter().map(|c| format!("- {}: {}", c.cli, c.models.join(", "))).collect::<Vec<_>>().join("\n");
    let seats = SEATS.iter().map(|(k, job)| format!("- {k}: {}", job.label())).collect::<Vec<_>>().join("\n");
    TEMPLATE
        .replace("{sources}", &request.names())
        .replace("{days}", &request.days.to_string())
        .replace("{models}", if models.is_empty() { "- claude: sonnet" } else { &models })
        .replace("{seats}", &seats)
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub runner: RunnerView,
    pub sources: Vec<String>,
    pub backend: String,
    pub model: String,
}

pub fn device_from(runner: Option<RunnerView>, owner: Uuid, main: &WorldAgent) -> std::result::Result<Device, String> {
    let Some(runner) = runner else {
        return Err("Connect your Mac first: pair it in Settings, under Devices, then ask me to look at your setup.".into());
    };
    let mac = runner.platform == "macos" || runner.capabilities.get("platform").and_then(Value::as_str) == Some("macos");
    if runner.owner_user_id != owner || !mac {
        return Err("I only read your setup on your own Mac, and I do not live on it yet. Move me to your Mac under Devices, then ask again.".into());
    }
    if runner.status != "online" {
        return Err(format!("Your Mac {} is offline. Open the Loka app on it, then ask me again.", runner.name));
    }
    if runner.capabilities.get("setup_scan").and_then(Value::as_bool) != Some(true) {
        return Err("Your Mac runs an older Loka app. Update it, then ask me again.".into());
    }
    let has = |cli: &str| runner.capabilities.get("backends").and_then(|b| b.get(cli)).and_then(Value::as_bool) == Some(true);
    let (backend, model) = if has("claude") {
        (crate::world::GUEST_BACKEND.to_string(), crate::world::GUEST_MODEL.to_string())
    } else if has("omp") && main.cli == "omp" {
        ("omp".to_string(), main.model.clone())
    } else {
        return Err("Reading your setup needs Claude Code on your Mac. Install it, then ask me again.".into());
    };
    let sources: Vec<String> = SOURCES.iter().filter(|s| has(s)).map(|s| s.to_string()).collect();
    if sources.is_empty() {
        return Err("None of Claude Code, Codex, Cursor or omp is installed on your Mac yet.".into());
    }
    Ok(Device { runner, sources, backend, model })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardKind {
    Request,
    Suggestions,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Applied {
    pub created: Vec<String>,
    pub problems: Vec<String>,
    pub team: bool,
}

impl Applied {
    pub fn message(&self) -> String {
        let mut lines = Vec::new();
        if self.created.is_empty() && self.problems.is_empty() {
            return "Nothing was picked, so I created nothing. Ask me to look at your setup again any time.".into();
        }
        if !self.created.is_empty() {
            lines.push(format!("Done. I created {}.", list(&self.created, "and")));
        }
        for p in &self.problems {
            lines.push(p.clone());
        }
        lines.push("Everything can be changed or removed later in Kit, Rules and Residents.".into());
        lines.join("\n")
    }
}

impl WorldStore {
    pub async fn setup_scan_device(&self, owner: Uuid, main: &WorldAgent) -> Result<std::result::Result<Device, String>> {
        let runner = RunnerStore::new(self.pool().clone()).agent_runner(owner, main.id).await?;
        Ok(device_from(runner, owner, main))
    }

    pub async fn setup_card_kind(&self, agent_id: Uuid, card: Uuid) -> Result<Option<CardKind>> {
        let row: Option<(bool, bool)> = sqlx::query_as("SELECT meta ? 'setup_request', meta ? 'setup' FROM agent_messages WHERE id = $1 AND agent_id = $2")
            .bind(card)
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(match row {
            Some((true, _)) => Some(CardKind::Request),
            Some((_, true)) => Some(CardKind::Suggestions),
            _ => None,
        })
    }

    async fn say_setup(&self, agent: &WorldAgent, body: &str, meta: Value, session: Option<Uuid>) -> Result<Uuid> {
        let msg = self
            .insert_message(NewMessage {
                agent_id: agent.id,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent.id),
                body,
                session_id: session,
                run_id: None,
                meta,
            })
            .await?;
        let mut event = NewEvent::new("message.created")
            .agent(agent.id)
            .payload(json!({ "message_id": msg.id, "author": agent.name, "author_kind": "agent", "body": body }));
        if let Some(session) = session {
            event = event.session(session);
        }
        self.emit(event, None).await?;
        Ok(msg.id)
    }

    pub async fn post_setup_note(&self, agent: &WorldAgent, body: &str) -> Result<Uuid> {
        self.say_setup(agent, body, json!({}), None).await
    }

    pub async fn post_setup_request(&self, owner: Uuid, main: &WorldAgent, onboarding: bool) -> Result<std::result::Result<Uuid, String>> {
        let device = match self.setup_scan_device(owner, main).await? {
            Ok(device) => device,
            Err(why) => return Ok(Err(why)),
        };
        let (body, spec) = request_card(&device.sources)?;
        let mut meta = json!({ "kind": "ui", "ui": spec, "setup_request": { "sources": device.sources } });
        if onboarding {
            meta["onboarding"] = json!("scan");
        }
        Ok(Ok(self.say_setup(main, &body, meta, None).await?))
    }

    pub async fn start_setup_scan(&self, owner: Uuid, main: &WorldAgent, request: &ScanRequest) -> Result<std::result::Result<SessionRun, String>> {
        if main.owner_user_id != Some(owner) {
            bail!("only the owner of this agent can ask it to read their setup");
        }
        let device = match self.setup_scan_device(owner, main).await? {
            Ok(device) => device,
            Err(why) => return Ok(Err(why)),
        };
        if let Some(why) = self.spend_blocker(main.id).await? {
            return Ok(Err(format!("I cannot look right now: {why}.")));
        }
        let sources: Vec<String> = request.sources.iter().filter(|s| device.sources.contains(s)).cloned().collect();
        let Some(request) = ScanRequest::new(&sources, request.days) else {
            return Ok(Err("None of the CLIs you picked is installed on your Mac.".into()));
        };
        let busy = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_sessions s JOIN session_runs r ON r.session_id = s.id
               WHERE s.agent_id = $1 AND s.role = 'aside' AND s.asked_by->>'kind' = $2 AND r.status IN ('queued', 'running'))",
        )
        .bind(main.id)
        .bind(KIND)
        .fetch_one(self.pool())
        .await?;
        if busy {
            return Ok(Err("I am already looking at your setup; my suggestions will show up here soon.".into()));
        }
        let plan = self.starter_plan(owner, main.org_id).await?;
        let prompt = render_prompt(&request, &plan.clis);
        let parent = self.main_session(main.id).await?.map(|s| s.id);
        let asked_by = json!({ "kind": KIND, "lean": true, "user_id": owner, "sources": request.sources, "days": request.days });
        let mut tx = self.pool().begin().await?;
        let session = sqlx::query_as::<_, AgentSession>(
            "INSERT INTO agent_sessions (agent_id, role, backend, model, parent_session_id, working_dir, usage_baseline, asked_by)
             VALUES ($1, 'aside', $2, $3, $4, $5, '{}'::jsonb, $6) RETURNING *",
        )
        .bind(main.id)
        .bind(&device.backend)
        .bind(&device.model)
        .bind(parent)
        .bind(format!("agents/{}/setup-scan", main.id))
        .bind(asked_by)
        .fetch_one(&mut *tx)
        .await?;
        let run = sqlx::query_as::<_, SessionRun>(
            "INSERT INTO session_runs (session_id, agent_id, kind, status, prompt, timeout_secs)
             VALUES ($1, $2, 'aside', 'queued', $3, $4)
             RETURNING id, session_id, agent_id, task_id, kind, status, fork_from, prompt, output, error,
                       timeout_secs, cost_usd, input_tokens, cached_input_tokens, output_tokens,
                       created_at, started_at, finished_at",
        )
        .bind(session.id)
        .bind(main.id)
        .bind(prompt)
        .bind(TIMEOUT_SECS)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(run))
    }

    pub async fn deliver_setup_scan(&self, agent: &WorldAgent, session: &AgentSession, answer: &str, ok: bool) -> Result<()> {
        let delivered = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM agent_messages WHERE agent_id = $1 AND session_id = $2 AND author_kind = 'agent')")
            .bind(agent.id)
            .bind(session.id)
            .fetch_one(self.pool())
            .await?;
        if delivered {
            return Ok(());
        }
        let request = requested(session.asked_by.as_ref()).unwrap_or(ScanRequest { sources: vec!["claude".into()], days: DEFAULT_DAYS });
        if !ok {
            let why: String = answer.trim().chars().take(400).collect();
            let body = format!("I could not finish reading your setup: {why} Ask me to look again any time.");
            self.say_setup(agent, &body, json!({}), Some(session.id)).await?;
            return Ok(());
        }
        match parse(answer).and_then(|s| suggestions_card(&s, &request).map(|card| (s, card))) {
            Ok((suggestions, (body, spec))) => {
                let meta = json!({ "kind": "ui", "ui": spec, "setup": suggestions, "setup_scan": request });
                self.say_setup(agent, &body, meta, Some(session.id)).await?;
            }
            Err(e) => {
                let body = format!("I read your setup but could not put my suggestions together ({e}). Ask me to look again.");
                self.say_setup(agent, &body, json!({}), Some(session.id)).await?;
            }
        }
        Ok(())
    }

    pub async fn take_setup_card(&self, agent_id: Uuid, card: Uuid) -> Result<Option<Suggestions>> {
        let raw: Option<Value> = sqlx::query_scalar(
            "UPDATE agent_messages SET meta = meta || jsonb_build_object('setup_applied', NOW())
             WHERE id = $1 AND agent_id = $2 AND meta ? 'setup' AND NOT meta ? 'setup_applied' RETURNING meta->'setup'",
        )
        .bind(card)
        .bind(agent_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(raw.and_then(|v| serde_json::from_value::<Suggestions>(v).ok()).map(check))
    }

    pub async fn take_setup_request(&self, agent_id: Uuid, card: Uuid) -> Result<Option<Vec<String>>> {
        let raw: Option<Value> = sqlx::query_scalar(
            "UPDATE agent_messages SET meta = meta || jsonb_build_object('setup_answered', NOW())
             WHERE id = $1 AND agent_id = $2 AND meta ? 'setup_request' AND NOT meta ? 'setup_answered' RETURNING meta->'setup_request'->'sources'",
        )
        .bind(card)
        .bind(agent_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(raw.map(|v| v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect()))
    }

    async fn free_kit_name(&self, table: &str, base: &str) -> Result<String> {
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE name = $1)");
        let mut name = base.to_string();
        let mut n = 2;
        while sqlx::query_scalar::<_, bool>(&sql).bind(&name).fetch_one(self.pool()).await? {
            name = format!("{base}-{n}");
            n += 1;
            if n > 50 {
                bail!("no free name for {base}");
            }
        }
        Ok(name)
    }

    async fn add_starting_notes(&self, agent: &WorldAgent, owner: Uuid, lines: &[String]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut section = String::from("\n## Starting notes\n");
        for line in lines {
            let entry = format!("- {}\n", line.replace('\n', " "));
            if section.len() + entry.len() > NOTES_MAX_BYTES / 2 {
                break;
            }
            section.push_str(&entry);
        }
        match self.own_notes_skill(agent.id, Some(owner)).await? {
            Some(skill) => {
                let mut body = skill.body.trim_end().to_string();
                body.push('\n');
                body.push_str(&section);
                if body.len() > NOTES_MAX_BYTES {
                    bail!("{}'s notes are full", agent.name);
                }
                let description = crate::world::check_notes(&skill.name, &body)?;
                self.save_own_notes(agent.id, Some(owner), Some(skill.id), &skill.name, &description, &body).await?;
            }
            None => {
                let name = format!("{}-notes", agent.slug);
                let description = format!("{}'s working notes, read before every job: role, people, open threads and lessons.", agent.name);
                let body = format!("---\nname: {name}\ndescription: {description}\n---\n# {}'s notes\n{section}", agent.name);
                let description = crate::world::check_notes(&name, &body)?;
                self.save_own_notes(agent.id, Some(owner), None, &name, &description, &body).await?;
            }
        }
        Ok(())
    }

    pub async fn apply_setup(&self, owner: Uuid, main: &WorldAgent, s: &Suggestions, picks: &[String]) -> Result<Applied> {
        let picked = |id: &str| picks.iter().any(|p| p == id);
        let org = main.org_id;
        let mut applied = Applied::default();
        let mut agents: BTreeMap<String, WorldAgent> = BTreeMap::new();
        agents.insert("main".into(), main.clone());
        let seats: Vec<&Seat> = s.team.iter().filter(|t| picked(&t.id)).collect();
        if !seats.is_empty() {
            let options = StarterOptions {
                team: false,
                seats: Some(seats.iter().map(|t| SeatPick { seat: t.seat.clone(), cli: t.cli.clone(), model: t.model.clone() }).collect()),
            };
            match self.build_starter_team(owner, org, &options).await {
                Ok(built) => {
                    applied.team = !built.created.is_empty();
                    if !built.created.is_empty() {
                        applied.created.push(format!("team members {}", list(&built.created, "and")));
                    }
                    for seat in built.plan.seats.iter().filter(|s| s.exists) {
                        if let Some(agent) = match seat.agent_id {
                            Some(id) => self.get_agent(id).await?,
                            None => None,
                        } {
                            agents.insert(seat.seat.to_string(), agent);
                        }
                    }
                }
                Err(e) => applied.problems.push(format!("The team was not created: {e}.")),
            }
        }
        let target = |key: &str| agents.get(key).cloned().unwrap_or_else(|| main.clone());
        for skill in s.skills.iter().filter(|k| picked(&k.id)) {
            let agent = target(&skill.agent);
            let result = async {
                let name = self.free_kit_name("world_skills", &skill.name).await?;
                let body = skill_markdown(&name, &skill.description, &skill.body);
                let entry = self.upsert_skill(&name, &skill.description, &body, None, "agent", Some(owner), org).await?;
                self.attach_skill(agent.id, entry.id, true).await?;
                Ok::<_, anyhow::Error>(name)
            }
            .await;
            match result {
                Ok(name) => applied.created.push(format!("skill {name} for {}", agent.name)),
                Err(e) => applied.problems.push(format!("Skill {} was not added: {e}.", skill.name)),
            }
        }
        for rule in s.rules.iter().filter(|r| picked(&r.id)) {
            let made = self.create_rule(Some(owner), None, &rule.scope, &rule.text).await?;
            sqlx::query("UPDATE world_rules SET org_id = $2 WHERE id = $1").bind(made.id).bind(org).execute(self.pool()).await?;
            applied.created.push(format!("a {} rule", if rule.scope == "talk" { "chat" } else { "work" }));
        }
        let granted = self.environment_names_for_agent(main.id).await.unwrap_or_default();
        for server in s.mcp_servers.iter().filter(|m| picked(&m.id)) {
            let result = async {
                let name = self.free_kit_name("world_mcp_servers", &server.name).await?;
                let entry = self.upsert_mcp_server(&name, &server.definition, "agent", Some(owner), org).await?;
                let missing: Vec<String> = crate::environment::secret_reference_names(&server.definition)?
                    .into_iter()
                    .filter(|k| !granted.contains(k))
                    .collect();
                if missing.is_empty() {
                    self.attach_mcp_server(main.id, entry.id, true).await?;
                }
                Ok::<_, anyhow::Error>((name, missing))
            }
            .await;
            match result {
                Ok((name, missing)) if missing.is_empty() => applied.created.push(format!("MCP server {name} for {}", main.name)),
                Ok((name, missing)) => {
                    applied.created.push(format!("MCP server {name}"));
                    applied.problems.push(format!(
                        "{name} needs the keys {}. Save them under Settings > Keys, grant them to {}, then equip {name} in their Kit.",
                        missing.join(", "),
                        main.name
                    ));
                }
                Err(e) => applied.problems.push(format!("MCP server {} was not added: {e}.", server.name)),
            }
        }
        let mut notes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let repos: Vec<&Repo> = s.repos.iter().filter(|r| picked(&r.id)).collect();
        if !repos.is_empty() {
            notes.entry("main".into()).or_default().push(format!(
                "Repos my person works in: {}.",
                repos.iter().map(|r| if r.reason.is_empty() { r.repo.clone() } else { format!("{} ({})", r.repo, r.reason) }).collect::<Vec<_>>().join("; ")
            ));
            applied.created.push(format!("{} repo{} in {}'s notes", repos.len(), if repos.len() == 1 { "" } else { "s" }, main.name));
        }
        for seat in seats.iter().filter(|t| !t.repos.is_empty()) {
            notes.entry(seat.seat.clone()).or_default().push(format!("Repos to work in: {}.", seat.repos.join(", ")));
        }
        for note in s.notes.iter().filter(|n| picked(&n.id)) {
            notes.entry(note.agent.clone()).or_default().push(note.text.clone());
        }
        let picked_notes = s.notes.iter().filter(|n| picked(&n.id)).count();
        for (key, lines) in notes {
            let agent = target(&key);
            if let Err(e) = self.add_starting_notes(&agent, owner, &lines).await {
                applied.problems.push(format!("Some starting notes for {} were not saved: {e}.", agent.name));
            }
        }
        if picked_notes > 0 {
            applied.created.push(format!("{picked_notes} starting note{}", if picked_notes == 1 { "" } else { "s" }));
        }
        Ok(applied)
    }

    pub async fn post_setup_applied(&self, main: &WorldAgent, applied: &Applied) -> Result<Uuid> {
        self.say_setup(main, &applied.message(), json!({}), None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let block = json!({
            "summary": "You mostly ship Rust and review PRs.",
            "team": [
                { "seat": "coder-1", "cli": "claude", "model": "opus", "repos": ["https://github.com/acme/api.git", "bad repo"], "reason": "Most sessions write code \u{2014} daily." },
                { "seat": "coder-1", "cli": "claude" },
                { "seat": "designer", "cli": "claude" },
                { "seat": "reviewer", "cli": "codex", "model": "--yolo" }
            ],
            "skills": [
                { "name": "Release Steps!", "description": "Draft release notes", "body": "1. Read the merged PRs\n2. Group them", "reason": "You asked for this 6 times", "agent": "coder-1" },
                { "name": "x-notes", "body": "nope" },
                { "name": "empty", "body": "  " }
            ],
            "rules": [ { "scope": "talk", "text": "Answer in short bullet points.", "source": "~/.claude/CLAUDE.md" }, { "text": "Use token sk-abcdefghijklmnopqrstuvwxyz123456 to deploy" } ],
            "mcp_servers": [
                { "name": "Linear", "definition": { "url": "https://mcp.linear.app/sse?token=abc", "headers": { "Authorization": "Bearer abcdefghijklmnopqrstuvwxyz0123456789" } }, "source": "claude" },
                { "name": "github", "definition": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_abcdefghijklmnopqrstuvwxyz0123", "DEBUG": "1" } } },
                { "name": "leaky", "definition": { "command": "server", "args": ["--api-key", "abcdefghijklmnopqrstuvwxyz0123456789"] } },
                { "name": "world", "definition": { "command": "x" } }
            ],
            "repos": [ { "repo": "git@github.com:acme/api.git", "reason": "30 sessions" }, { "repo": "acme/api" }, { "repo": "../etc" } ],
            "notes": [ { "agent": "reviewer", "text": "Run cargo clippy before approving." } ]
        });
        format!("Here you go.\n<!-- setup {block} setup -->")
    }

    #[test]
    fn setup_scan_parses_and_validates() {
        let s = parse(&sample()).unwrap();
        assert_eq!(s.team.len(), 2);
        assert_eq!(s.team[0].id, "t1");
        assert_eq!(s.team[0].repos, vec!["acme/api".to_string()]);
        assert!(!s.team[0].reason.contains('\u{2014}'));
        assert_eq!(s.team[1].seat, "reviewer");
        assert_eq!(s.team[1].model, None);
        assert_eq!(s.skills.len(), 1);
        assert_eq!(s.skills[0].name, "release-steps");
        assert_eq!(s.skills[0].agent, "coder-1");
        assert!(s.skills[0].body.starts_with("---\nname: release-steps\ndescription: Draft release notes\n---\n1. Read"));
        assert_eq!(s.rules.len(), 2);
        assert_eq!(s.rules[0].scope, "talk");
        assert!(s.rules[1].text.contains("[removed]") && !s.rules[1].text.contains("sk-abc"));
        let names: Vec<&str> = s.mcp_servers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["linear", "github"]);
        assert_eq!(s.mcp_servers[0].definition["url"], "https://mcp.linear.app/sse");
        assert_eq!(s.mcp_servers[0].definition["headers"]["Authorization"], "${secret:LINEAR_AUTHORIZATION}");
        assert_eq!(s.mcp_servers[1].definition["env"]["GITHUB_PERSONAL_ACCESS_TOKEN"], "${secret:GITHUB_PERSONAL_ACCESS_TOKEN}");
        assert_eq!(s.mcp_servers[1].definition["env"]["DEBUG"], "1");
        assert!(!serde_json::to_string(&s).unwrap().contains("ghp_abc"));
        assert_eq!(s.repos.len(), 1);
        assert_eq!(s.repos[0].repo, "acme/api");
        assert_eq!(s.notes[0].agent, "reviewer");
    }

    #[test]
    fn setup_scan_refuses_broken_replies() {
        assert!(parse("no block here").is_err());
        assert!(parse("<!-- setup {not json} setup -->").is_err());
        let bare = parse("<!-- setup {\"summary\": \"x\"} -->").unwrap();
        assert_eq!(bare.count(), 0);
        let mut many = Suggestions::default();
        for i in 0..20 {
            many.rules.push(Rule { text: format!("rule {i}"), scope: "work".into(), ..Default::default() });
            many.notes.push(Note { text: "n".repeat(5000), agent: "main".into(), ..Default::default() });
        }
        let checked = check(many);
        assert_eq!(checked.rules.len(), MAX_RULES);
        assert_eq!(checked.notes.len(), MAX_NOTES);
        assert!(checked.notes.iter().all(|n| n.text.chars().count() <= NOTE_CHARS + 3));
        assert!(serde_json::to_vec(&checked).unwrap().len() <= MAX_STORED);
    }

    #[test]
    fn setup_cards_round_trip_answers() {
        let s = parse(&sample()).unwrap();
        let request = ScanRequest::new(&["codex".into(), "claude".into(), "vim".into()], 30).unwrap();
        assert_eq!(request.sources, vec!["claude".to_string(), "codex".to_string()]);
        let (body, spec) = suggestions_card(&s, &request).unwrap();
        assert!(body.contains("Claude Code and Codex"));
        assert_eq!(spec["state"]["pick"]["t1"], false);
        assert!(serde_json::to_vec(&spec).unwrap().len() <= crate::ui_cards::MAX_BYTES);
        let answer = format!("{CREATE}:\n- pick.t1: true\n- pick.s1: false\n- pick.m2: true");
        let mut ids = picked_ids(&answer);
        ids.sort();
        assert_eq!(ids, vec!["m2".to_string(), "t1".to_string()]);

        let installed = vec!["claude".to_string(), "cursor".to_string()];
        let (_, request_spec) = request_card(&installed).unwrap();
        assert_eq!(request_spec["state"]["days"], "Last 30 days");
        let req = parse_request(&format!("{LOOK}:\n- look.claude: true\n- look.cursor: false\n- days: Last 90 days"), &installed).unwrap();
        assert_eq!(req, ScanRequest { sources: vec!["claude".into()], days: 90 });
        assert!(parse_request(NOT_NOW, &installed).is_none());
        assert!(parse_request(&format!("{LOOK}:\n- look.claude: false\n- days: Last 7 days"), &installed).is_none());
    }

    #[test]
    fn setup_scan_redacts_servers() {
        assert!(looks_secret("ghp_abcdefghijklmnopqrstu"));
        assert!(looks_secret("AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"));
        assert!(!looks_secret("@modelcontextprotocol/server-github"));
        assert!(!looks_secret("/Users/me/vscode/project-with-a-long-name-2024"));
        let server = redact_server("pg", &json!({ "command": "psql-mcp", "args": ["postgres://user:hunter2@db:5432/app"] }));
        assert!(server.is_none());
        let remote = redact_server("x", &json!({ "url": "https://user:pw@mcp.example.com/mcp?key=1", "type": "http" })).unwrap();
        assert_eq!(remote["url"], "https://mcp.example.com/mcp");
        assert!(redact_server("x", &json!({ "url": "file:///etc/passwd" })).is_none());
        let rendered = render_prompt(&ScanRequest { sources: vec!["claude".into()], days: 7 }, &[]);
        assert!(rendered.contains("Claude Code") && rendered.contains("coder-1") && !rendered.contains("{days}"));
        assert!(!rendered.contains('\u{2014}'));
    }

    async fn person(pool: &sqlx::PgPool) -> Result<(Uuid, Uuid)> {
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Sam') RETURNING id")
            .bind(format!("{}@setup.test", Uuid::new_v4()))
            .fetch_one(pool)
            .await?;
        let main: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, role) VALUES ($1, 'Nia', 'claude', 'sonnet', $2, 'assistant') RETURNING id",
        )
        .bind(format!("nia-{}", &Uuid::new_v4().simple().to_string()[..8]))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE users SET main_agent_id = $2 WHERE id = $1").bind(owner).bind(main).execute(pool).await?;
        crate::orgs::join_default_for_tests(pool, owner, Some(main)).await?;
        Ok((owner, main))
    }

    #[tokio::test]
    async fn setup_scan_runs_on_the_owners_mac_and_creates_only_picks() -> Result<()> {
        let Ok(url) = std::env::var("SETUP_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let (owner, main_id) = person(&pool).await?;
        let main = store.get_agent(main_id).await?.unwrap();
        assert!(store.setup_scan_device(owner, &main).await?.is_err());
        let request = ScanRequest::new(&["claude".into(), "codex".into(), "cursor".into()], 30).unwrap();
        assert!(store.start_setup_scan(owner, &main, &request).await?.is_err());

        let runner: Uuid = sqlx::query_scalar(
            "INSERT INTO world_runners (owner_user_id, name, platform, token_hash, status, last_seen_at, capabilities)
             VALUES ($1, 'Mac', 'macos', $2, 'online', NOW(), $3) RETURNING id",
        )
        .bind(owner)
        .bind([*Uuid::new_v4().as_bytes(), *Uuid::new_v4().as_bytes()].concat())
        .bind(json!({ "setup_scan": true, "platform": "macos", "backends": { "claude": true, "codex": true, "cursor": false } }))
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_profiles SET runner_id = $2 WHERE id = $1").bind(main_id).bind(runner).execute(&pool).await?;
        let main = store.get_agent(main_id).await?.unwrap();
        let device = store.setup_scan_device(owner, &main).await?.unwrap();
        assert_eq!(device.sources, vec!["claude".to_string(), "codex".to_string()]);
        let card = store.post_setup_request(owner, &main, false).await?.unwrap();
        assert_eq!(store.setup_card_kind(main_id, card).await?, Some(CardKind::Request));
        assert!(store.take_setup_request(main_id, card).await?.is_some());
        assert!(store.take_setup_request(main_id, card).await?.is_none());

        let run = store.start_setup_scan(owner, &main, &request).await?.unwrap();
        assert!(store.start_setup_scan(owner, &main, &request).await?.is_err());
        let session = store.get_session(run.session_id).await?.unwrap();
        assert_eq!(requested(session.asked_by.as_ref()).unwrap().sources, vec!["claude".to_string(), "codex".to_string()]);
        assert!(crate::world::toolless_aside(session.asked_by.as_ref()));
        let pinned: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM agent_sessions WHERE id = $1").bind(session.id).fetch_one(&pool).await?;
        assert_eq!(pinned, Some(runner));
        assert!(run.prompt.contains("Claude Code and Codex"));

        assert!(store.deliver_loka_answer(&main, &session, &sample(), true).await?);
        assert!(store.deliver_loka_answer(&main, &session, &sample(), true).await?);
        let cards: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM agent_messages WHERE agent_id = $1 AND session_id = $2").bind(main_id).bind(session.id).fetch_all(&pool).await?;
        assert_eq!(cards.len(), 1);
        assert_eq!(store.setup_card_kind(main_id, cards[0]).await?, Some(CardKind::Suggestions));
        let suggestions = store.take_setup_card(main_id, cards[0]).await?.unwrap();
        assert!(store.take_setup_card(main_id, cards[0]).await?.is_none());

        let picks: Vec<String> = ["t1", "s1", "r1", "m2", "p1", "n1"].iter().map(|s| s.to_string()).collect();
        let applied = store.apply_setup(owner, &main, &suggestions, &picks).await?;
        assert!(applied.team, "{applied:?}");
        assert!(applied.problems.iter().any(|p| p.contains("GITHUB_PERSONAL_ACCESS_TOKEN")), "{applied:?}");
        let agents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE owner_user_id = $1").bind(owner).fetch_one(&pool).await?;
        assert_eq!(agents, 2);
        let coder: (Uuid, String, String) = sqlx::query_as(
            "SELECT a.id, a.cli, a.model FROM starter_team_seats s JOIN agent_profiles a ON a.id = s.agent_id WHERE s.owner_user_id = $1 AND s.seat = 'coder-1'",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        assert_eq!((coder.1.as_str(), coder.2.as_str()), ("claude", "opus"));
        let skill_holder: Uuid = sqlx::query_scalar(
            "SELECT k.agent_id FROM agent_skills k JOIN world_skills s ON s.id = k.skill_id WHERE s.name LIKE 'release-steps%' AND s.created_by = $1",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        assert_eq!(skill_holder, coder.0);
        let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_rules WHERE user_id = $1 AND body = 'Answer in short bullet points.' AND org_id IS NOT NULL")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        assert_eq!(rules, 1);
        let unattached: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS (SELECT 1 FROM agent_mcp_servers a WHERE a.server_id = m.id) FROM world_mcp_servers m WHERE m.created_by = $1 AND m.name LIKE 'github%'",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        assert!(unattached);
        let linear: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_mcp_servers WHERE created_by = $1 AND name LIKE 'linear%'").bind(owner).fetch_one(&pool).await?;
        assert_eq!(linear, 0);
        let main_notes = store.own_notes_skill(main_id, Some(owner)).await?.unwrap();
        assert!(main_notes.body.contains("acme/api") && main_notes.body.contains("Run cargo clippy"), "{}", main_notes.body);
        let coder_notes = store.own_notes_skill(coder.0, Some(owner)).await?.unwrap();
        assert!(coder_notes.body.contains("Repos to work in: acme/api"), "{}", coder_notes.body);
        Ok(())
    }
}
